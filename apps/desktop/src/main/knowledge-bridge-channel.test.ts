import { describe, expect, it } from 'vitest'
import type { BridgeArrival, KnowledgeBridge } from './knowledge-bridge'
import { BRIDGE_PATH, dispatchScript, KnowledgeBridgeChannel, reservedRequest, type BridgeFrame } from './knowledge-bridge-channel'

const ORIGIN = 'https://kb.example.org'

describe('reservedRequest', () => {
  it('takes only a POST to the exact path on the allowed origin as the bridge', () => {
    expect(reservedRequest(`${ORIGIN}${BRIDGE_PATH}`, 'POST', ORIGIN)).toBe('bridge')
    expect(reservedRequest(`${ORIGIN}${BRIDGE_PATH}`, 'GET', ORIGIN)).toBe('reserved')
    expect(reservedRequest(`${ORIGIN}${BRIDGE_PATH}?x=1`, 'POST', ORIGIN)).toBe('reserved')
    expect(reservedRequest(`${ORIGIN}${BRIDGE_PATH}/x`, 'POST', ORIGIN)).toBe('reserved')
    expect(reservedRequest(`${ORIGIN}/__agent24/other`, 'POST', ORIGIN)).toBe('reserved')
    expect(reservedRequest(`${ORIGIN}/__agent24`, 'POST', ORIGIN)).toBe('reserved')
    // A dotted spelling is the same path once normalised.
    expect(reservedRequest(`${ORIGIN}/x/../__agent24/bridge`, 'POST', ORIGIN)).toBe('bridge')
  })

  it('leaves everything else to the origin filter', () => {
    expect(reservedRequest(`${ORIGIN}/api/v1/x`, 'POST', ORIGIN)).toBeNull()
    expect(reservedRequest(`${ORIGIN}/__agent24x/bridge`, 'POST', ORIGIN)).toBeNull()
    expect(reservedRequest(`https://evil.example${BRIDGE_PATH}`, 'POST', ORIGIN)).toBeNull()
    expect(reservedRequest('not a url', 'POST', ORIGIN)).toBeNull()
  })
})

describe('dispatchScript', () => {
  it('carries the text as a string literal parsed in the page, never as code', () => {
    const text = JSON.stringify({ a: '</script>"); alert(1); ("' })
    const script = dispatchScript(text)
    let got: unknown = null
    const fakeWindow = { dispatchEvent: (e: { data: unknown }) => { got = e.data } }
    // eslint-disable-next-line @typescript-eslint/no-implied-eval
    new Function('window', 'MessageEvent', script)(fakeWindow, class { data: unknown; constructor(_t: string, init: { data: unknown }) { this.data = init.data } })
    expect(got).toEqual(JSON.parse(text))
  })
})

class FakeFrame implements BridgeFrame {
  detached = false
  scripts: string[] = []
  fail = false
  constructor(public processId: number, public routingId: number, public origin: string = ORIGIN) {}
  executeJavaScript(code: string) { this.scripts.push(code); return this.fail ? Promise.reject(new Error('gone')) : Promise.resolve() }
}

class FakeContents {
  readonly id = 7
  mainFrame = new FakeFrame(4, 1)
  destroyed = false
  listeners = new Map<string, Array<(...a: any[]) => void>>()
  isDestroyed() { return this.destroyed }
  on(event: string, fn: (...a: any[]) => void) { this.listeners.set(event, [...(this.listeners.get(event) ?? []), fn]) }
  removeListener(event: string, fn: (...a: any[]) => void) { this.listeners.set(event, (this.listeners.get(event) ?? []).filter((f) => f !== fn)) }
  emit(event: string, ...args: unknown[]) { for (const fn of this.listeners.get(event) ?? []) fn(...args) }
}

function setup() {
  const contents = new FakeContents()
  const arrivals: BridgeArrival[] = []
  const docs: Array<number | string> = []
  const bridge = {
    receive: (a: BridgeArrival) => arrivals.push(a),
    newDocument: (d: number) => docs.push(d),
    leaveDocument: () => docs.push('left'),
    resumeDocument: (d: number) => docs.push(`resumed ${d}`),
  } as unknown as KnowledgeBridge
  let now = 100
  const logs: string[] = []
  const channel = new KnowledgeBridgeChannel(contents, bridge, () => now, (m) => logs.push(m))
  const body = (text = '{}') => [{ bytes: Buffer.from(text) }]
  return { contents, channel, arrivals, docs, logs, body, setNow: (n: number) => { now = n } }
}

