import net from 'node:net'
import { describe, expect, it, vi } from 'vitest'
import {
  KNOWLEDGE_VIEW_DEV_URL_ENV,
  KNOWLEDGE_VIEW_PARTITION_PREFIX,
  MAX_KNOWLEDGE_VIEW_PARTITIONS,
  KNOWLEDGE_VIEW_DESTROY_WAIT_MS,
  KNOWLEDGE_VIEW_FIRST_LOAD_TIMEOUT_MS,
  KnowledgeViewHost,
  type KnowledgeViewHostDeps,
  type KnowledgeViewUi,
  type KnowledgeViewUiContext,
  devKnowledgeViewTarget,
  resolveKnowledgeViewTarget,
  settingsKnowledgeViewTarget,
  isAllowedDocument,
  isAllowedRequest,
  knowledgeViewCsp,
  knowledgeViewPreferences,
  knowledgeViewTarget,
  normalizeKnowledgeViewBounds,
  registerKnowledgeViewIpc,
  withKnowledgeCsp,
  KNOWLEDGE_VIEW_PERMISSIONS_POLICY,
  knowledgeViewProxyConfig,
  knowledgeViewBlackhole,
  listenDeadProxy,
} from './knowledge-view'

const ORIGIN = 'https://kb.example.org'
const URL_ = `${ORIGIN}/embed-admin/knowledge-bases`
const BOUNDS = { x: 10.4, y: 20, width: 800, height: 600 }

// eslint-disable-next-line @typescript-eslint/no-explicit-any -- fakes for Electron's many listener shapes
type Listener = (...args: any[]) => unknown
/** What the next page load does; tests replace it. */
let nextLoad: () => Promise<void> = () => Promise.resolve()

class FakeEmitter {
  readonly listeners = new Map<string, Listener[]>()
  on(event: string, listener: Listener): this {
    this.listeners.set(event, [...(this.listeners.get(event) ?? []), listener])
    return this
  }
  once(event: string, listener: Listener): this {
    const wrapped: Listener & { original?: Listener } = (...args) => {
      this.listeners.set(event, (this.listeners.get(event) ?? []).filter((l) => l !== wrapped))
      return listener(...args)
    }
    wrapped.original = listener
    return this.on(event, wrapped)
  }
  removeListener(event: string, listener: Listener): this {
    // Like Node's EventEmitter: a once() listener is removed by the function given to once().
    this.listeners.set(event, (this.listeners.get(event) ?? []).filter((l) => l !== listener && (l as { original?: Listener }).original !== listener))
    return this
  }
  emit(event: string, ...args: unknown[]): void {
    for (const l of this.listeners.get(event) ?? []) l(...args)
  }
}

function prevented<T extends object>(extra?: T): T & { preventDefault: () => void; prevented: boolean } {
  const e = { ...(extra ?? {}), prevented: false, preventDefault: () => { e.prevented = true } } as T & { preventDefault: () => void; prevented: boolean }
  return e
}

class FakeSession extends FakeEmitter {
  permissionRequest: Listener | null = null
  permissionCheck: Listener | null = null
  devicePermission: Listener | null = null
  beforeRequest: Listener | null = null
  headersReceived: Listener | null = null
  readonly serviceWorkers = new FakeEmitter()
  readonly cleared: string[] = []
  beforeSendHeaders: Listener | null = null
  completed: Listener | null = null
  errorOccurred: Listener | null = null
  readonly webRequest = {
    onBeforeRequest: (l: Listener) => { this.beforeRequest = l },
    onHeadersReceived: (l: Listener) => { this.headersReceived = l },
    onBeforeSendHeaders: (l: Listener) => { this.beforeSendHeaders = l },
    onCompleted: (l: Listener) => { this.completed = l },
    onErrorOccurred: (l: Listener) => { this.errorOccurred = l },
  }
  /** Calls in order, for checking that cleanup comes after the page is gone. */
  readonly log: string[]
  constructor(readonly partition: string, log: string[] = []) { super(); this.log = log }
  setPermissionRequestHandler(l: Listener) { this.permissionRequest = l }
  setPermissionCheckHandler(l: Listener) { this.permissionCheck = l }
  setDevicePermissionHandler(l: Listener) { this.devicePermission = l }
  proxy: unknown = null
  /** Held open by a test to see that nothing loads before the proxy is set. */
  proxyGate: Promise<void> = Promise.resolve()
  setProxy(config: unknown) { this.proxy = config; this.log.push('proxy'); return this.proxyGate }
  private record(what: string) { this.cleared.push(what); this.log.push(what); return Promise.resolve() }
  clearStorageData(options?: { storages?: string[] }) { return this.record(options ? `storage:${options.storages?.join(',')}` : 'storage') }
  clearCache() { return this.record('cache') }
  clearAuthCache() { return this.record('auth') }
  closeAllConnections() { return this.record('connections') }
  /** What the partition's network filter says about `url`. */
  allows(url: string): boolean {
    let out: { cancel: boolean } | null = null
    this.beforeRequest?.({ url }, (r: { cancel: boolean }) => { out = r })
    return out !== null && !(out as { cancel: boolean }).cancel
  }
}

let contentsIds = 100
class FakeContents extends FakeEmitter {
  readonly id = ++contentsIds
  readonly mainFrame = {
    processId: 4, routingId: this.id, origin: ORIGIN, detached: false, scripts: [] as string[],
    executeJavaScript(code: string) { this.scripts.push(code); return Promise.resolve() },
  }
  destroyed = false
  url = ''
  openHandler: Listener | null = null
  webrtc: string | null = null
  loads: string[] = []
  isDestroyed() { return this.destroyed }
  getURL() { return this.url }
  loadURL(url: string) { this.loads.push(url); this.url = url; return nextLoad() }
  reload() {}
  stopped = 0
  stop() { this.stopped++ }
  /** Electron destroys a closed page later; tests that care set this. */
  asyncClose = false
  closed = false
  close() {
    this.closed = true
    if (!this.asyncClose) this.finishClose()
  }
  finishClose() {
    this.destroyed = true
    this.emit('destroyed')
  }
  crashedByHost = false
  forcefullyCrashRenderer() { this.crashedByHost = true }
  setWindowOpenHandler(l: Listener) { this.openHandler = l }
  setWebRTCIPHandlingPolicy(p: string) { this.webrtc = p }
}

class FakeView {
  readonly webContents = new FakeContents()
  bounds: unknown = null
  visible: boolean | null = null
  background: string | null = null
  constructor(readonly prefs: Record<string, unknown>) {}
  setBounds(b: unknown) { this.bounds = b }
  setVisible(v: boolean) { this.visible = v }
  setBackgroundColor(c: string) { this.background = c }
}

class FakeWindow extends FakeEmitter {
  destroyed = false
  readonly children: FakeView[] = []
  readonly contentView = {
    addChildView: (v: FakeView) => { this.children.push(v) },
    removeChildView: (v: FakeView) => { this.children.splice(this.children.indexOf(v), 1) },
  }
  isDestroyed() { return this.destroyed }
}

const ALL_CLEARED = ['connections', 'storage', 'cache', 'auth']
const flush = () => new Promise((resolve) => setTimeout(resolve, 0))

function setup(over: Partial<{ binding: string | null; target: ReturnType<typeof knowledgeViewTarget>; packaged: boolean; ui: (c: KnowledgeViewUiContext) => KnowledgeViewUi }> = {}) {
  const views: FakeView[] = []
  const sessions: FakeSession[] = []
  const states: unknown[] = []
  const timers: Array<{ fn: () => void; ms: number }> = []
  const cleared: number[] = []
  const loadTimers: Array<{ fn: () => void; ms: number }> = []
  let now = 0
  const env = {
    win: new FakeWindow() as FakeWindow | null,
    binding: 'binding' in over ? over.binding! : 'https://kb.example.org/api/v1',
    target: 'target' in over ? over.target! : knowledgeViewTarget(URL_),
  }
  let seq = 0
  const deps = {
    window: () => env.win,
    createView: (prefs: Record<string, unknown>) => { const v = new FakeView(prefs); views.push(v); return v },
    sessionFor: (partition: string) => { const s = new FakeSession(partition); sessions.push(s); return s },
    target: () => env.target,
    binding: () => env.binding,
    packaged: over.packaged ?? true,
    onState: (s: unknown) => states.push(s),
    now: () => now,
    // First-load timeouts are kept apart, so the crash and cleanup timers
    // the other tests count are not mixed with one per load.
    setTimer: (fn: () => void, ms: number) => {
      if (ms === KNOWLEDGE_VIEW_FIRST_LOAD_TIMEOUT_MS) { loadTimers.push({ fn, ms }); return -loadTimers.length }
      timers.push({ fn, ms })
      return timers.length
    },
    clearTimer: (handle: unknown) => { cleared.push(handle as number) },
    log: () => {},
    partitionName: () => `${KNOWLEDGE_VIEW_PARTITION_PREFIX}p${++seq}`,
    proxyPort: () => Promise.resolve(9),
    ...(over.ui ? { ui: over.ui } : {}),
  } as unknown as KnowledgeViewHostDeps
  const host = new KnowledgeViewHost(deps)
  return { host, views, sessions, states, timers, loadTimers, cleared, env, advance: (ms: number) => { now += ms } }
}

