import { describe, expect, it, vi } from 'vitest'
import {
  KNOWLEDGE_VIEW_DEV_URL_ENV,
  KNOWLEDGE_VIEW_PARTITION_PREFIX,
  MAX_KNOWLEDGE_VIEW_PARTITIONS,
  KNOWLEDGE_VIEW_DESTROY_WAIT_MS,
  KNOWLEDGE_VIEW_FIRST_LOAD_TIMEOUT_MS,
  KnowledgeViewHost,
  type KnowledgeViewHostDeps,
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
  readonly webRequest = {
    onBeforeRequest: (l: Listener) => { this.beforeRequest = l },
    onHeadersReceived: (l: Listener) => { this.headersReceived = l },
  }
  /** Calls in order, for checking that cleanup comes after the page is gone. */
  readonly log: string[]
  constructor(readonly partition: string, log: string[] = []) { super(); this.log = log }
  setPermissionRequestHandler(l: Listener) { this.permissionRequest = l }
  setPermissionCheckHandler(l: Listener) { this.permissionCheck = l }
  setDevicePermissionHandler(l: Listener) { this.devicePermission = l }
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

class FakeContents extends FakeEmitter {
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

function setup(over: Partial<{ binding: string | null; target: ReturnType<typeof knowledgeViewTarget>; packaged: boolean }> = {}) {
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
  } as unknown as KnowledgeViewHostDeps
  const host = new KnowledgeViewHost(deps)
  return { host, views, sessions, states, timers, loadTimers, cleared, env, advance: (ms: number) => { now += ms } }
}

describe('knowledgeViewTarget (U4.4)', () => {
  it('takes https, and http only to a loopback IP literal', () => {
    expect(knowledgeViewTarget(URL_)).toEqual({ origin: ORIGIN, url: URL_ })
    expect(knowledgeViewTarget('http://127.0.0.1:18282/')).toEqual({ origin: 'http://127.0.0.1:18282', url: 'http://127.0.0.1:18282/' })
    expect(knowledgeViewTarget('http://[::1]:9/x')?.origin).toBe('http://[::1]:9')
    for (const bad of ['http://localhost:18282/', 'http://10.0.0.2/', 'file:///etc/passwd', 'agent24://x', 'https://u:p@kb.example.org/',
      'https://kb.example.org/?token=x', 'https://kb.example.org/#t', 'not a url', 42, undefined]) {
      expect(knowledgeViewTarget(bad)).toBeNull()
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
  })

  it('adds its CSP next to the server one, never in place of it', () => {
    const out = withKnowledgeCsp({ 'content-security-policy': ["script-src 'self'"], 'X-A': ['1'] }, ORIGIN)
    expect(out['Content-Security-Policy']).toEqual(["script-src 'self'", knowledgeViewCsp(ORIGIN)])
    expect(out['content-security-policy']).toBeUndefined()
    expect(out['X-A']).toEqual(['1'])
  })

  it('switches powerful features off in the page, after any server policy', () => {
    const out = withKnowledgeCsp({ 'permissions-policy': ['geolocation=*, fullscreen=()'], 'X-A': ['1'] }, ORIGIN)
    expect(out['Permissions-Policy']).toEqual(['geolocation=*, fullscreen=()', KNOWLEDGE_VIEW_PERMISSIONS_POLICY])
    expect(out['permissions-policy']).toBeUndefined()
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
    nextLoad = () => new Promise<void>((resolve) => { finish = resolve })
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
    nextLoad = () => new Promise<void>((resolve) => { finish = resolve })
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
    nextLoad = () => new Promise<void>((resolve) => { finish = resolve })
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
