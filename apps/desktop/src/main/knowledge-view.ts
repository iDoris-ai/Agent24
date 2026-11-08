// KB-C2-03 (#1622): the KnowledgeView container. A WebContentsView, owned by
// main, that shows the WeKnora page in the window's content area (the same
// place as Design), isolated as ADR-OD-004 §4–§5 and CONTRACT-UI-BRIDGE-v1
// U4.1–U4.8 require:
//
// - every view instance gets its own non-persistent partition with a random
//   name (never derived from a profile, path, URL or principal), never reused;
//   at most 64 per process (U4.1, U4.6);
// - no Node, no preload, contextIsolation and sandbox on, no DevTools in a
//   packaged build (U4.2);
// - the partition alone carries the page's CSP and request filter: resources
//   and connections only from the allowed origin, no frames, workers, objects
//   or <base>; Agent24's own CSP is not relaxed (U4.2, ADR §5);
// - window.open, cross-origin navigation, file:// and custom schemes,
//   downloads, permissions, device pickers, HTTP auth and client certificates
//   are all refused; a Service Worker that registers is removed (U4.2, U4.3);
// - Off, a binding change, a new window and app quit destroy the instance and
//   clear its partition; hiding keeps it (U4.7 ④⑤, U4.8).
//
// This slice has no session and no bridge: the ticket/JWT injection is
// KB-C2-04a and the message bridge KB-C2-04b, which add U4.7 ①–③ in front of
// the destruction steps here. The page address comes from the Knowledge
// settings (#1625); a development-only variable can override it.
//
// Nothing here touches the Creative view: it has its own module, partitions,
// channels and teardown.

import { randomBytes } from 'node:crypto'
import net from 'node:net'
import type { BrowserWindow, IpcMain, IpcMainInvokeEvent, Session, WebContents, WebContentsView } from 'electron'
import type { IpcGuard } from './ipc/guard'
import { isPlainHostname } from './knowledge-hostname'
import type { InjectDecision, InjectRequest } from './knowledge-ui-inject'
import type { UiInstanceEvent } from './knowledge-ui-instance'
import { UiSessionLedger, UI_SESSION_WINDOW_MS } from './knowledge-ui-session'
import { KnowledgeBridge, type CitationShown } from './knowledge-bridge'
import { KnowledgeBridgeChannel, reservedRequest, type BridgeRequestDetails } from './knowledge-bridge-channel'
import type { ChunkRead, CitationRef } from './knowledge-readonly'
import { IpcChannels } from '../shared/ipc-types'
import type {
  KnowledgeViewBounds,
  KnowledgeViewClosedCode,
  KnowledgeViewResult,
  KnowledgeViewState,
  KnowledgeViewTheme,
} from '../shared/knowledge-view-types'

export type { KnowledgeViewBounds, KnowledgeViewResult, KnowledgeViewState, KnowledgeViewTheme }

export const KNOWLEDGE_VIEW_PARTITION_PREFIX = 'agent24-knowledge-v1:'
/** U4.6: in-memory partitions live until the process ends, so their number is capped. */
export const MAX_KNOWLEDGE_VIEW_PARTITIONS = 64
/** Development only (ignored in a packaged build) until #1625 wires the settings. */
export const KNOWLEDGE_VIEW_DEV_URL_ENV = 'AGENT24_KNOWLEDGE_VIEW_URL'
/** U4.8: more crashes than this within the window close the view (`crash_loop`). */
export const KNOWLEDGE_VIEW_MAX_CRASHES = 3
export const KNOWLEDGE_VIEW_CRASH_WINDOW_MS = 10 * 60_000
const CRASH_BACKOFF_MS = [1_000, 2_000, 4_000]
/** A first load that has not settled by then counts as failed, so Reload is offered. */
export const KNOWLEDGE_VIEW_FIRST_LOAD_TIMEOUT_MS = 30_000
/** How long cleanup waits for the page to be destroyed before clearing anyway. */
export const KNOWLEDGE_VIEW_DESTROY_WAIT_MS = 2_000

export interface KnowledgeViewTarget {
  /** The exact allowed origin (U4.4). */
  origin: string
  /** The page to load; same origin. */
  url: string
}

const LOOPBACK_HOST = /^127(\.\d{1,3}){3}$/

/** U4.4: `https`, or `http` to a loopback IP literal; no user info, query or
 * fragment (nothing secret ever rides in the page address, U2.5). */
export function knowledgeViewTarget(value: unknown): KnowledgeViewTarget | null {
  if (typeof value !== 'string' || value.length > 2048) return null
  let url: URL
  try {
    url = new URL(value)
  } catch {
    return null
  }
  const loopback = url.hostname === '[::1]' || LOOPBACK_HOST.test(url.hostname)
  const scheme = url.protocol === 'https:' || (url.protocol === 'http:' && loopback)
  if (!scheme || url.username !== '' || url.password !== '' || url.search !== '' || url.hash !== '') return null
  if (!isPlainHostname(url.hostname) || url.port === '0') return null
  return { origin: url.origin, url: url.href }
}

/** KB-C2-02 (WeKnora#134): the embed page is served from the API's own
 * origin, at its root (v1 has no sub-path deployment). */
export const KNOWLEDGE_VIEW_EMBED_PATH = '/embed-admin/knowledge-bases'

/** The page for the endpoint saved in the Knowledge settings: its origin plus
 * the fixed embed path. The allowed origin is the endpoint's origin (U4.4);
 * nothing else is stored or taken from the server. */
export function settingsKnowledgeViewTarget(endpoint: unknown): KnowledgeViewTarget | null {
  if (typeof endpoint !== 'string' || endpoint === '') return null
  let url: URL
  try {
    url = new URL(endpoint)
  } catch {
    return null
  }
  url.search = ''
  url.hash = ''
  url.pathname = '/'
  const api = knowledgeViewTarget(url.href)
  return api ? { origin: api.origin, url: api.origin + KNOWLEDGE_VIEW_EMBED_PATH } : null
}

/** The development variable wins outside a packaged build; otherwise the settings. */
export function resolveKnowledgeViewTarget(
  env: Record<string, string | undefined>,
  packaged: boolean,
  endpoint: unknown,
): KnowledgeViewTarget | null {
  return devKnowledgeViewTarget(env, packaged) ?? settingsKnowledgeViewTarget(endpoint)
}