describe('dead proxy for WebRTC TCP (contract decision #30)', () => {
  it('sends everything but the exact allowed host:port to the dead proxy, loopback included, no DIRECT', () => {
    expect(knowledgeViewProxyConfig('https://kb.example.org', 9)).toEqual({
      mode: 'fixed_servers', proxyRules: 'http://127.0.0.1:9', proxyBypassRules: '<-loopback>;kb.example.org:443',
    })
    expect(knowledgeViewProxyConfig('http://127.0.0.1:18282', 9).proxyBypassRules).toBe('<-loopback>;127.0.0.1:18282')
    expect(knowledgeViewProxyConfig('http://[::1]:7', 9).proxyBypassRules).toBe('<-loopback>;[::1]:7')
    expect(knowledgeViewProxyConfig('https://kb.example.org:8443', 9).proxyBypassRules).toBe('<-loopback>;kb.example.org:8443')
    expect(JSON.stringify(knowledgeViewProxyConfig('https://kb.example.org', 9))).not.toMatch(/direct/i)
    // Never a pattern in the bypass list, whoever builds the origin.
    for (const bad of ['https://*', 'https://*.example.org', 'https://a;b.example.org', 'https://a,b.example.org']) {
      expect(() => knowledgeViewProxyConfig(bad, 9), bad).toThrow(/host name not allowed/)
    }
    expect(() => knowledgeViewProxyConfig('http://127.0.0.1:0', 9)).toThrow(/port 0/)
  })

  it('does not load a page whose first load already timed out while the proxy was being set', async () => {
    const t = setup()
    let release!: () => void
    const gate = new Promise<void>((resolve) => { release = resolve })
    const original = FakeSession.prototype.setProxy
    FakeSession.prototype.setProxy = function (this: FakeSession, config: unknown) { this.proxy = config; return gate }
    try {
      const shown = t.host.show(BOUNDS)
      await vi.waitFor(() => expect(t.loadTimers).toHaveLength(1))
      t.loadTimers[0].fn()
      expect(await shown).toEqual(expect.objectContaining({ ok: false }))
      release()
      await flush()
      expect(t.views[0].webContents.loads).toEqual([])
    } finally {
      FakeSession.prototype.setProxy = original
    }
  })

  it('does not load a page whose instance was replaced while the proxy was being set', async () => {
    const t = setup()
    const releases: Array<() => void> = []
    const original = FakeSession.prototype.setProxy
    FakeSession.prototype.setProxy = function (this: FakeSession, config: unknown) {
      this.proxy = config
      return new Promise<void>((resolve) => { releases.push(resolve) })
    }
    try {
      void t.host.show(BOUNDS)
      await vi.waitFor(() => expect(releases).toHaveLength(1))
      // A binding change while the first proxy is still pending: a second instance.
      t.env.binding = 'profile-2'
      t.host.bindingChanged()
      await vi.waitFor(() => expect(releases).toHaveLength(2))
      expect(t.views).toHaveLength(2)
      releases[0]()
      await flush()
      expect(t.views[0].webContents.loads).toEqual([])
      releases[1]()
      await vi.waitFor(() => expect(t.views[1].webContents.loads).toEqual([URL_]))
      await flush()
      expect(t.views[0].webContents.loads).toEqual([])
      expect(t.host.status()).toEqual({ state: 'ready' })
    } finally {
      FakeSession.prototype.setProxy = original
    }
  })

  it('loads the page only once the partition proxy is set', async () => {
    const t = setup()
    let release!: () => void
    const gate = new Promise<void>((resolve) => { release = resolve })
    const original = FakeSession.prototype.setProxy
    FakeSession.prototype.setProxy = function (this: FakeSession, config: unknown) { this.proxy = config; return gate }
    try {
      const shown = t.host.show(BOUNDS)
      await vi.waitFor(() => expect(t.sessions[0]?.proxy).toEqual(knowledgeViewProxyConfig(ORIGIN, 9)))
      await new Promise((d) => setTimeout(d, 5))
      expect(t.views[0].webContents.loads).toEqual([])
      release()
      await shown
      expect(t.views[0].webContents.loads).toEqual([URL_])
    } finally {
      FakeSession.prototype.setProxy = original
    }
  })

  it('keeps the page unloaded and reports a failed load when the proxy cannot be set', async () => {
    const t = setup()
    const original = FakeSession.prototype.setProxy
    FakeSession.prototype.setProxy = function (this: FakeSession, config: unknown) { this.proxy = config; return Promise.reject(new Error('no proxy')) }
    try {
      const result = await t.host.show(BOUNDS)
      expect(result.ok).toBe(false)
      expect(t.views[0].webContents.loads).toEqual([])
      expect(t.states.at(-1)).toMatchObject({ state: 'degraded', code: 'load_failed' })
    } finally {
      FakeSession.prototype.setProxy = original
    }
  })

  it('has a dead proxy that takes a connection and drops it', async () => {
    const port = await knowledgeViewBlackhole()
    expect(await knowledgeViewBlackhole()).toBe(port)
    // Accepted, then dropped with nothing sent: a port nobody listens on
    // (connection refused) would not pass.
    const seen = await new Promise<string[]>((resolve) => {
      const events: string[] = []
      const socket = net.connect(port, '127.0.0.1')
      socket.on('connect', () => events.push('connect'))
      socket.on('data', () => events.push('data'))
      socket.on('error', (error: NodeJS.ErrnoException) => events.push(`error:${error.code}`))
      socket.on('close', () => resolve(events))
    })
    expect(seen.filter((e) => e === 'connect' || e === 'data')).toEqual(['connect'])
    expect(seen[0]).toBe('connect')
  })

  it('keeps the dead proxy listening through errors once it is up', async () => {
    const logged: string[] = []
    const server = await listenDeadProxy((m) => logged.push(m))
    try {
      // A failed accept, twice: no uncaught error, the port stays ours.
      server.emit('error', new Error('EMFILE'))
      server.emit('error', new Error('EMFILE'))
      expect(logged).toEqual(['[knowledge-view] dead proxy: EMFILE', '[knowledge-view] dead proxy: EMFILE'])
      expect(server.listening).toBe(true)
    } finally {
      server.close()
    }
  })
})

describe('knowledgeViewTarget (U4.4)', () => {
  it('takes https, and http only to a loopback IP literal', () => {
    expect(knowledgeViewTarget(URL_)).toEqual({ origin: ORIGIN, url: URL_ })
    expect(knowledgeViewTarget('http://127.0.0.1:18282/')).toEqual({ origin: 'http://127.0.0.1:18282', url: 'http://127.0.0.1:18282/' })
    expect(knowledgeViewTarget('http://[::1]:9/x')?.origin).toBe('http://[::1]:9')
    // A fully qualified name keeps its root dot in the origin (Codex #1647 R1).
    expect(knowledgeViewTarget('https://kb.example.org./x')?.origin).toBe('https://kb.example.org.')
    expect(settingsKnowledgeViewTarget('https://kb.example.org./api/v1')?.origin).toBe('https://kb.example.org.')
    expect(knowledgeViewProxyConfig('https://kb.example.org.', 9).proxyBypassRules).toBe('<-loopback>;kb.example.org.:443')
    for (const bad of ['http://localhost:18282/', 'http://10.0.0.2/', 'file:///etc/passwd', 'agent24://x', 'https://u:p@kb.example.org/',
      'https://kb.example.org/?token=x', 'https://kb.example.org/#t', 'not a url', 42, undefined,
      // Host names the URL parser keeps but the proxy bypass would read as patterns (#1643 PR-Daemon C).
      'https://*.example.org/', 'https://a;b.example.org/', 'https://a,*,b.1.2.3.4.sslip.io/', 'https://kb_x.example.org/',
      // Port 0 in the bypass list is any port (#1649 PR-Daemon L1).
      'https://kb.example.org:0/', 'http://127.0.0.1:0/']) {
      expect(knowledgeViewTarget(bad), String(bad)).toBeNull()
    }
  })

  it('derives the page from the settings endpoint: its origin plus the fixed embed path (#1625)', () => {
    expect(settingsKnowledgeViewTarget('https://kb.example.org/api/v1')).toEqual({ origin: ORIGIN, url: `${ORIGIN}/embed-admin/knowledge-bases` })
    expect(settingsKnowledgeViewTarget('http://127.0.0.1:18282/api/v1')).toEqual({
      origin: 'http://127.0.0.1:18282', url: 'http://127.0.0.1:18282/embed-admin/knowledge-bases',
    })
    expect(settingsKnowledgeViewTarget('http://[::1]:9000')?.url).toBe('http://[::1]:9000/embed-admin/knowledge-bases')
    for (const bad of ['', 'http://10.0.0.2/api/v1', 'http://localhost:18282/api/v1', 'https://u:p@kb.example.org/api/v1', 'file:///x', 'nope', undefined, 7]) {
      expect(settingsKnowledgeViewTarget(bad)).toBeNull()
    }
  })

  it('the development variable overrides the settings only outside a packaged build', () => {
    const env = { [KNOWLEDGE_VIEW_DEV_URL_ENV]: 'http://127.0.0.1:5000/embed-admin/knowledge-bases' }
    expect(resolveKnowledgeViewTarget(env, false, 'https://kb.example.org/api/v1')?.origin).toBe('http://127.0.0.1:5000')
    expect(resolveKnowledgeViewTarget(env, true, 'https://kb.example.org/api/v1')?.origin).toBe(ORIGIN)
    expect(resolveKnowledgeViewTarget({}, false, 'https://kb.example.org/api/v1')?.origin).toBe(ORIGIN)
    expect(resolveKnowledgeViewTarget({}, true, '')).toBeNull()
  })

  it('reads the development variable only when not packaged', () => {
    const env = { [KNOWLEDGE_VIEW_DEV_URL_ENV]: URL_ }
    expect(devKnowledgeViewTarget(env, false)?.url).toBe(URL_)
    expect(devKnowledgeViewTarget(env, true)).toBeNull()
    expect(devKnowledgeViewTarget({}, false)).toBeNull()
  })
})

