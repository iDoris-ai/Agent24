// KB-C2-04a-2 (#1623): the bridge's transport (design §3.1 option A, decided
// by evidence R0 on 11-08). Page→host is a POST to the reserved path on the
// allowed origin, taken in the partition's one `onBeforeRequest` and always
// cancelled, so it never leaves the machine (U6.1 b); what the request is
// (webContents, frame, its committed origin, the bytes) comes from Electron's
// metadata, never from the body (U6.1 a, d; U6.5). Host→page is a fixed
// `message` event dispatched in the current document's main frame (U6.1 e).
// Activation-type input is noted for 04b's intent check (U6.1 f).

import type { BridgeArrival, KnowledgeBridge } from './knowledge-bridge'

/** Reserved on the allowed origin; Weknora confirmed the server never uses `/__agent24/`. */
export const BRIDGE_PATH = '/__agent24/bridge'
const RESERVED_PREFIX = '/__agent24/'

/** U6.1 f: pressing, clicking, touching; not moves, hovers or scrolls. */
const ACTIVATION = new Set(['mouseDown', 'rawKeyDown', 'keyDown', 'touchStart', 'gestureTap'])

/** 'bridge' for the channel's POST, 'reserved' for anything else under the prefix (cancelled, never sent), null otherwise. */
export function reservedRequest(url: string, method: string, origin: string): 'bridge' | 'reserved' | null {
  let parsed: URL
  try { parsed = new URL(url) } catch { return null }
  if (parsed.origin !== origin) return null
  // The normalised path: a dotted spelling of the prefix is the prefix.
  if (!parsed.pathname.startsWith(RESERVED_PREFIX) && parsed.pathname !== RESERVED_PREFIX.slice(0, -1)) return null
  return method === 'POST' && parsed.pathname === BRIDGE_PATH && parsed.search === '' ? 'bridge' : 'reserved'
}

/** The slice of a WebFrameMain the channel reads. */
export interface BridgeFrame {
  processId: number
  routingId: number
  readonly origin: string
  readonly detached?: boolean
  executeJavaScript(code: string): Promise<unknown>
  isDestroyed?(): boolean
}

/** The slice of a WebContents the channel uses. */
export interface BridgeContents {
  readonly id: number
  readonly mainFrame: BridgeFrame
  isDestroyed(): boolean
  on(event: string, listener: (...args: any[]) => void): unknown
  removeListener(event: string, listener: (...args: any[]) => void): unknown
}

/** What `onBeforeRequest` gives for one request, as far as the channel reads it. */
export interface BridgeRequestDetails {
  webContentsId?: number
  frame?: BridgeFrame | null
  uploadData?: Array<{ bytes?: Uint8Array; file?: string; blobUUID?: string }>
}

function sameFrame(a: BridgeFrame, b: BridgeFrame): boolean {
  try {
    return a === b || (a.processId === b.processId && a.routingId === b.routingId)
  } catch {
    return false
  }
}

/** One inline body, or null (blob, file, several parts): ⑦ refuses it without reading (design §3.1 d). */
function inlineBytes(upload: BridgeRequestDetails['uploadData']): Uint8Array | null {
  if (!Array.isArray(upload) || upload.length !== 1) return null
  const part = upload[0]
  if (part.file !== undefined || part.blobUUID !== undefined || !(part.bytes instanceof Uint8Array)) return null
  return part.bytes
}

/** The page side receives `{ type: 'message', data }` on window; the data is the U7.1 envelope. */
export function dispatchScript(text: string): string {
  // The text is JSON; embedded as a JS string literal and parsed in the page, never evaluated.
  // The page owns window, JSON and MessageEvent and may make any of them throw; the catch keeps
  // what it throws in the page, so nothing of the page's size comes back to main (#1661 C-1).
  return `try{window.dispatchEvent(new MessageEvent('message', { data: JSON.parse(${JSON.stringify(text)}) }))}catch{};undefined`
}

export class KnowledgeBridgeChannel {
  private document = 0
  private between = true
  /** A main-frame navigation started and has not committed yet. */
  private pending = false
  /** The page shown is still `document`: false before the first commit and after a committed error page. */
  private resumable = false
  private activatedAt: number | null = null
  private disposed = false
  private readonly listeners: Array<[string, (...args: any[]) => void]> = []