/** The development address; a packaged build never reads it. */
export function devKnowledgeViewTarget(env: Record<string, string | undefined>, packaged: boolean): KnowledgeViewTarget | null {
  if (packaged) return null
  return knowledgeViewTarget(env[KNOWLEDGE_VIEW_DEV_URL_ENV])
}

function parse(url: string): URL | null {
  try {
    return new URL(url)
  } catch {
    return null
  }
}

/** A main-frame document may only be at the allowed origin. */
export function isAllowedDocument(url: string, origin: string): boolean {
  const parsed = parse(url)
  return parsed !== null && (parsed.protocol === 'https:' || parsed.protocol === 'http:') && parsed.origin === origin
}

/** Network filter for the partition: http(s) to the allowed origin and its
 * matching ws/wss; nothing else leaves (U4.2). */
export function isAllowedRequest(url: string, origin: string): boolean {
  const parsed = parse(url)
  if (!parsed) return false
  if (parsed.protocol === 'https:' || parsed.protocol === 'http:') return parsed.origin === origin
  if (parsed.protocol === 'wss:' || parsed.protocol === 'ws:') {
    const http = parsed.protocol === 'wss:' ? 'https:' : 'http:'
    return `${http}//${parsed.host}` === origin
  }
  return false
}

/** U4.2 CSP floor. `style-src 'unsafe-inline'` is the one relaxation the
 * WeKnora UI (component library inline styles) needs; decision #27 is to be
 * settled with KB-C2-02's page. */
export function knowledgeViewCsp(origin: string): string {
  // Chromium's CSP parser takes no bracketed IPv6 host: `script-src
  // http://[::1]:8443` matches nothing and the page's own scripts are blocked
  // (headless Chromium 153, 11-08; Codex #1654 R1). For such an origin the
  // sources are `'self'`: the main frame never leaves the allowed origin
  // (guardKnowledgeContents), so 'self' is that origin. In connect-src 'self'
  // also matches same-origin ws/wss (CSP3); v1 uses no WebSocket anyway.
  const ipv6 = new URL(origin).hostname.startsWith('[')
  const src = ipv6 ? "'self'" : origin
  const connect = ipv6 ? "'self'" : `${origin} ${origin.replace(/^http/, 'ws')}`
  return [
    "default-src 'none'",
    `script-src ${src}`,
    `style-src ${src} 'unsafe-inline'`,
    `img-src ${src} data: blob:`,
    `font-src ${src} data:`,
    `connect-src ${connect}`,
    `media-src ${src}`,
    `manifest-src ${src}`,
    `form-action ${src}`,
    "frame-src 'none'",
    "child-src 'none'",
    "worker-src 'none'",
    "object-src 'none'",
    "base-uri 'none'",
    "frame-ancestors 'none'",
  ].join('; ')
}

/** Powerful features switched off in the page itself (UI-BRIDGE U4.2). The
 * permission handlers already refuse them, but a request that never reaches
 * them (David's 11-08 Mac run: geolocation left pending, no answer) is still
 * left open; a feature the document is not allowed to use is refused inside
 * Blink at once (W3C Geolocation: PERMISSION_DENIED), whatever the OS does.
 * David's run had no location prompt at all: the request hung before the OS. */
export const KNOWLEDGE_VIEW_PERMISSIONS_POLICY = [
  'accelerometer', 'ambient-light-sensor', 'bluetooth', 'camera', 'clipboard-read', 'display-capture',
  'geolocation', 'gyroscope', 'hid', 'idle-detection', 'local-fonts', 'magnetometer', 'microphone', 'midi',
  'payment', 'publickey-credentials-create', 'publickey-credentials-get', 'screen-wake-lock', 'serial', 'usb',
  'window-management',
].map((feature) => `${feature}=()`).join(', ')

function serverSwitchOffs(values: string[]): string[] {
  // Each member is kept only when it is exactly `feature=()`; everything else
  // (a grant, or text that does not parse) is left out, so what goes out is
  // always well formed and can only switch more off.
  const kept = values.flatMap((v) => v.split(',')).map((m) => m.trim()).filter((m) => /^[a-z][a-z0-9-]*=\(\)$/.test(m))
  return kept.length > 0 ? [kept.join(', ')] : []
}

/** Adds the partition CSP next to whatever the server sent: browsers enforce
 * every policy present, so the server's own can only tighten it. */
export function withKnowledgeCsp(
  headers: Record<string, string | string[]> | undefined,
  origin: string,
): Record<string, string[]> {
  const out: Record<string, string[]> = {}
  const csp: string[] = []
  const policy: string[] = []
  for (const [name, value] of Object.entries(headers ?? {})) {
    const values = Array.isArray(value) ? value : [value]
    if (name.toLowerCase() === 'content-security-policy') csp.push(...values)
    else if (name.toLowerCase() === 'permissions-policy') policy.push(...values)
    else out[name] = values
  }
  out['Content-Security-Policy'] = [...csp, knowledgeViewCsp(origin)]
  // The header is one Structured Fields dictionary: a single malformed member
  // from the server would make Blink ignore the whole field, ours included
  // (RFC 8941 §4.2). So of the server's policy only its plain switch-offs
  // (`feature=()`) are passed on; the rest is dropped. Ours goes last.
  out['Permissions-Policy'] = [...serverSwitchOffs(policy), KNOWLEDGE_VIEW_PERMISSIONS_POLICY]
  return out
}

export function knowledgeViewPreferences(partition: string, packaged: boolean): Record<string, unknown> {
  return {
    partition,
    contextIsolation: true,
    nodeIntegration: false,
    nodeIntegrationInSubFrames: false,
    nodeIntegrationInWorker: false,
    sandbox: true,
    webSecurity: true,
    allowRunningInsecureContent: false,
    webviewTag: false,
    navigateOnDragDrop: false,
    spellcheck: false,
    safeDialogs: true,
    devTools: !packaged,
  }
}

export function normalizeKnowledgeViewBounds(value: unknown): KnowledgeViewBounds | null {
  if (typeof value !== 'object' || value === null) return null
  const v = value as Record<string, unknown>
  const n = (key: string): number | null => {
    const x = v[key]
    return typeof x === 'number' && Number.isFinite(x) ? x : null
  }
  const x = n('x'), y = n('y'), width = n('width'), height = n('height')
  if (x === null || y === null || width === null || height === null) return null
  return {
    x: Math.max(0, Math.round(x)),
    y: Math.max(0, Math.round(y)),
    width: Math.max(1, Math.round(width)),
    height: Math.max(1, Math.round(height)),
  }
}