describe('origin policy and CSP (U4.2)', () => {
  it('allows documents and requests only at the exact origin', () => {
    expect(isAllowedDocument(`${ORIGIN}/a`, ORIGIN)).toBe(true)
    expect(isAllowedDocument('https://kb.example.org:444/a', ORIGIN)).toBe(false)
    expect(isAllowedDocument('https://evil.example.org/', ORIGIN)).toBe(false)
    expect(isAllowedDocument('file:///x', ORIGIN)).toBe(false)
    expect(isAllowedRequest('wss://kb.example.org/ws', ORIGIN)).toBe(true)
    expect(isAllowedRequest('ws://kb.example.org/ws', ORIGIN)).toBe(false)
    expect(isAllowedRequest('ws://127.0.0.1:9/ws', 'http://127.0.0.1:9')).toBe(true)
    expect(isAllowedRequest('https://cdn.example.org/x.js', ORIGIN)).toBe(false)
    expect(isAllowedRequest('agent24://x', ORIGIN)).toBe(false)
    expect(isAllowedRequest('garbage', ORIGIN)).toBe(false)
  })

  it('denies frames, workers, objects and <base>, and pins everything else to the origin', () => {
    const csp = knowledgeViewCsp(ORIGIN)
    for (const d of ["default-src 'none'", "frame-src 'none'", "child-src 'none'", "worker-src 'none'", "object-src 'none'",
      "base-uri 'none'", "frame-ancestors 'none'", `form-action ${ORIGIN}`, `connect-src ${ORIGIN} wss://kb.example.org`, `script-src ${ORIGIN}`]) {
      expect(csp).toContain(d)
    }
    expect(csp).not.toMatch(/unsafe-eval|\*/)
    // A loopback IPv4 origin stays an explicit source.
    expect(knowledgeViewCsp('http://127.0.0.1:18282')).toContain('script-src http://127.0.0.1:18282;')
  })

  it("uses 'self' for an IPv6 origin, which Chromium's CSP parser cannot take as a host (Codex #1654 R1)", () => {
    const csp = knowledgeViewCsp('http://[::1]:8443')
    expect(csp).not.toContain('[')
    for (const d of ["script-src 'self'", "style-src 'self' 'unsafe-inline'", "img-src 'self' data: blob:", "font-src 'self' data:",
      "connect-src 'self'", "media-src 'self'", "manifest-src 'self'", "form-action 'self'", "default-src 'none'", "frame-src 'none'",
      "worker-src 'none'", "frame-ancestors 'none'"]) {
      expect(csp, d).toContain(d)
    }
    expect(csp).not.toMatch(/unsafe-eval|\*|ws:/)
    expect(knowledgeViewCsp('https://[2001:db8::1]')).toContain("script-src 'self';")
  })

  it('adds its CSP next to the server one, never in place of it', () => {
    const out = withKnowledgeCsp({ 'content-security-policy': ["script-src 'self'"], 'X-A': ['1'] }, ORIGIN)
    expect(out['Content-Security-Policy']).toEqual(["script-src 'self'", knowledgeViewCsp(ORIGIN)])
    expect(out['content-security-policy']).toBeUndefined()
    expect(out['X-A']).toEqual(['1'])
  })

  it('switches powerful features off in the page, keeping only the server switch-offs', () => {
    const headers = { 'permissions-policy': ['geolocation=*, fullscreen=()'], 'Permissions-Policy': ['camera=('], 'X-A': ['1'] }
    const out = withKnowledgeCsp(headers, ORIGIN)
    // A malformed or granting server member must not go out: only the
    // server's plain switch-offs pass (fullscreen here), then ours.
    expect(out['Permissions-Policy']).toEqual(['fullscreen=()', KNOWLEDGE_VIEW_PERMISSIONS_POLICY])
    // A server that only switches more off keeps those, ahead of ours.
    expect(withKnowledgeCsp({ 'Permissions-Policy': ['sync-xhr=(), fullscreen=()', 'autoplay=()'] }, ORIGIN)['Permissions-Policy'])
      .toEqual(['sync-xhr=(), fullscreen=(), autoplay=()', KNOWLEDGE_VIEW_PERMISSIONS_POLICY])
    expect(withKnowledgeCsp({ 'Permissions-Policy': ['sync-xhr=(), geolocation=(self)'] }, ORIGIN)['Permissions-Policy'])
      .toEqual(['sync-xhr=()', KNOWLEDGE_VIEW_PERMISSIONS_POLICY])
    expect(withKnowledgeCsp({ 'Permissions-Policy': ['geolocation=*'] }, ORIGIN)['Permissions-Policy'])
      .toEqual([KNOWLEDGE_VIEW_PERMISSIONS_POLICY])
    // A member is kept only when it is a whole `feature=()`: text before or
    // after it would make Blink drop the merged field, ours included.
    expect(withKnowledgeCsp({ 'Permissions-Policy': ['camera=()*, x=()"', '*camera=()'] }, ORIGIN)['Permissions-Policy'])
      .toEqual([KNOWLEDGE_VIEW_PERMISSIONS_POLICY])
    expect(out['permissions-policy']).toBeUndefined()
    expect(headers['permissions-policy']).toEqual(['geolocation=*, fullscreen=()'])
    for (const feature of ['geolocation', 'camera', 'microphone', 'display-capture', 'usb', 'serial', 'hid', 'bluetooth']) {
      expect(KNOWLEDGE_VIEW_PERMISSIONS_POLICY).toContain(`${feature}=()`)
    }
    expect(KNOWLEDGE_VIEW_PERMISSIONS_POLICY).not.toMatch(/\*|self/)
    expect(withKnowledgeCsp(undefined, ORIGIN)['Permissions-Policy']).toEqual([KNOWLEDGE_VIEW_PERMISSIONS_POLICY])
  })

  it('has no Node, no preload, sandbox on, and DevTools only in development', () => {
    const prefs = knowledgeViewPreferences('agent24-knowledge-v1:x', true)
    expect(prefs).toMatchObject({ contextIsolation: true, nodeIntegration: false, sandbox: true, webviewTag: false, devTools: false })
    expect(prefs).not.toHaveProperty('preload')
    expect(knowledgeViewPreferences('agent24-knowledge-v1:x', false).devTools).toBe(true)
  })

  it('accepts only numeric bounds', () => {
    expect(normalizeKnowledgeViewBounds(BOUNDS)).toEqual({ x: 10, y: 20, width: 800, height: 600 })
    expect(normalizeKnowledgeViewBounds({ x: -5, y: 0, width: 0, height: 2 })).toEqual({ x: 0, y: 0, width: 1, height: 2 })
    expect(normalizeKnowledgeViewBounds({ x: '1', y: 0, width: 1, height: 1 })).toBeNull()
    expect(normalizeKnowledgeViewBounds({ x: Infinity, y: 0, width: 1, height: 1 })).toBeNull()
    expect(normalizeKnowledgeViewBounds(null)).toBeNull()
  })
})