  constructor(
    private readonly contents: BridgeContents,
    private readonly bridge: KnowledgeBridge,
    private readonly now: () => number,
    private readonly log: (message: string) => void = () => {},
  ) {
    this.listen('did-start-navigation', (details: { isMainFrame?: boolean; isSameDocument?: boolean } | undefined) => {
      if (!details?.isMainFrame || details.isSameDocument) return
      // Design §3.1 ⑤: from here to the new document's hello, nothing is taken or delivered.
      this.between = true
      this.pending = true
      this.bridge.leaveDocument()
    })
    this.listen('did-navigate', () => {
      // U6.3: every committed main-frame document, reloads included.
      this.document++
      this.between = false
      this.pending = false
      this.resumable = true
      this.bridge.newDocument(this.document)
    })
    // Electron 34 (WebContents::DidFinishNavigation) reports a main-frame
    // navigation that never committed — cancelled by the guards'
    // will-navigate / will-frame-navigate / will-redirect, a download, a 204 —
    // with no did-navigate and no did-fail-*: only the load stopping. The
    // page shown is still the document it was, and its bridge comes back.
    // did-fail-provisional-load fires only for an error page Chromium did
    // commit (without did-navigate): a document the bridge never met, so it
    // stays off until the next real commit.
    this.listen('did-fail-provisional-load', (_event: unknown, _code: unknown, _description: unknown, _url: unknown, isMainFrame: unknown) => {
      if (!isMainFrame) return
      this.pending = false
      this.resumable = false
    })
    this.listen('did-stop-loading', () => {
      if (!this.pending) return
      this.pending = false
      if (!this.resumable) return
      this.between = false
      this.bridge.resumeDocument(this.document)
    })
    this.listen('input-event', (_event: unknown, input: { type?: string } | undefined) => {
      if (input?.type && ACTIVATION.has(input.type)) this.activatedAt = this.now()
    })
  }

  /** The document now, or null between documents. */
  currentDocument(): number | null {
    return this.between ? null : this.document
  }

  /** When the view last had activation-type input (04b: intent within 5 s, U7.2). */
  lastActivation(): number | null {
    return this.activatedAt
  }

  /** One request to the bridge path (the partition cancels it in any case). */
  request(details: BridgeRequestDetails): void {
    if (this.disposed) return
    const fromView = details.webContentsId === this.contents.id
    const frame = details.frame ?? null
    let mainFrame: boolean | null = null
    let documentOrigin: string | null = null
    if (frame && !this.contents.isDestroyed()) {
      try {
        // A detached frame is a document the navigation already replaced
        // (unload, keepalive): late, not a sub-frame of this one, so ⑤ drops
        // it silently instead of ② closing the bridge.
        mainFrame = frame.detached ? null : sameFrame(frame, this.contents.mainFrame)
        documentOrigin = typeof frame.origin === 'string' ? frame.origin : null
      } catch {
        mainFrame = null
      }
    }
    const arrival: BridgeArrival = {
      fromView,
      mainFrame,
      documentOrigin,
      document: this.currentDocument(),
      bytes: inlineBytes(details.uploadData),
      senderKey: `${details.webContentsId ?? 'none'}:${frame ? `${frame.processId}/${frame.routingId}` : 'noframe'}`,
    }
    this.bridge.receive(arrival)
  }

  /** U6.1 e: only to the main frame of document `document`, and only while it is current. */
  send(text: string, document: number): void {
    if (this.disposed || this.between || document !== this.document || this.contents.isDestroyed()) return
    let frame: BridgeFrame
    try {
      frame = this.contents.mainFrame
      if (frame.detached || frame.isDestroyed?.()) return
    } catch {
      return
    }
    frame.executeJavaScript(dispatchScript(text)).catch((error: unknown) => {
      this.log(`[knowledge-bridge] delivery failed: ${error instanceof Error ? error.name : 'error'}`)
    })
  }

  dispose(): void {
    if (this.disposed) return
    this.disposed = true
    this.between = true
    for (const [event, listener] of this.listeners) {
      try { this.contents.removeListener(event, listener) } catch { /* a destroyed page has none left */ }
    }
    this.listeners.length = 0
  }

  private listen(event: string, listener: (...args: any[]) => void): void {
    this.contents.on(event, listener)
    this.listeners.push([event, listener])
  }
}