// ── Electron is imported for types only, so tests can hand in fakes ─────────

export interface KnowledgeViewHostDeps {
  window: () => BrowserWindow | null
  createView: (webPreferences: Record<string, unknown>) => WebContentsView
  sessionFor: (partition: string) => Session
  target: () => KnowledgeViewTarget | null
  binding: () => string | null
  packaged: boolean
  onState?: (state: KnowledgeViewState) => void
  now?: () => number
  setTimer?: (fn: () => void, ms: number) => unknown
  clearTimer?: (handle: unknown) => void
  log?: (message: string) => void
  partitionName?: () => string
  /** Port of the dead proxy the partition's other traffic is sent to
   * (contract decision #30). Defaults to one listener of our own per process. */
  proxyPort?: () => Promise<number>
  /** KB-C2-04a: the instance's UI session (KnowledgeUiInstance in the app). Without it no page request carries a session. */
  ui?: (context: KnowledgeViewUiContext) => KnowledgeViewUi
  /** BCP 47 locale for the bridge's `state` (U7.3); 'en' without it. */
  locale?: () => string
}

/** What the host hands the UI session of a new instance. */
export interface KnowledgeViewUiContext {
  partition: string
  binding: string
  origin: string
  /** Shared by the binding's instances; a new one once the binding ends (U3.8). */
  ledger: UiSessionLedger
  onEvent: (event: UiInstanceEvent) => void
}

/** The host's view of KnowledgeUiInstance. */
export interface KnowledgeViewUi {
  start(): Promise<boolean>
  inject(request: InjectRequest, webContentsId: number): InjectDecision
  responded(id: number, statusCode: number | undefined): void
  destroy(): void
  /** U6.4 ⑬⑭ for the bridge (design §6: open while the session has a JWT it may inject). */
  bridgeAdmission(): { open: boolean; generation: number }
  /** U7.2: settings ∩ the exchange, or null when admission is closed or there is no exchange. */
  citationScope(): readonly string[] | null
  readCitation(generation: number, ref: CitationRef, signal: AbortSignal, visible: () => boolean): Promise<ChunkRead>
}

/** U6.6: the third violation closing within this window of one binding is final. */
export const KNOWLEDGE_BRIDGE_CLOSE_WINDOW_MS = UI_SESSION_WINDOW_MS
export const KNOWLEDGE_BRIDGE_CLOSE_LIMIT = 3

// The app's own --bg (styles.css), so the area does not flash while the page loads.
const BACKGROUND: Record<KnowledgeViewTheme, string> = { light: '#f4f4ef', dark: '#0a0a0a' }

// ── Contract decision #30: TURN over TCP ─────────────────────────────────────
// WebRTC's TCP (TURN-TCP, ICE-TCP) opens its own sockets: neither webRequest nor
// the CSP sees them, and `disable_non_proxied_udp` only stops UDP (the 11-08
// Mac smoke reached a foreign TURN port). Chromium does resolve those sockets
// through the session's proxy, so the partition gets a fixed proxy that goes
// nowhere: a listener of ours that drops every connection. The allowed origin
// alone bypasses it, by exact host:port; `<-loopback>` first, or Chromium would
// let every loopback address through implicitly. No DIRECT fallback.

/** The partition's proxy settings for `origin` and the dead proxy's port. */
export function knowledgeViewProxyConfig(origin: string, port: number): { mode: 'fixed_servers'; proxyRules: string; proxyBypassRules: string } {
  const url = new URL(origin)
  // Never a pattern in the bypass list: `*` or a `,`/`;` would open it up.
  if (!isPlainHostname(url.hostname)) throw new Error('knowledge view: host name not allowed in the proxy bypass')
  // Port 0 in a bypass rule means any port (#1649 PR-Daemon L1).
  if (url.port === '0') throw new Error('knowledge view: port 0 not allowed in the proxy bypass')
  const hostPort = `${url.hostname}:${url.port || (url.protocol === 'https:' ? '443' : '80')}`
  return { mode: 'fixed_servers', proxyRules: `http://127.0.0.1:${port}`, proxyBypassRules: `<-loopback>;${hostPort}` }
}

let blackhole: Promise<number> | null = null

/** The process's dead proxy: listens on loopback and drops every connection.
 * Started once, never kept the process alive. A failure to start lets the next
 * caller try again; an error once listening (a failed accept, say) is only
 * logged: the listener stays, since partitions already point at its port. */
export function knowledgeViewBlackhole(log: (message: string) => void = () => {}): Promise<number> {
  blackhole ??= listenDeadProxy(log).then((server) => (server.address() as net.AddressInfo).port, (error: unknown) => {
    blackhole = null
    throw error
  })
  return blackhole
}

/** One dead-proxy listener (the singleton above is the one in use). */
export function listenDeadProxy(log: (message: string) => void): Promise<net.Server> {
  return new Promise<net.Server>((resolve, reject) => {
    const server = net.createServer((socket) => socket.destroy())
    let listening = false
    server.on('error', (error) => {
      if (listening) { log(`[knowledge-view] dead proxy: ${error.message}`); return }
      reject(error)
    })
    server.listen(0, '127.0.0.1', () => {
      listening = true
      server.unref()
      const address = server.address()
      if (typeof address === 'object' && address !== null) resolve(server)
      else { server.close(); reject(new Error('no port')) }
    })
  })
}

/** Installs the partition-wide refusals. Done before the view exists, so the
 * first request already meets them. */