describe('KnowledgeViewHost', () => {
  it('opens in the same window, at the bounds, in a fresh non-persistent partition', async () => {
    const t = setup()
    expect(await t.host.show(BOUNDS, 'light')).toEqual({ ok: true })
    const [view] = t.views
    expect(t.env.win!.children).toEqual([view])
    expect(view.bounds).toEqual({ x: 10, y: 20, width: 800, height: 600 })
    expect(view.visible).toBe(true)
    expect(view.background).toBe('#f4f4ef')
    expect(view.webContents.loads).toEqual([URL_])
    const partition = view.prefs.partition as string
    expect(partition.startsWith(KNOWLEDGE_VIEW_PARTITION_PREFIX)).toBe(true)
    expect(partition).not.toMatch(/^persist:/)
    expect(partition).not.toContain('kb.example.org')
    expect(t.sessions[0].partition).toBe(partition)
  })

  it('random partition names carry nothing of the binding', () => {
    const host = new KnowledgeViewHost({
      window: () => null, createView: () => { throw new Error('no') }, sessionFor: () => { throw new Error('no') },
      target: () => null, binding: () => null, packaged: true,
    })
    const name = (host as unknown as { partitionName: () => string }).partitionName()
    expect(name).toMatch(/^agent24-knowledge-v1:[A-Za-z0-9_-]{22}$/)
  })

  it('refuses popups, off-origin navigation, subframes, downloads, permissions, devices, auth and certificates', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    const wc = t.views[0].webContents
    const ses = t.sessions[0]
    expect(wc.openHandler!({ url: `${ORIGIN}/x` })).toEqual({ action: 'deny' })
    expect(wc.openHandler!({ url: 'https://evil.example.org/' })).toEqual({ action: 'deny' })
    expect(wc.webrtc).toBe('disable_non_proxied_udp')

    for (const [url, blocked] of [[`${ORIGIN}/b`, false], ['https://evil.example.org/', true], ['file:///etc/hosts', true], ['agent24://x', true]] as const) {
      const e = prevented()
      wc.emit('will-navigate', e, url)
      expect(e.prevented).toBe(blocked)
    }
    const sub = prevented({ url: `${ORIGIN}/frame`, isMainFrame: false })
    wc.emit('will-frame-navigate', sub)
    expect(sub.prevented).toBe(true)
    const redirect = prevented({ url: 'https://evil.example.org/', isMainFrame: true })
    wc.emit('will-redirect', redirect)
    expect(redirect.prevented).toBe(true)

    const attach = prevented()
    wc.emit('will-attach-webview', attach)
    expect(attach.prevented).toBe(true)
    const cert = prevented(); const certCb = vi.fn()
    wc.emit('select-client-certificate', cert, URL_, [{}], certCb)
    expect(cert.prevented).toBe(true)
    expect(certCb).toHaveBeenCalledWith()
    const login = prevented(); const loginCb = vi.fn()
    wc.emit('login', login, {}, {}, loginCb)
    expect(login.prevented).toBe(true)
    expect(loginCb).toHaveBeenCalledWith()
    const bt = prevented(); const btCb = vi.fn()
    wc.emit('select-bluetooth-device', bt, [{ deviceId: 'd' }], btCb)
    expect(btCb).toHaveBeenCalledWith('')

    const dl = prevented(); const item = { cancel: vi.fn() }
    ses.emit('will-download', dl, item)
    expect(dl.prevented).toBe(true)
    expect(item.cancel).toHaveBeenCalled()
    const granted = vi.fn()
    ses.permissionRequest!({}, 'media', granted)
    expect(granted).toHaveBeenCalledWith(false)
    expect(ses.permissionCheck!()).toBe(false)
    expect(ses.devicePermission!()).toBe(false)
    for (const event of ['select-hid-device', 'select-usb-device']) {
      const e = prevented(); const cb = vi.fn()
      ses.emit(event, e, {}, cb)
      expect(e.prevented).toBe(true)
      expect(cb).toHaveBeenCalledWith()
    }
    const serial = prevented(); const serialCb = vi.fn()
    ses.emit('select-serial-port', serial, [{ portId: 'p' }], {}, serialCb)
    expect(serialCb).toHaveBeenCalledWith('')

    expect(ses.allows(`${ORIGIN}/api/x`)).toBe(true)
    expect(ses.allows('https://evil.example.org/x')).toBe(false)
    let headers: Record<string, string[]> | null = null
    ses.headersReceived!({ responseHeaders: {} }, (r: { responseHeaders: Record<string, string[]> }) => { headers = r.responseHeaders })
    expect(headers!['Content-Security-Policy']).toEqual([knowledgeViewCsp(ORIGIN)])
  })

  it('removes a Service Worker that registers anyway (U4.3)', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    t.sessions[0].serviceWorkers.emit('registration-completed')
    expect(t.sessions[0].cleared).toContain('storage:serviceworkers,cachestorage')
  })

  it('hiding keeps the instance and its session; showing again does not reload', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    t.host.hide()
    expect(t.views[0].visible).toBe(false)
    expect(t.sessions[0].cleared).toEqual([])
    await t.host.show(BOUNDS)
    expect(t.views).toHaveLength(1)
    expect(t.views[0].webContents.loads).toHaveLength(1)
    expect(t.views[0].visible).toBe(true)
  })

  it('a binding change ends the instance, clears its partition, and the next one is a new partition', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    t.host.hide()
    t.env.binding = 'https://other.example.org/api/v1'
    t.host.bindingChanged()
    expect(t.env.win!.children).toEqual([])
    expect(t.views[0].webContents.destroyed).toBe(true)
    await flush()
    expect(t.sessions[0].cleared).toEqual(ALL_CLEARED)
    await t.host.show(BOUNDS)
    expect(t.views).toHaveLength(2)
    expect(t.views[1].prefs.partition).not.toBe(t.views[0].prefs.partition)
  })

  it('a binding change while on screen opens the new binding at once, never a blank area', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    t.env.binding = 'https://other.example.org/api/v1'
    t.host.bindingChanged()
    expect(t.views[0].webContents.destroyed).toBe(true)
    expect(t.env.win!.children).toEqual([t.views[1]])
    expect(t.views[1].visible).toBe(true)
    await flush()
    expect(t.host.status()).toEqual({ state: 'ready' })
  })

  it('Off then On again while on screen reopens the page', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    t.env.binding = null
    t.host.bindingChanged()
    expect(t.host.status()).toEqual({ state: 'unavailable', code: 'knowledge_off' })
    t.env.binding = 'https://kb.example.org/api/v1'
    t.host.bindingChanged()
    await flush()
    expect(t.host.status()).toEqual({ state: 'ready' })
    expect(t.views).toHaveLength(2)
    expect(t.views[1].visible).toBe(true)
  })

  it('a settings change that keeps the binding does not skip a crash backoff', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    t.views[0].webContents.emit('render-process-gone', {}, { reason: 'crashed' })
    expect(t.timers).toHaveLength(1)
    t.host.bindingChanged()
    expect(t.views).toHaveLength(1)
    t.timers[0].fn()
    await flush()
    expect(t.views).toHaveLength(2)
  })

  it('a settings change while hidden creates nothing', async () => {
    const t = setup()
    t.env.binding = null
    t.host.bindingChanged()
    t.env.binding = 'https://kb.example.org/api/v1'
    t.host.bindingChanged()
    expect(t.views).toHaveLength(0)
  })

  it('Off while on screen says Off at once (U4.8)', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    t.env.binding = null
    t.host.bindingChanged()
    expect(t.host.status()).toEqual({ state: 'unavailable', code: 'knowledge_off' })
    expect(t.views).toHaveLength(1)
  })

  it('an unchanged binding keeps the instance', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    t.host.bindingChanged()
    expect(t.views[0].webContents.destroyed).toBe(false)
  })

  it('Off destroys the instance and creates none (U4.8)', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    t.env.binding = null
    t.host.bindingChanged()
    expect(t.views[0].webContents.destroyed).toBe(true)
    expect(await t.host.show(BOUNDS)).toEqual({ ok: false, code: 'knowledge_off' })
    expect(t.views).toHaveLength(1)
    expect(t.host.status()).toEqual({ state: 'unavailable', code: 'knowledge_off' })
  })

  it('no address configured: no view', async () => {
    const t = setup({ target: null })
    expect(await t.host.show(BOUNDS)).toEqual({ ok: false, code: 'not_configured' })
    expect(t.views).toHaveLength(0)
  })

  it('a new window gets a new instance; the old one is destroyed', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    const first = t.env.win!
    t.env.win = new FakeWindow()
    await t.host.show(BOUNDS)
    expect(first.children).toEqual([])
    expect(t.views[0].webContents.destroyed).toBe(true)
    expect(t.env.win.children).toEqual([t.views[1]])
  })

  it('app quit destroys everything', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    t.host.shutdown()
    expect(t.views[0].webContents.destroyed).toBe(true)
    await flush()
    expect(t.sessions[0].cleared).toEqual(ALL_CLEARED)
    // ADR §4: no new instance once quitting, whatever the renderer still sends.
    expect(await t.host.show(BOUNDS)).toEqual({ ok: false, code: 'window_unavailable' })
    expect(await t.host.reload(BOUNDS)).toEqual({ ok: false, code: 'window_unavailable' })
    expect(t.views).toHaveLength(1)
  })

  it('seals the partition first and clears it only after the page is destroyed (U4.7)', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    const wc = t.views[0].webContents
    const ses = t.sessions[0]
    wc.asyncClose = true
    t.env.binding = null
    t.host.bindingChanged()
    expect(wc.closed).toBe(true)
    // Sealed: the closing page's late requests are all cancelled, even to the allowed origin.
    expect(ses.allows(`${ORIGIN}/keepalive`)).toBe(false)
    await flush()
    expect(ses.cleared).toEqual([])
    wc.finishClose()
    await flush()
    expect(ses.cleared).toEqual(ALL_CLEARED)
  })

  it('a page not destroyed in time has its renderer killed; the partition is cleared once it is gone', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    const wc = t.views[0].webContents
    wc.asyncClose = true
    t.host.destroyAll('test')
    const waits = () => t.timers.filter((x) => x.ms === KNOWLEDGE_VIEW_DESTROY_WAIT_MS)
    expect(waits()).toHaveLength(1)
    waits()[0].fn()
    await flush()
    expect(wc.crashedByHost).toBe(true)
    // U4.7 ④ before ⑤: nothing is cleared while the page may still be there.
    expect(t.sessions[0].cleared).toEqual([])
    wc.finishClose()
    await flush()
    expect(t.sessions[0].cleared).toEqual(ALL_CLEARED)
  })

  it('a page that survives even the kill: the sealed partition is cleared, and again if it ever goes', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    const wc = t.views[0].webContents
    wc.asyncClose = true
    t.host.destroyAll('test')
    t.timers.filter((x) => x.ms === KNOWLEDGE_VIEW_DESTROY_WAIT_MS)[0].fn()
    await flush()
    t.timers.filter((x) => x.ms === KNOWLEDGE_VIEW_DESTROY_WAIT_MS)[1].fn()
    await flush()
    expect(t.sessions[0].cleared).toEqual(ALL_CLEARED)
    wc.finishClose()
    await flush()
    expect(t.sessions[0].cleared).toEqual([...ALL_CLEARED, ...ALL_CLEARED])
  })

  it('a closed window takes its view down even if nothing shows it again', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    t.env.win!.emit('closed')
    expect(t.views[0].webContents.destroyed).toBe(true)
  })

  it('an unresponsive page steps aside with a typed state and comes back when it recovers', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    const wc = t.views[0].webContents
    wc.emit('unresponsive')
    expect(t.views[0].visible).toBe(false)
    expect(t.host.status()).toEqual({ state: 'degraded', code: 'unresponsive' })
    wc.emit('responsive')
    expect(t.views[0].visible).toBe(true)
    expect(t.host.status()).toEqual({ state: 'ready' })
    // Recovery ends a hang, not a load failure that happened meanwhile.
    wc.emit('unresponsive')
    wc.emit('did-fail-load', {}, -102, 'refused', URL_, true)
    wc.emit('responsive')
    expect(t.views[0].visible).toBe(false)
    expect(t.host.status()).toEqual({ state: 'degraded', code: 'load_failed' })
    expect(await t.host.show(BOUNDS)).toEqual({ ok: true })
    wc.emit('unresponsive')
    // A route round trip is not a recovery: the hung page stays out of the way.
    t.host.hide()
    expect(await t.host.show(BOUNDS)).toEqual({ ok: true })
    expect(t.views[0].visible).toBe(false)
    expect(t.host.status()).toEqual({ state: 'degraded', code: 'unresponsive' })
    expect(await t.host.reload(BOUNDS)).toEqual({ ok: true })
    expect(wc.destroyed).toBe(true)
    expect(t.views).toHaveLength(2)
  })

  it('a main frame that commits off-origin ends the instance', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    t.views[0].webContents.emit('did-navigate', {}, 'https://evil.example.org/')
    expect(t.views[0].webContents.destroyed).toBe(true)
    expect(t.host.status()).toEqual({ state: 'degraded', code: 'navigated_away' })
  })

  it('stops at 64 partitions per process (U4.6)', async () => {
    const t = setup()
    for (let i = 0; i < MAX_KNOWLEDGE_VIEW_PARTITIONS; i++) {
      expect(await t.host.reload(BOUNDS)).toEqual({ ok: true })
    }
    expect(await t.host.reload(BOUNDS)).toEqual({ ok: false, code: 'partition_limit' })
    expect(t.views).toHaveLength(MAX_KNOWLEDGE_VIEW_PARTITIONS)
  })

  it('a hide during the load wins over the late load result', async () => {
    const t = setup()
    let finish: () => void = () => {}
    { const pending = new Promise<void>((resolve) => { finish = resolve }); nextLoad = () => pending }
    try {
      const shown = t.host.show(BOUNDS)
      t.host.hide()
      finish()
      expect(await shown).toEqual({ ok: false, code: 'superseded' })
      expect(t.views[0].visible).toBe(false)
    } finally {
      nextLoad = () => Promise.resolve()
    }
    // The load was not thrown away: the next show uses it, no second navigation.
    expect(await t.host.show(BOUNDS)).toEqual({ ok: true })
    expect(t.views[0].webContents.loads).toHaveLength(1)
    expect(t.host.status()).toEqual({ state: 'ready' })
  })

  it('a second show during the first load waits for it instead of loading again', async () => {
    const t = setup()
    let finish: () => void = () => {}
    { const pending = new Promise<void>((resolve) => { finish = resolve }); nextLoad = () => pending }
    try {
      const first = t.host.show(BOUNDS)
      const second = t.host.show(BOUNDS)
      finish()
      expect(await second).toEqual({ ok: true })
      expect(await first).toEqual({ ok: false, code: 'superseded' })
    } finally {
      nextLoad = () => Promise.resolve()
    }
    expect(t.views[0].webContents.loads).toHaveLength(1)
  })

  it('did-finish-load does not cover a hung page or a failed load with ready', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    const wc = t.views[0].webContents
    wc.emit('unresponsive')
    wc.emit('did-finish-load')
    expect(t.host.status()).toEqual({ state: 'degraded', code: 'unresponsive' })
    wc.emit('responsive')
    expect(t.host.status()).toEqual({ state: 'ready' })
    wc.emit('did-fail-load', {}, -102, 'refused', URL_, true)
    wc.emit('did-finish-load')
    expect(t.host.status()).toEqual({ state: 'degraded', code: 'load_failed' })
    expect(t.views[0].visible).toBe(false)
  })

  it('events from a replaced instance are ignored (crash, hang)', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    const old = t.views[0].webContents
    await t.host.reload(BOUNDS)
    expect(t.views).toHaveLength(2)
    old.emit('render-process-gone', {}, { reason: 'crashed' })
    old.emit('unresponsive')
    expect(t.host.status()).toEqual({ state: 'ready' })
    expect(t.timers).toHaveLength(0)
    expect(t.views[1].visible).toBe(true)
  })

  it('a pending crash rebuild is cancelled when the instance is destroyed', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    t.views[0].webContents.emit('render-process-gone', {}, { reason: 'crashed' })
    expect(t.timers).toHaveLength(1)
    t.host.destroyAll('test')
    expect(t.cleared).toContain(1)
  })

  it('destroying an instance removes its listener from the window', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    expect(t.env.win!.listeners.get('closed')).toHaveLength(1)
    await t.host.reload(BOUNDS)
    expect(t.env.win!.listeners.get('closed')).toHaveLength(1)
  })

  it('a main-frame load failure reported later also steps the view aside', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    t.views[0].webContents.emit('did-fail-load', {}, -3, 'aborted', URL_, true)
    expect(t.views[0].visible).toBe(true)
    t.views[0].webContents.emit('did-fail-load', {}, -102, 'refused', `${ORIGIN}/sub`, false)
    expect(t.views[0].visible).toBe(true)
    t.views[0].webContents.emit('did-fail-load', {}, -102, 'refused', URL_, true)
    expect(t.views[0].visible).toBe(false)
    expect(t.host.status()).toEqual({ state: 'degraded', code: 'load_failed' })
    expect(await t.host.show(BOUNDS)).toEqual({ ok: true })
    expect(t.views[0].webContents.loads).toHaveLength(2)
  })

  it('a first load that never settles fails after the limit and can be retried (PR-Daemon #1634 F1)', async () => {
    const t = setup()
    nextLoad = () => new Promise<void>(() => {})
    let shown: Promise<unknown>
    try {
      shown = t.host.show(BOUNDS)
      t.host.hide()
      // Coming back does not start a second load; the first one is still waited for.
      void t.host.show(BOUNDS)
      await flush() // the page loads once the partition proxy is set (decision #30)
      expect(t.views[0].webContents.loads).toHaveLength(1)
      expect(t.loadTimers.map((x) => x.ms)).toEqual([KNOWLEDGE_VIEW_FIRST_LOAD_TIMEOUT_MS])
      t.loadTimers[0].fn()
      await flush()
      expect(t.views[0].webContents.stopped).toBe(1)
      expect(t.host.status()).toEqual({ state: 'degraded', code: 'load_failed' })
      expect(t.views[0].visible).toBe(false)
      expect(await shown).toEqual({ ok: false, code: 'superseded' })
    } finally {
      nextLoad = () => Promise.resolve()
    }
    expect(await t.host.show(BOUNDS)).toEqual({ ok: true })
    expect(t.views[0].webContents.loads).toHaveLength(2)
  })

  it('a load that settles in time cancels its timeout', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    expect(t.cleared).toContain(-1)
  })

  it('a 4xx/5xx main-frame answer is a failed load with Reload, not ready', async () => {
    const t = setup()
    let finish: () => void = () => {}
    { const pending = new Promise<void>((resolve) => { finish = resolve }); nextLoad = () => pending }
    try {
      const shown = t.host.show(BOUNDS)
      t.views[0].webContents.emit('did-navigate', {}, URL_, 502, 'Bad Gateway')
      finish()
      expect(await shown).toEqual({ ok: false, code: 'load_failed' })
    } finally {
      nextLoad = () => Promise.resolve()
    }
    expect(t.host.status()).toEqual({ state: 'degraded', code: 'load_failed' })
    expect(t.views[0].visible).toBe(false)
    // Retried on the next show; a 200 this time is ready.
    const again = t.host.show(BOUNDS)
    t.views[0].webContents.emit('did-navigate', {}, URL_, 200, 'OK')
    expect(await again).toEqual({ ok: true })
    // A later in-page navigation that lands on an error page fails too.
    t.views[0].webContents.emit('did-navigate', {}, `${ORIGIN}/embed-admin/knowledge-bases/missing`, 404, 'Not Found')
    expect(t.host.status()).toEqual({ state: 'degraded', code: 'load_failed' })
  })

  it('a reopen that throws becomes a typed state, not an unhandled rejection or a blank area (F5)', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    t.env.binding = 'https://other.example.org/api/v1'
    const host = t.host as unknown as { create: () => never }
    host.create = () => { throw new Error('boom') }
    t.host.bindingChanged()
    await flush()
    expect(t.host.status()).toEqual({ state: 'degraded', code: 'load_failed' })
  })

  it('a failed load is reported and retried on the next show', async () => {
    const t = setup()
    nextLoad = () => Promise.reject(new Error('ERR_CONNECTION_REFUSED'))
    try {
      expect(await t.host.show(BOUNDS)).toEqual({ ok: false, code: 'load_failed' })
      expect(t.host.status()).toEqual({ state: 'degraded', code: 'load_failed' })
      // Out of the way of the page's message and Reload button.
      expect(t.views[0].visible).toBe(false)
    } finally {
      nextLoad = () => Promise.resolve()
    }
    expect(await t.host.show(BOUNDS)).toEqual({ ok: true })
    expect(t.views).toHaveLength(1)
    expect(t.views[0].webContents.loads).toHaveLength(2)
  })

  it('rebuilds after a crash with backoff, and closes on a crash loop (U4.8)', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    for (let i = 1; i <= 3; i++) {
      t.views[t.views.length - 1].webContents.emit('render-process-gone', {}, { reason: 'crashed' })
      expect(t.host.status()).toEqual({ state: 'degraded', code: 'crashed' })
      expect(t.timers).toHaveLength(i)
      t.timers[i - 1].fn()
      await Promise.resolve()
      expect(t.views).toHaveLength(i + 1)
      t.advance(1_000)
    }
    expect(t.timers.map((x) => x.ms)).toEqual([1_000, 2_000, 4_000])
    t.views[3].webContents.emit('render-process-gone', {}, { reason: 'crashed' })
    expect(t.host.status()).toEqual({ state: 'closed', code: 'crash_loop' })
    expect(await t.host.show(BOUNDS)).toEqual({ ok: false, code: 'crash_loop' })
    expect(t.views).toHaveLength(4)
    // The user's reload is the way out.
    expect(await t.host.reload(BOUNDS)).toEqual({ ok: true })
    expect(t.views).toHaveLength(5)
  })

  it('a crash loop belongs to its binding: another binding or Off ends it (§9.2)', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    for (let i = 0; i < 4; i++) {
      t.views[t.views.length - 1].webContents.emit('render-process-gone', {}, { reason: 'crashed' })
      if (i < 3) { t.timers[t.timers.length - 1].fn(); await Promise.resolve() }
    }
    expect(t.host.status()).toEqual({ state: 'closed', code: 'crash_loop' })
    t.env.binding = 'https://other.example.org/api/v1'
    t.host.bindingChanged()
    // On screen, so the new binding's page opens straight away.
    await flush()
    expect(t.host.status()).toEqual({ state: 'ready' })
    expect(await t.host.show(BOUNDS)).toEqual({ ok: true })
    // The new binding starts with no crashes counted: one crash is a rebuild, not closed.
    t.views[t.views.length - 1].webContents.emit('render-process-gone', {}, { reason: 'crashed' })
    expect(t.host.status()).toEqual({ state: 'degraded', code: 'crashed' })
  })

  it('Off says Off, not the old crash loop', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    for (let i = 0; i < 4; i++) {
      t.views[t.views.length - 1].webContents.emit('render-process-gone', {}, { reason: 'crashed' })
      if (i < 3) { t.timers[t.timers.length - 1].fn(); await Promise.resolve() }
    }
    t.env.binding = null
    expect(await t.host.show(BOUNDS)).toEqual({ ok: false, code: 'knowledge_off' })
    t.env.binding = 'https://kb.example.org/api/v1'
    expect(await t.host.show(BOUNDS)).toEqual({ ok: true })
  })

  it('crashes outside the ten-minute window do not count', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    for (let i = 0; i < 5; i++) {
      t.views[t.views.length - 1].webContents.emit('render-process-gone', {}, { reason: 'crashed' })
      expect(t.host.status()).toEqual({ state: 'degraded', code: 'crashed' })
      t.timers[t.timers.length - 1].fn()
      await Promise.resolve()
      t.advance(11 * 60_000)
    }
  })

  it('rejects bounds that are not numbers', async () => {
    const t = setup()
    await expect(t.host.show({ x: 'a' })).rejects.toThrow('invalid bounds')
    expect(() => t.host.setBounds(null)).toThrow('invalid bounds')
    expect(t.views).toHaveLength(0)
  })
})