describe('KnowledgeBridgeChannel', () => {
  it('numbers committed documents and marks the gap a navigation opens', () => {
    const t = setup()
    expect(t.channel.currentDocument()).toBeNull()
    t.contents.emit('did-navigate')
    expect(t.channel.currentDocument()).toBe(1)
    // In-page and sub-frame navigations are not new documents.
    t.contents.emit('did-start-navigation', { isMainFrame: true, isSameDocument: true })
    t.contents.emit('did-start-navigation', { isMainFrame: false, isSameDocument: false })
    expect(t.channel.currentDocument()).toBe(1)
    t.contents.emit('did-start-navigation', { isMainFrame: true, isSameDocument: false })
    expect(t.channel.currentDocument()).toBeNull()
    t.contents.emit('did-navigate')
    expect(t.channel.currentDocument()).toBe(2)
    expect(t.docs).toEqual([1, 'left', 2])
  })

  it('a navigation that does not commit gives the shown document its bridge back', () => {
    const t = setup()
    t.contents.emit('did-navigate')
    // Electron's sequence for a cancelled navigation: start, then only the load stopping.
    t.contents.emit('did-start-navigation', { isMainFrame: true, isSameDocument: false })
    expect(t.channel.currentDocument()).toBeNull()
    t.contents.emit('did-stop-loading')
    expect(t.channel.currentDocument()).toBe(1)
    expect(t.docs).toEqual([1, 'left', 'resumed 1'])
    // Once resumed, another stop changes nothing.
    t.contents.emit('did-stop-loading')
    expect(t.docs).toEqual([1, 'left', 'resumed 1'])
  })

  it('a load stopping with no navigation pending changes nothing', () => {
    const t = setup()
    t.contents.emit('did-navigate')
    t.contents.emit('did-stop-loading')
    expect(t.docs).toEqual([1])
    expect(t.channel.currentDocument()).toBe(1)
  })

  it('a committed error page keeps the bridge off until the next commit, whatever its code', () => {
    for (const code of [-102, -3]) {
      const t = setup()
      t.contents.emit('did-navigate')
      t.contents.emit('did-start-navigation', { isMainFrame: true, isSameDocument: false })
      // A sub-frame's failure is not the main frame's.
      t.contents.emit('did-fail-provisional-load', {}, code, '', '', false)
      t.contents.emit('did-fail-provisional-load', {}, code, '', '', true)
      // Electron sends did-fail-load for an error page unless the code is ERR_ABORTED.
      if (code !== -3) t.contents.emit('did-fail-load', {}, code, '', '', true)
      t.contents.emit('did-stop-loading')
      expect(t.channel.currentDocument()).toBeNull()
      expect(t.docs).toEqual([1, 'left'])
      t.contents.emit('did-navigate')
      expect(t.channel.currentDocument()).toBe(2)
    }
  })

  it('after a committed error page, a navigation that does not commit brings no document back', () => {
    const t = setup()
    t.contents.emit('did-navigate')
    t.contents.emit('did-start-navigation', { isMainFrame: true, isSameDocument: false })
    t.contents.emit('did-fail-provisional-load', {}, -102, '', '', true)
    t.contents.emit('did-fail-load', {}, -102, '', '', true)
    t.contents.emit('did-stop-loading')
    // On the error page, the host reloads; the reload is a 204 and never commits.
    t.contents.emit('did-start-navigation', { isMainFrame: true, isSameDocument: false })
    t.contents.emit('did-stop-loading')
    expect(t.channel.currentDocument()).toBeNull()
    expect(t.docs).toEqual([1, 'left', 'left'])
    t.contents.emit('did-navigate')
    expect(t.channel.currentDocument()).toBe(2)
  })

  it('a sub-frame failure does not stop a pending main-frame navigation from resuming', () => {
    const t = setup()
    t.contents.emit('did-navigate')
    t.contents.emit('did-start-navigation', { isMainFrame: true, isSameDocument: false })
    t.contents.emit('did-fail-provisional-load', {}, -102, '', '', false)
    t.contents.emit('did-stop-loading')
    expect(t.channel.currentDocument()).toBe(1)
  })

  it('before the first document, a cancelled navigation gives no document', () => {
    const t = setup()
    t.contents.emit('did-start-navigation', { isMainFrame: true, isSameDocument: false })
    t.contents.emit('did-stop-loading')
    expect(t.channel.currentDocument()).toBeNull()
    expect(t.docs).toEqual(['left'])
  })

  it('describes a request from its metadata only', () => {
    const t = setup()
    t.contents.emit('did-navigate')
    t.channel.request({ webContentsId: 7, frame: t.contents.mainFrame, uploadData: t.body('{"origin":"x"}') })
    expect(t.arrivals[0]).toEqual({ fromView: true, mainFrame: true, documentOrigin: ORIGIN, document: 1, bytes: Buffer.from('{"origin":"x"}'), senderKey: '7:4/1' })
    // The same routing pair is the same frame, a copy of the object or not.
    t.channel.request({ webContentsId: 7, frame: new FakeFrame(4, 1), uploadData: t.body() })
    expect(t.arrivals[1].mainFrame).toBe(true)
  })

  it.each([
    ['another webContents', { webContentsId: 8 }, { fromView: false }],
    ['no webContents', { webContentsId: undefined }, { fromView: false }],
    ['a sub-frame', { frame: new FakeFrame(4, 2) }, { mainFrame: false }],
    ['a frame in another process', { frame: new FakeFrame(5, 1) }, { mainFrame: false }],
    ['no frame', { frame: null }, { mainFrame: null, documentOrigin: null }],
    ['a sub-frame on another origin', { frame: new FakeFrame(4, 2, 'https://evil.example') }, { mainFrame: false, documentOrigin: 'https://evil.example' }],
  ])('marks %s', (_name, over, expected) => {
    const t = setup()
    t.contents.emit('did-navigate')
    t.channel.request({ webContentsId: 7, frame: t.contents.mainFrame, uploadData: t.body(), ...over })
    expect(t.arrivals[0]).toMatchObject(expected)
  })

  it('a detached frame is a replaced document: no frame, not a sub-frame', () => {
    const t = setup()
    t.contents.emit('did-navigate')
    const old = t.contents.mainFrame
    old.detached = true
    t.channel.request({ webContentsId: 7, frame: old, uploadData: t.body() })
    expect(t.arrivals[0].mainFrame).toBeNull()
  })

  it.each([
    ['a blob', [{ blobUUID: 'b' }]],
    ['bytes next to a blob', [{ bytes: Buffer.from('{}'), blobUUID: 'b' }]],
    ['bytes next to a file', [{ bytes: Buffer.from('{}'), file: '/tmp/x' }]],
    ['a file', [{ file: '/etc/passwd' }]],
    ['several parts', [{ bytes: Buffer.from('{') }, { bytes: Buffer.from('}') }]],
    ['no body', undefined],
  ])('gives no bytes for %s', (_name, uploadData) => {
    const t = setup()
    t.contents.emit('did-navigate')
    t.channel.request({ webContentsId: 7, frame: t.contents.mainFrame, uploadData: uploadData as never })
    expect(t.arrivals[0].bytes).toBeNull()
  })

  it('a request in the navigation gap belongs to no document', () => {
    const t = setup()
    t.contents.emit('did-navigate')
    t.contents.emit('did-start-navigation', { isMainFrame: true, isSameDocument: false })
    t.channel.request({ webContentsId: 7, frame: t.contents.mainFrame, uploadData: t.body() })
    expect(t.arrivals[0].document).toBeNull()
  })

  it('sends only to the current document, through its main frame', () => {
    const t = setup()
    t.channel.send('{"a":1}', 0)
    t.contents.emit('did-navigate')
    t.channel.send('{"a":1}', 1)
    t.channel.send('{"a":2}', 2)
    expect(t.contents.mainFrame.scripts).toEqual([dispatchScript('{"a":1}')])
    t.contents.emit('did-start-navigation', { isMainFrame: true, isSameDocument: false })
    t.channel.send('{"a":3}', 1)
    t.contents.mainFrame.detached = true
    t.contents.emit('did-navigate')
    t.channel.send('{"a":4}', 2)
    t.contents.destroyed = true
    t.contents.mainFrame.detached = false
    t.channel.send('{"a":5}', 2)
    expect(t.contents.mainFrame.scripts).toHaveLength(1)
  })

  it('a failed delivery is logged without the message', async () => {
    const t = setup()
    t.contents.emit('did-navigate')
    t.contents.mainFrame.fail = true
    t.channel.send('{"secretish":1}', 1)
    await new Promise((r) => setTimeout(r, 0))
    expect(t.logs).toEqual(['[knowledge-bridge] delivery failed: Error'])
  })

  it('notes activation-type input only', () => {
    const t = setup()
    for (const type of ['mouseMove', 'mouseWheel', 'mouseEnter', 'keyUp', 'mouseUp']) t.contents.emit('input-event', {}, { type })
    expect(t.channel.lastActivation()).toBeNull()
    for (const [type, at] of [['mouseDown', 1], ['rawKeyDown', 2], ['keyDown', 3], ['touchStart', 4], ['gestureTap', 5]] as const) {
      t.setNow(at)
      t.contents.emit('input-event', {}, { type })
      expect(t.channel.lastActivation()).toBe(at)
    }
  })

  it('lets go of the page on dispose and takes nothing more', () => {
    const t = setup()
    t.contents.emit('did-navigate')
    t.channel.dispose()
    expect([...t.contents.listeners.values()].every((l) => l.length === 0)).toBe(true)
    t.channel.request({ webContentsId: 7, frame: t.contents.mainFrame, uploadData: t.body() })
    t.channel.send('{}', 1)
    expect(t.arrivals).toEqual([])
    expect(t.contents.mainFrame.scripts).toEqual([])
  })
})