export function hardenKnowledgeSession(ses: Session, origin: string, onBridge: (details: BridgeRequestDetails) => void = () => {}): void {
  ses.setPermissionRequestHandler((_wc, _permission, callback) => callback(false))
  ses.setPermissionCheckHandler(() => false)
  ses.setDevicePermissionHandler(() => false)
  ses.on('will-download', (event, item) => {
    event.preventDefault()
    item.cancel()
  })
  ses.on('select-hid-device', (event, _details, callback) => { event.preventDefault(); callback() })
  ses.on('select-serial-port', (event, _ports, _wc, callback) => { event.preventDefault(); callback('') })
  ses.on('select-usb-device', (event, _details, callback) => { event.preventDefault(); callback() })
  // U4.3: CSP `worker-src 'none'` already blocks registration; should one land
  // anyway, its registration and storage go at once.
  ses.serviceWorkers.on('registration-completed', () => {
    void ses.clearStorageData({ storages: ['serviceworkers', 'cachestorage'] }).catch(() => {})
  })
  // The partition's one onBeforeRequest (Electron keeps the last listener
  // only): the bridge's reserved path first, always cancelled, so it never
  // reaches the server (U6.1 b); then the origin filter. destroy() replaces
  // the whole listener, the bridge with it.
  ses.webRequest.onBeforeRequest((details, callback) => {
    const reserved = reservedRequest(details.url, details.method, origin)
    if (reserved === 'bridge') {
      try {
        onBridge(details as unknown as BridgeRequestDetails)
      } catch {
        // The request is cancelled whatever the bridge made of it.
      }
    }
    callback({ cancel: reserved !== null || !isAllowedRequest(details.url, origin) })
  })
  ses.webRequest.onHeadersReceived((details, callback) => callback({ responseHeaders: withKnowledgeCsp(details.responseHeaders, origin) }))
}

/** The frame a request came from is the page's main frame; null when the request has no frame. */
function isMainFrameOf(frame: { processId: number; routingId: number } | null | undefined, contents: WebContents): boolean | null {
  if (!frame) return null
  try {
    const main = contents.mainFrame
    return frame.processId === main.processId && frame.routingId === main.routingId
  } catch {
    return false
  }
}

/** KB-C2-04a: the partition's injection (U4.5) and the 401 watch on injected requests (U3.8). */
export function wireKnowledgeUi(ses: Session, contents: WebContents, ui: KnowledgeViewUi): void {
  ses.webRequest.onBeforeSendHeaders((details, callback) => {
    const decision = ui.inject({
      id: details.id,
      webContentsId: details.webContentsId,
      mainFrame: isMainFrameOf(details.frame, contents),
      resourceType: details.resourceType,
      method: details.method,
      url: details.url,
      requestHeaders: details.requestHeaders,
    }, contents.id)
    callback(decision.cancel ? { cancel: true } : { requestHeaders: decision.requestHeaders })
  })
  ses.webRequest.onCompleted((details) => ui.responded(details.id, details.statusCode))
  ses.webRequest.onErrorOccurred((details) => ui.responded(details.id, undefined))
}

/** Per-WebContents refusals; `onLeftOrigin` is the defense in depth for a main
 * frame that commits elsewhere anyway (U4.8 view-level handling). */
export function guardKnowledgeContents(contents: WebContents, origin: string, onLeftOrigin: () => void): void {
  contents.setWebRTCIPHandlingPolicy('disable_non_proxied_udp')
  contents.setWindowOpenHandler(() => ({ action: 'deny' }))
  contents.on('will-navigate', (event, url) => {
    if (!isAllowedDocument(url, origin)) event.preventDefault()
  })
  contents.on('will-frame-navigate', (details) => {
    if (!details.isMainFrame || !isAllowedDocument(details.url, origin)) details.preventDefault()
  })
  contents.on('will-redirect', (details) => {
    if (!isAllowedDocument(details.url, origin)) details.preventDefault()
  })
  contents.on('will-attach-webview', (event) => event.preventDefault())
  // Called with no certificate: the request goes on without one (U4.2: never
  // present a client certificate on the page's behalf).
  contents.on('select-client-certificate', (event, _url, _list, callback) => {
    event.preventDefault()
    ;(callback as (certificate?: unknown) => void)()
  })
  contents.on('login', (event, _details, _authInfo, callback) => { event.preventDefault(); callback() })
  contents.on('select-bluetooth-device', (event, _devices, callback) => { event.preventDefault(); callback('') })
  contents.on('did-navigate', (_event, url) => {
    if (!isAllowedDocument(url, origin)) onLeftOrigin()
  })
}

interface Instance {
  view: WebContentsView
  session: Session
  window: BrowserWindow
  binding: string
  target: KnowledgeViewTarget
  /** The first load of the page finished. */
  loaded: boolean
  /** U4.5: injection came back from a pause after the first load had begun
   * but before it finished; the page is reloaded once that load is done. */
  reloadPending: boolean
  /** loadURL was called: the page's scripts may already be asking for data. */
  navigating: boolean
  /** Hung (ADR §4): kept per instance so a hide/show round trip does not
   * put the hung page back over the content area. */
  unresponsive: boolean
  /** The first load in flight; a hide does not cancel it, a second show waits for it. */
  loading: Promise<boolean> | null
  /** Removed on destroy, so windows do not collect listeners (one per instance). */
  onWindowClosed: () => void
  /** The partition's dead proxy is in place (decision #30); the page loads only after. */
  proxied: Promise<void>
  /** The main frame's last commit answered 4xx/5xx: an error page is not the knowledge page. */
  httpFailed: boolean
  /** KB-C2-04a: the UI session, when the host has one. */
  ui: KnowledgeViewUi | null
  /** KB-C2-04a-2: the bridge, only with a UI session (no session, nothing to admit). */
  bridge: { bridge: KnowledgeBridge; channel: KnowledgeBridgeChannel } | null
}

/** Main's single owner of KnowledgeView instances (one binding for now). */
export class KnowledgeViewHost {
  private instance: Instance | null = null
  private partitions = 0
  /** Bumped by every show/hide/destroy; a show that resumes under a later number was overtaken. */
  private request = 0
  private visible = false
  private bounds: KnowledgeViewBounds | null = null
  private theme: KnowledgeViewTheme = 'dark'
  private crashes: number[] = []
  /** `closed` belongs to the binding it happened under; another binding, or
   * Off, ends it (§9.2: binding-level destruction ends `closed`). */
  private closed: { code: KnowledgeViewClosedCode; binding: string; final?: true } | null = null
  /** U6.6: when the bridge closed for a violation, per binding; a reload does not clear it, Knowledge settings do. */
  private bridgeCloses: number[] = []
  private bridgeBinding: string | null = null
  /** U3.8's per-binding counts, for `ledgerBinding`. */
  private ledger = new UiSessionLedger()
  private ledgerBinding: string | null = null
  private crashBinding: string | null = null
  /** App quit: no new instance from here on. */
  private disposed = false
  private rebuildTimer: unknown = null
  private state: KnowledgeViewState = { state: 'hidden' }
  private readonly now: () => number
  private readonly setTimer: (fn: () => void, ms: number) => unknown
  private readonly clearTimer: (handle: unknown) => void
  private readonly log: (message: string) => void
  private readonly partitionName: () => string