describe('UI session wiring (KB-C2-04a, U4.5, U4.7, U3.8)', () => {
  interface Made {
    context: KnowledgeViewUiContext
    destroyed: boolean
    injected: Array<{ request: Record<string, unknown>; webContentsId: number }>
    responded: Array<[number, number | undefined]>
  }
  function fakeUi(start: (context: KnowledgeViewUiContext, made: Made) => Promise<boolean> = async () => true, log: string[] = []) {
    const made: Made[] = []
    let decision: { cancel: true } | { cancel: false; requestHeaders: Record<string, string>; injected: boolean } = { cancel: true }
    const factory = (context: KnowledgeViewUiContext): KnowledgeViewUi => {
      const m: Made = { context, destroyed: false, injected: [], responded: [] }
      made.push(m)
      return {
        start: () => start(context, m),
        inject: (request, webContentsId) => { m.injected.push({ request: request as unknown as Record<string, unknown>, webContentsId }); return decision },
        responded: (id, status) => { m.responded.push([id, status]) },
        destroy: () => { m.destroyed = true; log.push('ui destroyed') },
        bridgeAdmission: () => ({ open: true, generation: 1 }),
      }
    }
    return { factory, made, decide: (d: typeof decision) => { decision = d } }
  }

  it('loads the page only once the first JWT is there', async () => {
    let release!: (ok: boolean) => void
    const ui = fakeUi(() => new Promise<boolean>((resolve) => { release = resolve }))
    const t = setup({ ui: ui.factory })
    const shown = t.host.show(BOUNDS)
    await vi.waitFor(() => expect(ui.made).toHaveLength(1))
    await flush()
    expect(t.views[0].webContents.loads).toEqual([])
    release(true)
    expect(await shown).toEqual({ ok: true })
    expect(t.views[0].webContents.loads).toEqual([URL_])
    expect(ui.made[0].context).toMatchObject({ partition: t.sessions[0].partition, binding: 'https://kb.example.org/api/v1', origin: ORIGIN })
  })

  it('does not load without a session, and keeps the state the session event set', async () => {
    const ui = fakeUi(async (context) => { context.onEvent({ kind: 'closed', code: 'empty_scope' }); return false })
    const t = setup({ ui: ui.factory })
    expect(await t.host.show(BOUNDS)).toEqual({ ok: false, code: 'superseded' })
    expect(t.views[0].webContents.loads).toEqual([])
    expect(t.host.status()).toEqual({ state: 'closed', code: 'empty_scope' })
    expect(ui.made[0].destroyed).toBe(true)
    // Closed stays closed for this binding until the way out.
    expect(await t.host.show(BOUNDS)).toEqual({ ok: false, code: 'empty_scope' })
    expect(ui.made).toHaveLength(1)
    await t.host.reload(BOUNDS)
    expect(ui.made).toHaveLength(2)
  })

  it('never loads after a failed start, even one that sent no event', async () => {
    const ui = fakeUi(async () => false)
    const t = setup({ ui: ui.factory })
    expect(await t.host.show(BOUNDS)).toEqual(expect.objectContaining({ ok: false }))
    expect(t.views[0].webContents.loads).toEqual([])
  })

  it('a session that cannot be had steps aside with Reload, without reopening by itself', async () => {
    const ui = fakeUi(async (context) => { context.onEvent({ kind: 'unavailable' }); return false })
    const t = setup({ ui: ui.factory })
    await t.host.show(BOUNDS)
    expect(t.host.status()).toEqual({ state: 'degraded', code: 'session_unavailable' })
    expect(ui.made).toHaveLength(1)
    expect(ui.made[0].destroyed).toBe(true)
  })

  it('rebuilds on a view-level event: new instance, new partition, same binding ledger; a new binding gets a new ledger', async () => {
    const ui = fakeUi()
    const t = setup({ ui: ui.factory })
    await t.host.show(BOUNDS)
    ui.made[0].context.onEvent({ kind: 'rebuild' })
    await vi.waitFor(() => expect(ui.made).toHaveLength(2))
    await flush()
    expect(ui.made[0].destroyed).toBe(true)
    expect(t.sessions[1].partition).not.toBe(t.sessions[0].partition)
    expect(ui.made[1].context.ledger).toBe(ui.made[0].context.ledger)
    expect(t.states).not.toContainEqual({ state: 'degraded', code: 'crashed' })
    t.env.binding = 'profile-2'
    t.host.bindingChanged()
    await vi.waitFor(() => expect(ui.made).toHaveLength(3))
    expect(ui.made[2].context.ledger).not.toBe(ui.made[1].context.ledger)
    // An event from an instance that is gone does nothing.
    ui.made[0].context.onEvent({ kind: 'closed', code: 'probe_failed' })
    expect(t.host.status()).not.toEqual({ state: 'closed', code: 'probe_failed' })
  })

  it('the way out of closed starts the binding with a fresh ledger', async () => {
    const ui = fakeUi()
    const t = setup({ ui: ui.factory })
    await t.host.show(BOUNDS)
    ui.made[0].context.onEvent({ kind: 'closed', code: 'session_rejected' })
    expect(t.host.status()).toEqual({ state: 'closed', code: 'session_rejected' })
    await t.host.reload(BOUNDS)
    expect(ui.made[1].context.ledger).not.toBe(ui.made[0].context.ledger)
  })

  it('hands every request to the injector with its frame, and applies the decision', async () => {
    const ui = fakeUi()
    const t = setup({ ui: ui.factory })
    await t.host.show(BOUNDS)
    const ses = t.sessions[0]
    const contents = t.views[0].webContents
    const send = (frame: unknown) => {
      let out: unknown = null
      ses.beforeSendHeaders?.({ id: 7, webContentsId: contents.id, frame, resourceType: 'xhr', method: 'GET', url: `${ORIGIN}/x`, requestHeaders: { A: '1' } }, (r: unknown) => { out = r })
      return out
    }
    expect(send(contents.mainFrame)).toEqual({ cancel: true })
    ui.decide({ cancel: false, requestHeaders: { A: '1', Authorization: 'Bearer j' }, injected: true })
    expect(send({ processId: 4, routingId: contents.id })).toEqual({ requestHeaders: { A: '1', Authorization: 'Bearer j' } })
    send({ processId: 5, routingId: contents.id })
    send({ processId: 4, routingId: 1 })
    send(null)
    send({ get processId(): number { throw new Error('gone') }, routingId: 1 })
    expect(ui.made[0].injected.map((i) => i.request.mainFrame)).toEqual([true, true, false, false, null, false])
    expect(ui.made[0].injected.every((i) => i.webContentsId === contents.id && i.request.id === 7 && i.request.url === `${ORIGIN}/x`)).toBe(true)
    ses.completed?.({ id: 7, statusCode: 401 })
    ses.errorOccurred?.({ id: 8 })
    expect(ui.made[0].responded).toEqual([[7, 401], [8, undefined]])
  })

  it('reloads the current document when injection resumes, only once it has loaded (U4.5)', async () => {
    const ui = fakeUi()
    const t = setup({ ui: ui.factory })
    let reloads = 0
    await t.host.show(BOUNDS)
    t.views[0].webContents.reload = () => { reloads++ }
    ui.made[0].context.onEvent({ kind: 'reload' })
    expect(reloads).toBe(1)
    expect(t.host.status()).toEqual({ state: 'ready' })
    t.views[0].webContents.destroyed = true
    ui.made[0].context.onEvent({ kind: 'reload' })
    expect(reloads).toBe(1)
  })

  it('a resume during the first load reloads once that load is done; one before loadURL does nothing (U4.5)', async () => {
    let release: ((ok: boolean) => void) | undefined
    const ui = fakeUi(() => new Promise<boolean>((resolve) => { release = resolve }))
    const t = setup({ ui: ui.factory })
    let finish: () => void = () => {}
    { const pending = new Promise<void>((resolve) => { finish = resolve }); nextLoad = () => pending }
    try {
      const shown = t.host.show(BOUNDS)
      await vi.waitFor(() => expect(ui.made).toHaveLength(1))
      let reloads = 0
      t.views[0].webContents.reload = () => { reloads++ }
      // Before the first JWT: no page yet, nothing was asked for.
      await vi.waitFor(() => expect(release).toBeTypeOf('function'))
      ui.made[0].context.onEvent({ kind: 'reload' })
      release!(true)
      await vi.waitFor(() => expect(t.views[0].webContents.loads).toHaveLength(1))
      // The page's scripts ran into a pause and it lifted before the load finished.
      ui.made[0].context.onEvent({ kind: 'reload' })
      ui.made[0].context.onEvent({ kind: 'reload' })
      expect(reloads).toBe(0)
      finish()
      expect(await shown).toEqual({ ok: true })
      expect(reloads).toBe(1)
      // Taken: a later load-finished path does not reload again.
      await flush()
      expect(reloads).toBe(1)
    } finally {
      nextLoad = () => Promise.resolve()
    }
  })

  it('a resume before loadURL alone never reloads the page that follows (U4.5)', async () => {
    let release: ((ok: boolean) => void) | undefined
    const ui = fakeUi(() => new Promise<boolean>((resolve) => { release = resolve }))
    const t = setup({ ui: ui.factory })
    const shown = t.host.show(BOUNDS)
    await vi.waitFor(() => expect(release).toBeTypeOf('function'))
    let reloads = 0
    t.views[0].webContents.reload = () => { reloads++ }
    ui.made[0].context.onEvent({ kind: 'reload' })
    release!(true)
    expect(await shown).toEqual({ ok: true })
    await flush()
    expect(reloads).toBe(0)
  })

  it('lets go of the partition listeners on destroy', async () => {
    const ui = fakeUi()
    const t = setup({ ui: ui.factory })
    await t.host.show(BOUNDS)
    expect(t.sessions[0].beforeSendHeaders).not.toBeNull()
    t.host.destroyAll('test')
    expect([t.sessions[0].beforeSendHeaders, t.sessions[0].completed, t.sessions[0].errorOccurred]).toEqual([null, null, null])
  })

  it('ends the UI session before the page goes (U4.7)', async () => {
    const log: string[] = []
    const ui = fakeUi(async () => true, log)
    const t = setup({ ui: ui.factory })
    await t.host.show(BOUNDS)
    const contents = t.views[0].webContents
    contents.close = () => { log.push('page closed'); contents.closed = true; contents.finishClose() }
    t.host.destroyAll('test')
    expect(log).toEqual(['ui destroyed', 'page closed'])
  })
})