  constructor(private readonly deps: KnowledgeViewHostDeps) {
    this.now = deps.now ?? Date.now
    this.setTimer = deps.setTimer ?? ((fn, ms) => setTimeout(fn, ms))
    this.clearTimer = deps.clearTimer ?? ((handle) => clearTimeout(handle as ReturnType<typeof setTimeout>))
    this.log = deps.log ?? ((message) => console.warn(message))
    this.partitionName = deps.partitionName ?? (() => KNOWLEDGE_VIEW_PARTITION_PREFIX + randomBytes(16).toString('base64url'))
  }

  status(): KnowledgeViewState {
    return this.state
  }

  /** Shows the view at `bounds`, creating an instance when there is none for
   * the current binding, window and address. */
  async show(rawBounds: unknown, rawTheme?: unknown): Promise<KnowledgeViewResult> {
    const bounds = normalizeKnowledgeViewBounds(rawBounds)
    if (!bounds) throw new Error('invalid bounds')
    this.request++
    this.visible = true
    this.bounds = bounds
    if ((rawTheme === 'light' || rawTheme === 'dark') && rawTheme !== this.theme) {
      this.theme = rawTheme
      if (this.instance) this.sendBridgeState(this.instance)
    }

    const win = this.deps.window()
    if (this.disposed || !win || win.isDestroyed()) return { ok: false, code: 'window_unavailable' }
    const binding = this.deps.binding()
    if (binding === null) {
      this.endBinding()
      this.endBridgeCount()
      this.destroy('knowledge off')
      this.setState({ state: 'unavailable', code: 'knowledge_off' })
      return { ok: false, code: 'knowledge_off' }
    }
    if (this.closed && this.closed.binding !== binding) this.endBinding()
    if (this.bridgeBinding !== null && this.bridgeBinding !== binding) this.endBridgeCount()
    if (this.crashBinding !== binding) this.crashes = []
    if (this.closed) {
      // A page that mounts again learns the whole state, `final` included (U6.6).
      this.setState({ state: 'closed', code: this.closed.code, ...(this.closed.final ? { final: true as const } : {}) })
      return { ok: false, code: this.closed.code }
    }
    const target = this.deps.target()
    if (!target) {
      this.destroy('no address')
      this.setState({ state: 'unavailable', code: 'not_configured' })
      return { ok: false, code: 'not_configured' }
    }
    const current = this.instance
    if (current && (current.binding !== binding || current.window !== win || current.target.url !== target.url
      || current.view.webContents.isDestroyed())) {
      this.destroy('binding, window or address changed')
    }
    let instance = this.instance
    if (!instance) {
      if (this.partitions >= MAX_KNOWLEDGE_VIEW_PARTITIONS) {
        this.setState({ state: 'unavailable', code: 'partition_limit' })
        return { ok: false, code: 'partition_limit' }
      }
      instance = this.create(win, binding, target)
    }
    instance.view.setBackgroundColor(BACKGROUND[this.theme])
    instance.view.setBounds(bounds)
    if (instance.unresponsive) {
      instance.view.setVisible(false)
      this.setState({ state: 'degraded', code: 'unresponsive' })
      return { ok: true }
    }
    instance.view.setVisible(true)
    if (instance.loaded) {
      if (this.state.state === 'hidden') this.setState({ state: 'ready' })
      return { ok: true }
    }
    // Taken after any destruction above; a hide or destroy during the load
    // bumps it again, and this caller's stale result is dropped. The load
    // itself carries on: a later show uses it instead of loading again.
    const mine = ++this.request
    this.setState({ state: 'loading' })
    const loaded = await this.load(instance)
    if (mine !== this.request || this.instance !== instance) return { ok: false, code: 'superseded' }
    return loaded ? { ok: true } : { ok: false, code: 'load_failed' }
  }

  /** The instance's first load, shared by every show while it is in flight. */
  private load(instance: Instance): Promise<boolean> {
    if (instance.loading) return instance.loading
    instance.loading = new Promise<boolean>((resolve) => {
      let settled = false
      const finish = (error: unknown | null): void => {
        if (settled) return
        settled = true
        this.clearTimer(timer)
        instance.loading = null
        if (this.instance !== instance) return resolve(false)
        const failure = error ?? (instance.httpFailed ? new Error('the server answered with an error page') : null)
        if (failure) {
          this.log(`[knowledge-view] load failed: ${failure instanceof Error ? failure.message : String(failure)}`)
          this.loadFailed(instance)
          return resolve(false)
        }
        instance.loaded = true
        if (this.visible && this.state.state === 'loading' && !instance.unresponsive) this.setState({ state: 'ready' })
        if (instance.reloadPending) {
          instance.reloadPending = false
          if (!instance.view.webContents.isDestroyed()) instance.view.webContents.reload()
        }
        resolve(true)
      }
      // A load that never settles would leave `loading` up for good, with no
      // way to Reload; past the limit it counts as failed (PR-Daemon #1634 F1).
      const timer = this.setTimer(() => {
        if (settled) return
        try {
          if (!instance.view.webContents.isDestroyed()) instance.view.webContents.stop()
        } catch {
          // Stopping is best effort; the load is treated as failed either way.
        }
        finish(new Error('first load timed out'))
      }, KNOWLEDGE_VIEW_FIRST_LOAD_TIMEOUT_MS)
      // Not one request before the dead proxy is in place: a page that loaded
      // first could open TURN-TCP directly (decision #30). Nor before the
      // first JWT (U4.5); without one the UI session's event has set the state.
      const current = (): boolean => !settled && this.instance === instance
      instance.proxied
        .then((): Promise<boolean> | boolean => (current() && instance.ui ? instance.ui.start() : true))
        .then(async (ready): Promise<boolean> => {
          // The session is there now: the page's first state says so.
          if (ready && current()) this.sendBridgeState(instance)
          if (ready && current()) {
            instance.navigating = true
            await instance.view.webContents.loadURL(instance.target.url)
          }
          return ready
        })
        .then((ready) => {
          if (ready) return finish(null)
          if (settled) return
          settled = true
          this.clearTimer(timer)
          instance.loading = null
          resolve(false)
        }, (error: unknown) => finish(error))
    })
    return instance.loading
  }