describe('registerKnowledgeViewIpc', () => {
  it('puts every channel behind the guard', async () => {
    const handlers = new Map<string, Listener>()
    const guarded: Listener[] = []
    const guard = ((h: Listener) => { guarded.push(h); return h }) as never
    const host = { show: vi.fn(async () => ({ ok: true })), setBounds: vi.fn(), hide: vi.fn(), reload: vi.fn(async () => ({ ok: true })) }
    registerKnowledgeViewIpc({ handle: (c: string, h: Listener) => { handlers.set(c, h) } } as never, guard,
      { show: 's', bounds: 'b', hide: 'h', reload: 'r' }, () => host as never)
    expect([...handlers.keys()]).toEqual(['s', 'b', 'h', 'r'])
    expect(guarded).toHaveLength(4)
    await handlers.get('s')!({}, BOUNDS, 'dark')
    expect(host.show).toHaveBeenCalledWith(BOUNDS, 'dark')
    handlers.get('b')!({}, BOUNDS)
    expect(host.setBounds).toHaveBeenCalledWith(BOUNDS)
  })

  it('answers window_unavailable before the host exists', async () => {
    const handlers = new Map<string, Listener>()
    registerKnowledgeViewIpc({ handle: (c: string, h: Listener) => { handlers.set(c, h) } } as never, ((h: Listener) => h) as never,
      { show: 's', bounds: 'b', hide: 'h', reload: 'r' }, () => null)
    expect(await handlers.get('s')!({}, BOUNDS)).toEqual({ ok: false, code: 'window_unavailable' })
  })
})