  /** Shows the page again later without throwing; a failure is a typed state, not a blank area. */
  private reopen(bounds: KnowledgeViewBounds): void {
    this.show(bounds).catch((error: unknown) => {
      this.log(`[knowledge-view] reopen failed: ${error instanceof Error ? error.message : String(error)}`)
      this.setState({ state: 'degraded', code: 'load_failed' })
    })
  }

  setBounds(rawBounds: unknown): void {
    const bounds = normalizeKnowledgeViewBounds(rawBounds)
    if (!bounds) throw new Error('invalid bounds')
    this.bounds = bounds
    if (this.visible && this.instance) this.instance.view.setBounds(bounds)
  }

  /** Route away: the instance and its session stay (ADR §4). */
  hide(): void {
    this.request++
    this.visible = false
    this.instance?.view.setVisible(false)
    if (this.state.state !== 'unavailable' && this.state.state !== 'closed') this.setState({ state: 'hidden' })
  }

  /** The user's reload: a new instance; also the way out of `closed` (§9.2). */
  async reload(rawBounds: unknown, rawTheme?: unknown): Promise<KnowledgeViewResult> {
    if (this.disposed) return { ok: false, code: 'window_unavailable' }
    // U6.6: past the third bridge closing, Reload is not a way out.
    if (this.closed?.final && this.closed.binding === this.deps.binding()) return { ok: false, code: this.closed.code }
    this.endBinding()
    this.destroy('reload')
    return this.show(rawBounds, rawTheme)
  }

  /** The Knowledge settings changed: a different binding (or Off) ends the
   * instance and whatever `closed` or crash count the old one left. */
  bindingChanged(): void {
    const binding = this.deps.binding()
    let ended = false
    if (this.closed && this.closed.binding !== binding) {
      this.endBinding()
      this.setState({ state: 'hidden' })
      ended = true
    }
    if (this.crashBinding !== binding) this.crashes = []
    if (this.bridgeBinding !== binding) this.endBridgeCount()
    if (this.instance && this.instance.binding !== binding) {
      this.destroy('binding changed')
      ended = true
    }
    // On screen: say why at once (Off), or open the new binding's page
    // (U4.8: the entry states why it is unavailable), never a blank area.
    // With no instance (Off, then On again) the page is re-evaluated too,
    // unless a crash rebuild is already waiting out its backoff.
    if ((ended || (!this.instance && this.rebuildTimer === null)) && this.visible && this.bounds) this.reopen(this.bounds)
  }

  /** Binding-level destruction (U4.8) without closing the door: window teardown, tests. */
  destroyAll(reason: string): void {
    this.endBinding()
    this.destroy(reason)
  }

  /** App quit: destroy, and refuse every later show or reload. */
  shutdown(): void {
    this.disposed = true
    this.destroyAll('app quit')
  }

  private endBridgeCount(): void {
    this.bridgeCloses = []
    this.bridgeBinding = null
  }

  private endBinding(): void {
    this.closed = null
    this.ledger = new UiSessionLedger()
    this.ledgerBinding = null
    this.crashes = []
    this.crashBinding = null
  }

  private create(win: BrowserWindow, binding: string, target: KnowledgeViewTarget): Instance {
    const partition = this.partitionName()
    this.partitions++
    const session = this.deps.sessionFor(partition)
    // The instance does not exist yet; its bridge is looked up per request.
    let made: Instance | null = null
    hardenKnowledgeSession(session, target.origin, (details) => { if (made && this.instance === made) made.bridge?.channel.request(details) })
    const view = this.deps.createView(knowledgeViewPreferences(partition, this.deps.packaged))
    if (this.ledgerBinding !== binding) {
      this.ledger = new UiSessionLedger()
      this.ledgerBinding = binding
    }
    const ui = this.deps.ui?.({
      partition, binding, origin: target.origin, ledger: this.ledger,
      onEvent: (event) => this.uiEvent(instance, event),
    }) ?? null
    if (ui) wireKnowledgeUi(session, view.webContents, ui)
    const instance: Instance = {
      view, session, window: win, binding, target, loaded: false, reloadPending: false, navigating: false, unresponsive: false, loading: null, httpFailed: false, ui, bridge: null,
      onWindowClosed: () => { if (this.instance === instance) this.destroy('window closed') },
      proxied: (this.deps.proxyPort ?? (() => knowledgeViewBlackhole((m) => this.log(m))))().then((port) => session.setProxy(knowledgeViewProxyConfig(target.origin, port))),
    }
    instance.proxied.catch(() => {}) // surfaced by load(); never an unhandled rejection
    made = instance
    if (ui) {
      const bridge = new KnowledgeBridge({
        origin: target.origin,
        admission: () => ui.bridgeAdmission(),
        send: (text, document) => instance.bridge?.channel.send(text, document),
        onClose: () => this.bridgeClosed(instance),
        now: this.now,
        setTimer: this.setTimer,
        clearTimer: this.clearTimer,
        log: this.log,
        citation: {
          scope: () => ui.citationScope(),
          read: (generation, ref, signal, visible) => ui.readCitation(generation, ref, signal, visible),
          show: (shown) => this.showCitation(instance, shown),
        },
      })
      instance.bridge = { bridge, channel: new KnowledgeBridgeChannel(view.webContents as never, bridge, this.now, this.log) }
      this.sendBridgeState(instance)
    }
    guardKnowledgeContents(view.webContents, target.origin, () => {
      if (this.instance !== instance) return
      this.log('[knowledge-view] main frame left the allowed origin')
      this.destroy('left the allowed origin')
      this.setState({ state: 'degraded', code: 'navigated_away' })
    })
    // 4xx/5xx on the main frame: the server's error page is not the knowledge
    // page, so it counts as a failed load and Reload is offered.
    view.webContents.on('did-navigate', (_event, _url, httpResponseCode) => {
      if (this.instance !== instance) return
      instance.httpFailed = typeof httpResponseCode === 'number' && httpResponseCode >= 400
      if (instance.httpFailed && instance.loaded) this.loadFailed(instance)
    })
    // Only a load that is still the one being waited for becomes `ready`;
    // a hung page or a failed load keeps its state.
    view.webContents.on('did-finish-load', () => {
      if (this.instance === instance && this.visible && this.state.state === 'loading' && !instance.unresponsive) {
        this.setState({ state: 'ready' })
      }
    })
    view.webContents.on('did-fail-load', (_event, code, _description, _url, isMainFrame) => {
      // -3 is an aborted load (a newer navigation took over).
      if (this.instance === instance && isMainFrame && code !== -3) this.loadFailed(instance)
    })
    view.webContents.on('render-process-gone', (_event, details) => {
      if (this.instance !== instance) return
      this.log(`[knowledge-view] render process gone: ${details.reason}`)
      this.crashed(instance.binding)
    })
    // ADR §4: a hung page is a typed degraded state; the view steps aside so
    // the page's Reload button can be reached, and comes back if it recovers.
    view.webContents.on('unresponsive', () => {
      if (this.instance !== instance) return
      instance.unresponsive = true
      view.setVisible(false)
      this.setState({ state: 'degraded', code: 'unresponsive' })
    })
    view.webContents.on('responsive', () => {
      if (this.instance !== instance || !instance.unresponsive) return
      instance.unresponsive = false
      // Recovering only ends the hang: a load that failed meanwhile stays failed.
      if (this.state.state !== 'degraded' || this.state.code !== 'unresponsive') return
      if (this.visible) view.setVisible(true)
      this.setState(this.visible ? { state: 'ready' } : { state: 'hidden' })
    })
    // The view does not go with its window (Electron resource management).
    win.once('closed', instance.onWindowClosed)
    view.setVisible(false)
    win.contentView.addChildView(view)
    this.instance = instance
    return instance
  }

  /** What the UI session's result means for the view (U3.8, §9.2). */
  private uiEvent(instance: Instance, event: UiInstanceEvent): void {
    if (this.instance !== instance) return
    if (event.kind === 'reload') {
      // U4.5: what was cancelled while injection was paused is asked for again.
      // During the first load the reload waits for that load to finish; before
      // loadURL (the first JWT still coming) nothing has been asked for yet.
      if (instance.loaded) {
        if (!instance.view.webContents.isDestroyed()) instance.view.webContents.reload()
      } else if (instance.navigating) {
        instance.reloadPending = true
      }
      return
    }
    if (event.kind === 'rebuild') {
      // View-level handling: a new instance, new vph; not a crash.
      this.log('[knowledge-view] session rejected; rebuilding the view')
      this.destroy('session rejected')
      if (this.visible && this.bounds) this.reopen(this.bounds)
      return
    }
    if (event.kind === 'closed') {
      this.log(`[knowledge-view] closed: ${event.code}`)
      this.destroy(`closed: ${event.code}`)
      this.closed = { code: event.code, binding: instance.binding }
      this.setState({ state: 'closed', code: event.code })
      return
    }
    this.log('[knowledge-view] no session for the page')
    this.destroy('no session')
    this.setState({ state: 'degraded', code: 'session_unavailable' })
  }

  /** U7.3 `state` for the page: non-secret display hints only. */
  private sendBridgeState(instance: Instance): void {
    if (!instance.bridge || !instance.ui) return
    instance.bridge.bridge.setState({
      // M2 (design §6): the binding is ready while the session may be used, otherwise unavailable.
      profile_state: instance.ui.bridgeAdmission().open ? 'ready' : 'unavailable',
      theme: this.theme,
      locale: this.deps.locale?.() ?? 'en',
    })
  }

  /** U6.6: the bridge closed for a violation: view-level, generation unchanged, `closed/bridge_closed`. */
  private bridgeClosed(instance: Instance): void {
    if (this.instance !== instance) return
    const now = this.now()
    if (this.bridgeBinding !== instance.binding) this.bridgeCloses = []
    this.bridgeBinding = instance.binding
    this.bridgeCloses = this.bridgeCloses.filter((t) => now - t < KNOWLEDGE_BRIDGE_CLOSE_WINDOW_MS)
    this.bridgeCloses.push(now)
    const final = this.bridgeCloses.length >= KNOWLEDGE_BRIDGE_CLOSE_LIMIT
    this.log(`[knowledge-view] closed: bridge_closed${final ? ' (final)' : ''}`)
    this.destroy('bridge closed')
    this.closed = { code: 'bridge_closed', binding: instance.binding, ...(final ? { final: true as const } : {}) }
    this.setState({ state: 'closed', code: 'bridge_closed', ...(final ? { final: true as const } : {}) })
  }

  /** The native view is drawn above the app's page; it steps aside so the
   * page's message and Reload button can be seen. */
  private loadFailed(instance: Instance): void {
    instance.loaded = false
    instance.unresponsive = false
    instance.view.setVisible(false)
    this.setState({ state: 'degraded', code: 'load_failed' })
  }

  /** U4.8: view-level handling with backoff; too many crashes close it. */
  private crashed(binding: string): void {
    const now = this.now()
    if (this.crashBinding !== binding) this.crashes = []
    this.crashBinding = binding
    this.crashes = this.crashes.filter((at) => now - at < KNOWLEDGE_VIEW_CRASH_WINDOW_MS)
    this.crashes.push(now)
    this.destroy('render process gone')
    if (this.crashes.length > KNOWLEDGE_VIEW_MAX_CRASHES) {
      this.closed = { code: 'crash_loop', binding }
      this.setState({ state: 'closed', code: 'crash_loop' })
      return
    }
    this.setState({ state: 'degraded', code: 'crashed' })
    if (!this.visible || !this.bounds) return
    const bounds = this.bounds
    const delay = CRASH_BACKOFF_MS[Math.min(this.crashes.length - 1, CRASH_BACKOFF_MS.length - 1)]
    this.rebuildTimer = this.setTimer(() => {
      this.rebuildTimer = null
      if (this.visible && !this.instance && !this.closed) this.reopen(this.bounds ?? bounds)
    }, delay)
  }