describe('bridge wiring (KB-C2-04a-2, U6.1, U6.6)', () => {
  type Ui = KnowledgeViewUi & { admission: { open: boolean; generation: number } }
  const ui = (): { factory: (c: KnowledgeViewUiContext) => KnowledgeViewUi; made: Ui[] } => {
    const made: Ui[] = []
    return {
      made,
      factory: () => {
        const u: Ui = {
          admission: { open: true, generation: 3 },
          start: async () => true,
          inject: () => ({ cancel: false, requestHeaders: {}, injected: false }),
          responded: () => {},
          destroy: () => {},
          bridgeAdmission: () => u.admission,
        }
        made.push(u)
        return u
      },
    }
  }
  const BRIDGE = `${ORIGIN}/__agent24/bridge`
  let n = 0
  const msg = (payload: unknown = { state: 'ready' }, extra: Record<string, unknown> = {}) =>
    ({ v: 2, direction: 'page_to_host', type: 'status', id: `01J9ZQ3V6R8XKQ4M2N5P7T${String(n++).padStart(4, '0')}`, generation: 3, payload, ...extra })
  const delivered = (contents: FakeContents) => contents.mainFrame.scripts.map((s) => JSON.parse(JSON.parse(/JSON\.parse\((.*)\) \}\)\);undefined$/.exec(s)![1])))
  const post = (t: ReturnType<typeof setup>, i: number, body: unknown, over: Record<string, unknown> = {}) => {
    const contents = t.views[i].webContents
    let out: unknown = null
    t.sessions[i].beforeRequest?.({ url: BRIDGE, method: 'POST', webContentsId: contents.id, frame: contents.mainFrame, uploadData: [{ bytes: Buffer.from(JSON.stringify(body)) }], ...over }, (r: unknown) => { out = r })
    return out
  }

  it('takes the reserved POST into the bridge and never lets it out; greets each new document', async () => {
    const u = ui()
    const t = setup({ ui: u.factory })
    await t.host.show(BOUNDS)
    const contents = t.views[0].webContents
    contents.emit('did-navigate', {}, URL_, 200)
    expect(delivered(contents).map((m) => m.type)).toEqual(['hello', 'state'])
    expect(delivered(contents)[1].payload).toEqual({ profile_state: 'ready', theme: 'dark', locale: 'en' })
    const m = msg()
    expect(post(t, 0, m)).toEqual({ cancel: true })
    expect(delivered(contents).at(-1)).toMatchObject({ type: 'reply', in_reply_to: m.id, generation: 3, payload: { result: 'done' } })
    // Anything else under the prefix is cancelled too and reaches no bridge.
    let other: unknown = null
    t.sessions[0].beforeRequest?.({ url: `${ORIGIN}/__agent24/x`, method: 'GET' }, (r: unknown) => { other = r })
    expect(other).toEqual({ cancel: true })
    expect(t.sessions[0].allows(`${ORIGIN}/api/v1/x`)).toBe(true)
    contents.emit('did-navigate', {}, URL_, 200)
    expect(delivered(contents).filter((x) => x.type === 'hello')).toHaveLength(2)
  })

  it('the first state is taken once the session has started, not when the instance is made', async () => {
    const made: Ui[] = []
    const factory = (): KnowledgeViewUi => {
      const u: Ui = {
        admission: { open: false, generation: 3 },
        start: async () => { u.admission.open = true; return true },
        inject: () => ({ cancel: false, requestHeaders: {}, injected: false }),
        responded: () => {},
        destroy: () => {},
        bridgeAdmission: () => u.admission,
      }
      made.push(u)
      return u
    }
    const t = setup({ ui: factory })
    await t.host.show(BOUNDS)
    const contents = t.views[0].webContents
    contents.emit('did-navigate', {}, URL_, 200)
    expect(delivered(contents).filter((m) => m.type === 'state').map((m) => m.payload.profile_state)).toEqual(['ready'])
  })

  it('sends a new state when the theme changes', async () => {
    const u = ui()
    const t = setup({ ui: u.factory })
    await t.host.show(BOUNDS)
    const contents = t.views[0].webContents
    contents.emit('did-navigate', {}, URL_, 200)
    await t.host.show(BOUNDS, 'light')
    expect(delivered(contents).at(-1)).toMatchObject({ type: 'state', payload: { theme: 'light' } })
  })

  it('without a UI session there is no bridge, and the path is still cancelled', async () => {
    const t = setup()
    await t.host.show(BOUNDS)
    t.views[0].webContents.emit('did-navigate', {}, URL_, 200)
    expect(post(t, 0, msg())).toEqual({ cancel: true })
    expect(t.views[0].webContents.mainFrame.scripts).toEqual([])
  })

  it('a violation closes the view as bridge_closed; the third in 10 minutes is final until Knowledge settings', async () => {
    const u = ui()
    const t = setup({ ui: u.factory })
    for (let i = 0; i < 3; i++) {
      await t.host.reload(BOUNDS)
      const contents = t.views[i].webContents
      contents.emit('did-navigate', {}, URL_, 200)
      post(t, i, msg({ state: 'ready', Frame: 1 }))
      expect(t.host.status()).toEqual(i < 2 ? { state: 'closed', code: 'bridge_closed' } : { state: 'closed', code: 'bridge_closed', final: true })
      expect(contents.closed).toBe(true)
    }
    // Reload is no way out now, nor is showing again; a page that mounts
    // again is told the whole state, final included.
    expect(await t.host.reload(BOUNDS)).toEqual({ ok: false, code: 'bridge_closed' })
    t.states.length = 0
    expect(await t.host.show(BOUNDS)).toEqual({ ok: false, code: 'bridge_closed' })
    expect(t.states).toEqual([{ state: 'closed', code: 'bridge_closed', final: true }])
    expect(t.views).toHaveLength(3)
    // Knowledge off and on again: a fresh count.
    t.env.binding = null
    await t.host.show(BOUNDS)
    t.env.binding = 'https://kb.example.org/api/v1'
    expect(await t.host.show(BOUNDS)).toEqual({ ok: true })
    t.views[3].webContents.emit('did-navigate', {}, URL_, 200)
    post(t, 3, msg({ state: 'ready', Frame: 1 }))
    expect(t.host.status()).toEqual({ state: 'closed', code: 'bridge_closed' })
  })

  it('closings older than 10 minutes do not count', async () => {
    const u = ui()
    const t = setup({ ui: u.factory })
    for (let i = 0; i < 3; i++) {
      await t.host.reload(BOUNDS)
      t.views[i].webContents.emit('did-navigate', {}, URL_, 200)
      post(t, i, msg({ state: 'ready', Frame: 1 }))
      t.advance(5 * 60_000 + 1)
    }
    expect(t.host.status()).toEqual({ state: 'closed', code: 'bridge_closed' })
  })

  it('a sub-frame of the view closes it too; another webContents is ignored', async () => {
    const u = ui()
    const t = setup({ ui: u.factory })
    await t.host.show(BOUNDS)
    const contents = t.views[0].webContents
    contents.emit('did-navigate', {}, URL_, 200)
    post(t, 0, msg(), { webContentsId: 999 })
    expect(t.host.status()).toEqual({ state: 'ready' })
    post(t, 0, msg(), { frame: { processId: 4, routingId: 12345, origin: ORIGIN } })
    expect(t.host.status()).toEqual({ state: 'closed', code: 'bridge_closed' })
  })

  it('the bridge is sealed with the partition on destroy', async () => {
    const u = ui()
    const t = setup({ ui: u.factory })
    await t.host.show(BOUNDS)
    const contents = t.views[0].webContents
    contents.emit('did-navigate', {}, URL_, 200)
    const before = contents.mainFrame.scripts.length
    // The page lingers a moment after close (Electron destroys it later).
    contents.asyncClose = true
    t.host.destroyAll('test')
    expect(post(t, 0, msg())).toEqual({ cancel: true })
    contents.emit('did-navigate', {}, URL_, 200)
    expect(contents.mainFrame.scripts).toHaveLength(before)
  })

  it('admission closed: not_ready, and the view stays', async () => {
    const u = ui()
    const t = setup({ ui: u.factory })
    await t.host.show(BOUNDS)
    const contents = t.views[0].webContents
    u.made[0].admission.open = false
    contents.emit('did-navigate', {}, URL_, 200)
    post(t, 0, msg())
    expect(delivered(contents).at(-1)?.payload).toEqual({ result: 'rejected', code: 'unavailable', reason: 'not_ready' })
    expect(t.host.status()).toEqual({ state: 'ready' })
  })
})