  /** U4.7 ④⑤: seal the partition, take the page down, and clear the
   * partition once the page is gone. Never throws; nothing here reopens the
   * view. The partition is never used again either way. */
  private destroy(reason: string): void {
    this.request++
    if (this.rebuildTimer !== null) {
      this.clearTimer(this.rebuildTimer)
      this.rebuildTimer = null
    }
    const instance = this.instance
    if (!instance) return
    this.clearCitation(instance)
    this.instance = null
    // The bridge takes and sends nothing more (its listener goes with the seal below).
    try {
      instance.bridge?.bridge.destroy()
      instance.bridge?.channel.dispose()
    } catch (error) {
      this.log(`[knowledge-view] end bridge failed (${reason}): ${String(error)}`)
    }
    // U4.7: injection stops and the family is revoked before the page goes.
    try {
      instance.ui?.destroy()
      // The partition's listeners let go of the instance (and nothing of it is used again).
      if (instance.ui) {
        instance.session.webRequest.onBeforeSendHeaders(null)
        instance.session.webRequest.onCompleted(null)
        instance.session.webRequest.onErrorOccurred(null)
      }
    } catch (error) {
      this.log(`[knowledge-view] end UI session failed (${reason}): ${String(error)}`)
    }
    try {
      instance.window.removeListener('closed', instance.onWindowClosed)
    } catch {
      // A destroyed window has no listeners left to remove.
    }
    const ses = instance.session
    // Sealed first: nothing the closing page still has queued (unload
    // handlers, keepalive) leaves or comes back to refill the partition.
    try {
      ses.webRequest.onBeforeRequest((_details, callback) => callback({ cancel: true }))
    } catch (error) {
      this.log(`[knowledge-view] seal partition failed (${reason}): ${String(error)}`)
    }
    try {
      if (!instance.window.isDestroyed()) instance.window.contentView.removeChildView(instance.view)
    } catch (error) {
      this.log(`[knowledge-view] remove view failed (${reason}): ${String(error)}`)
    }
    try {
      if (!instance.view.webContents.isDestroyed()) instance.view.webContents.close()
    } catch (error) {
      this.log(`[knowledge-view] close page failed (${reason}): ${String(error)}`)
    }
    void this.clearWhenGone(instance, reason)
    if (this.state.state !== 'closed') this.setState({ state: 'hidden' })
  }

  /** Electron destroys a closed page asynchronously; the partition is cleared
   * after that, never while the page may still write. A page not gone within
   * the wait has its renderer killed first, and the partition is cleared again
   * once it is finally destroyed. */
  private async clearWhenGone(instance: Instance, reason: string): Promise<void> {
    const contents = instance.view.webContents
    const destroyed = (): Promise<boolean> => new Promise<boolean>((resolve) => {
      if (contents.isDestroyed()) return resolve(true)
      const timer = this.setTimer(() => resolve(false), KNOWLEDGE_VIEW_DESTROY_WAIT_MS)
      contents.once('destroyed', () => {
        this.clearTimer(timer)
        resolve(true)
      })
    })
    if (!(await destroyed())) {
      this.log(`[knowledge-view] page not destroyed in time (${reason}); killing its renderer`)
      try {
        if (!contents.isDestroyed()) contents.forcefullyCrashRenderer()
      } catch (error) {
        this.log(`[knowledge-view] kill renderer failed (${reason}): ${String(error)}`)
      }
      if (!(await destroyed())) {
        // Last resort: the partition is sealed and never used again; clear what
        // can be cleared now and once more if the page is ever destroyed.
        this.log(`[knowledge-view] page still not destroyed (${reason}); clearing a sealed partition`)
        contents.once('destroyed', () => { void this.clearPartition(instance, `${reason}, late`) })
      }
    }
    await this.clearPartition(instance, reason)
  }

  private async clearPartition(instance: Instance, reason: string): Promise<void> {
    const ses = instance.session
    const steps: Array<() => Promise<void>> = [
      () => ses.closeAllConnections(),
      () => ses.clearStorageData(),
      () => ses.clearCache(),
      () => ses.clearAuthCache(),
    ]
    for (const step of steps) {
      try {
        await step()
      } catch (error) {
        this.log(`[knowledge-view] partition cleanup failed (${reason}): ${String(error)}`)
      }
    }
  }

  private setState(state: KnowledgeViewState): void {
    this.state = state
    this.deps.onState?.(state)
  }

  /** U7.2: the citation strip. Only the main window, and only the three ids. */
  private showCitation(instance: Instance, shown: CitationShown): void {
    this.sendCitation(instance, {
      title: shown.title,
      text: shown.text,
      ref: {
        knowledge_base_id: shown.ref.knowledge_base_id,
        knowledge_id: shown.ref.knowledge_id,
        chunk_id: shown.ref.chunk_id,
      },
    })
  }

  /** A destroyed instance, or a binding change that destroys it, clears the strip. */
  private clearCitation(instance: Instance): void {
    this.sendCitation(instance, null)
  }

  private sendCitation(instance: Instance, payload: { title: string | null; text: string; ref: { knowledge_base_id: string; knowledge_id: string; chunk_id: string } } | null): void {
    if (this.instance !== instance) return
    const win = instance.window
    try {
      if (win.isDestroyed()) return
      win.webContents?.send(IpcChannels.KnowledgeViewCitation, payload)
    } catch (error) {
      this.log(`[knowledge-view] citation failed: ${String(error)}`)
    }
  }
}

/** The four renderer calls, each behind main's sender guard (the Agent24
 * window's main frame only; the KnowledgeView itself can never reach them). */
export function registerKnowledgeViewIpc(
  ipc: Pick<IpcMain, 'handle'>,
  guard: IpcGuard,
  channels: { show: string; bounds: string; hide: string; reload: string },
  host: () => KnowledgeViewHost | null,
): void {
  const unavailable: KnowledgeViewResult = { ok: false, code: 'window_unavailable' }
  ipc.handle(channels.show, guard((_event: IpcMainInvokeEvent, bounds: unknown, theme: unknown) => host()?.show(bounds, theme) ?? unavailable))
  ipc.handle(channels.bounds, guard((_event: IpcMainInvokeEvent, bounds: unknown) => { host()?.setBounds(bounds) }))
  ipc.handle(channels.hide, guard(() => { host()?.hide() }))
  ipc.handle(channels.reload, guard((_event: IpcMainInvokeEvent, bounds: unknown, theme: unknown) => host()?.reload(bounds, theme) ?? unavailable))
}
