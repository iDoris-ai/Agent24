// KB-C2-04a-2 (#1623): the host side of one view instance's bridge
// (CONTRACT-UI-BRIDGE-v1 §6). Every page message is judged in U6.4's order,
// the first step that does not hold decides (①–⑯), and violations close the
// bridge (U6.6). Host→page messages (`hello`, `state`, `reply`) go out only to
// the document they were made for, within U6.7's budgets. No Electron here:
// knowledge-bridge-channel.ts turns the partition's requests into
// `BridgeArrival`s and carries the encoded messages back to the main frame.
//
// What the accepted messages then do is the handler's (04b: citation reads,
// return to chat). This slice answers `status` and `cancel` itself and has
// citation and intent wait for 04b: `unavailable/not_ready`, as for a bridge
// that is not admitted yet (design §6: when unsure, not ready).

import { createHash, randomBytes } from 'node:crypto'
import {
  BRIDGE_MAX_MESSAGE_BYTES,
  BRIDGE_SUPPORTED,
  encodeHostMessage,
  envelopeId,
  hasForbiddenKey,
  parseBridgeJson,
  readPageMessage,
  versionOf,
  type HostMessage,
  type PageMessage,
  type ReplyBody,
} from './knowledge-bridge-message'

// ── U6.6 / U6.7 numbers ──────────────────────────────────────────────────────
export const BRIDGE_RATE_PER_S = 10
export const BRIDGE_BURST = 20
/** How long a page is told to back off after `overloaded`. */
export const BRIDGE_BACKOFF_MS = 1_000
/** U6.6: arrivals within this of an `overloaded` do not count against the page. */
export const BRIDGE_BACKOFF_GRACE_MS = 500
export const BRIDGE_BACKOFF_STRIKES = 3
export const BRIDGE_VIOLATION_WINDOW_MS = 10_000
export const BRIDGE_SOFT_LIMIT = 5
/** U6.6: ⑪ is counted apart; this many in the window make one soft violation. */
export const BRIDGE_VERSION_LIMIT = 5
export const BRIDGE_DEDUPE_LIMIT = 4096
/** U6.3: hello requests per document. */
export const BRIDGE_HELLO_REQUESTS = 3
/** U6.7: `hello` and `state` together. */
export const BRIDGE_HOST_PER_S = 5
/** U6.4 ①–⑤: senders tracked for counting, at most. */
export const BRIDGE_TRACKED_SENDERS = 64

/** What the transport knows about one bridge request, from its metadata only (U6.5). */
export interface BridgeArrival {
  /** ①: it came from this instance's webContents. */
  fromView: boolean
  /** ②: the page's main frame; null when the request has no frame any more. */
  mainFrame: boolean | null
  /** ③: the committed origin of the frame that sent it, when known. */
  documentOrigin: string | null
  /** ⑤: the document it belongs to (the channel's count), null between documents. */
  document: number | null
  /** ⑦: the body's bytes; null when it was not one inline body (blob, file, several parts). */
  bytes: Uint8Array | null
  /** For U6.4's per-sender counting of silent drops. */
  senderKey: string
}

export interface BridgeDeps {
  /** The allowed origin (③). */
  origin: string
  /** ⑬⑭: admission open, servable and fresh, and the generation now (design §6: the UI session's). */
  admission: () => { open: boolean; generation: number }
  /** Hands an encoded message to the main frame of document `document`; the channel drops it if that is not current. */
  send: (text: string, document: number) => void
  /** The bridge closed itself for a violation (U6.6). */
  onClose: () => void
  now: () => number
  setTimer: (fn: () => void, ms: number) => unknown
  clearTimer: (handle: unknown) => void
  log: (message: string) => void
  /** Tests only. */
  viewInstanceId?: string
}

type Step = '①' | '②' | '③' | '⑤' | '⑥' | '⑦' | '⑧' | '⑨' | '⑪' | '⑫' | '⑬' | '⑭' | '⑮'

interface Backoff { from: number; until: number; strikes: number; struck: boolean }

const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ'

/** A ULID for host messages (U7.1): 48-bit time, 80 random bits. */
export function newUlid(now: number = Date.now()): string {
  let time = ''
  let t = Math.floor(now)
  for (let i = 0; i < 10; i++) { time = CROCKFORD[t % 32] + time; t = Math.floor(t / 32) }
  const rand = randomBytes(16)
  let tail = ''
  for (let i = 0; i < 16; i++) tail += CROCKFORD[rand[i] % 32]
  return time + tail
}

export class KnowledgeBridge {
  readonly viewInstanceId: string
  /** H("kb-view-v1"; view instance id) for logs (U6.8). */
  private readonly logTag: string
  private closed = false
  private document: number | null = null
  // Per document (U6.3: cleared on every new document).
  private seen: string[] = []
  private seenSet = new Set<string>()
  private helloRequests = 0
  // Per instance.
  private tokens = BRIDGE_BURST
  private refilledAt: number
  private rateBackoff: Backoff | null = null
  private soft: number[] = []
  private versions: number[] = []
  private dropped = new Map<string, number>()
  // Host→page budget for hello + state (U6.7); replies are never held.
  private hostTokens = BRIDGE_HOST_PER_S
  private hostRefilledAt: number
  private queuedHello = false
  private queuedState: HostMessage | null = null
  private flushTimer: unknown = null
  private lastState: HostMessage | null = null

  constructor(private readonly deps: BridgeDeps) {
    this.viewInstanceId = deps.viewInstanceId ?? randomBytes(16).toString('base64url')
    this.logTag = createHash('sha256').update('kb-view-v1').update('\0').update(this.viewInstanceId).digest('base64url').slice(0, 16)
    this.refilledAt = deps.now()
    this.hostRefilledAt = deps.now()
  }

  isClosed(): boolean {
    return this.closed
  }

  /** U6.3: the main frame committed a new document (reloads too). Tables go, and it gets `hello`. */
  newDocument(document: number): void {
    if (this.closed) return
    this.document = document
    this.seen = []
    this.seenSet.clear()
    this.helloRequests = 0
    this.queuedHello = false
    this.queuedState = null
    this.queueHello()
    if (this.lastState) this.queueState(this.lastState)
  }

  /** A navigation started: until the next document, nothing is delivered and nothing is taken. */
  leaveDocument(): void {
    this.document = null
    this.queuedHello = false
    this.queuedState = null
  }

  /** U7.3 `state`; only the newest one waits when the budget is spent. */
  setState(payload: { profile_state: string; code?: string; theme: 'light' | 'dark'; locale: string }): void {
    const message: HostMessage = { type: 'state', payload }
    this.lastState = message
    if (!this.closed && this.document !== null) this.queueState(message)
  }

  /** The instance is going: nothing more is taken or sent. Not a violation. */
  destroy(): void {
    this.closed = true
    this.document = null
    if (this.flushTimer !== null) this.deps.clearTimer(this.flushTimer)
    this.flushTimer = null
  }

  /** One request to the bridge path, judged in U6.4's order. */
  receive(arrival: BridgeArrival): void {
    const now = this.deps.now()
    // ①–⑤: silent; from inside the view ②③ are hard violations (U6.6).
    if (this.closed || !arrival.fromView) return this.drop('①', arrival)
    if (arrival.mainFrame === null) return this.drop('⑤', arrival)
    if (!arrival.mainFrame) return this.hard('②', arrival)
    // An origin that cannot be read is not judged against the page.
    if (arrival.documentOrigin === null) return this.drop('⑤', arrival)
    if (arrival.documentOrigin !== this.deps.origin) return this.hard('③', arrival)
    // ④ holds by construction: the channel listens on this instance's partition only.
    const document = this.document
    if (document === null || arrival.document !== document) return this.drop('⑤', arrival)
    // ⑥ transport rate, before anything is read.
    if (!this.takeToken(now)) return this.rateLimited(now, document)
    // ⑦ raw length.
    const bytes = arrival.bytes
    if (bytes === null || bytes.byteLength > BRIDGE_MAX_MESSAGE_BYTES) return this.reject('⑦', document, undefined, { code: 'invalid_request', reason: 'too_large' }, true)
    // ⑧ strict JSON.
    const value = parseBridgeJson(bytes)
    if (value === undefined) return this.reject('⑧', document, undefined, { code: 'invalid_request', reason: 'schema_violation' }, true)
    const id = envelopeId(value)
    // ⑨ forbidden keys: reply, then close.
    if (hasForbiddenKey(value)) {
      this.reply(document, id, { result: 'rejected', code: 'invalid_request', reason: 'forbidden_key' })
      return this.hard('⑨', arrival, id)
    }
    // ⑩ holds: v1 has no free text.
    // ⑪ version, counted apart.
    if (versionOf(value) === 'unsupported') {
      this.reply(document, id, { result: 'rejected', code: 'invalid_request', reason: 'unsupported_protocol_version', supported: [...BRIDGE_SUPPORTED] })
      this.log('⑪', id, 'unsupported_protocol_version')
      this.versions = this.recent(this.versions, now)
      this.versions.push(now)
      if (this.versions.length >= BRIDGE_VERSION_LIMIT) {
        this.versions = []
        this.softViolation(now)
      }
      return
    }
    // ⑫ schema.
    const read = readPageMessage(value)
    if (!read.ok) return this.reject('⑫', document, id, { code: 'invalid_request', reason: 'schema_violation' }, read.counted)
    const message = read.message
    if (message.helloRequest) {
      if (this.helloRequests < BRIDGE_HELLO_REQUESTS) {
        this.helloRequests++
        this.queueHello()
        return
      }
      // U6.3: past three, a generation-0 status is a stale one.
      return this.reject('⑭', document, id, { code: 'invalid_request', reason: 'stale_generation' }, true)
    }
    // ⑬ admission: refused, never counted.
    const admission = this.deps.admission()
    if (!admission.open) return this.reject('⑬', document, id, { code: 'unavailable', reason: 'not_ready' }, false)
    // ⑭ generation.
    if (message.generation !== admission.generation) return this.reject('⑭', document, id, { code: 'invalid_request', reason: 'stale_generation' }, true)
    // ⑮ duplicate.
    if (this.seenSet.has(message.id)) return this.reject('⑮', document, id, { code: 'invalid_request', reason: 'duplicate_id' }, true)
    // ⑯ holds: nothing of this slice stays in flight. 04b's citation reads and
    // intents bring the table (≤4, intent ≤1, U6.7) with them.
    this.remember(message.id)
    this.handle(message, document)
  }

  // ── handling (04a-2's share) ──────────────────────────────────────────────

  private handle(message: PageMessage, document: number): void {
    const type = message.payload.type
    // `status` is for the host's display only (U7.2); this slice shows nothing of it.
    if (type === 'status') return this.reply(document, message.id, { result: 'done' })
    // Nothing of 04a-2 is ever in flight, so a cancel always finds no target (U7.2).
    if (type === 'cancel') return this.reply(document, message.id, { result: 'accepted' })
    // Citation and intent come with 04b.
    this.reply(document, message.id, { result: 'rejected', code: 'unavailable', reason: 'not_ready' })
  }

  // ── steps ──────────────────────────────────────────────────────────────────

  private takeToken(now: number): boolean {
    if (this.rateBackoff && now < this.rateBackoff.until) return false
    this.tokens = Math.min(BRIDGE_BURST, this.tokens + ((now - this.refilledAt) / 1000) * BRIDGE_RATE_PER_S)
    this.refilledAt = now
    if (this.tokens < 1) return false
    this.tokens -= 1
    return true
  }

  /** ⑥: one `overloaded` per window, the rest silent; repeated pressure after the grace is a soft violation (U6.6). */
  private rateLimited(now: number, document: number): void {
    const b = this.rateBackoff
    if (b && now < b.until) {
      this.strike(b, now)
      return
    }
    this.rateBackoff = { from: now, until: now + BRIDGE_BACKOFF_MS, strikes: 0, struck: false }
    this.log('⑥', undefined, 'overloaded')
    // ⑥ replies carry no in_reply_to (U7.1): nothing was read.
    this.reply(document, undefined, { result: 'rejected', code: 'unavailable', reason: 'overloaded', retry_after_ms: BRIDGE_BACKOFF_MS })
  }

  private strike(b: Backoff, now: number): void {
    if (b.struck || now - b.from < BRIDGE_BACKOFF_GRACE_MS) return
    b.strikes++
    if (b.strikes >= BRIDGE_BACKOFF_STRIKES) {
      b.struck = true
      this.softViolation(now)
    }
  }

  private reject(step: Step, document: number, id: string | undefined, body: { code: string; reason: string }, counted: boolean): void {
    this.log(step, id, body.reason)
    this.reply(document, id, { result: 'rejected', ...body })
    if (counted) this.softViolation(this.deps.now())
  }

  private softViolation(now: number): void {
    if (this.closed) return
    this.soft = this.recent(this.soft, now)
    this.soft.push(now)
    if (this.soft.length >= BRIDGE_SOFT_LIMIT) this.close('soft violations')
  }

  /** U6.6: one is enough; audited as `knowledge.bridge_violation`, without the message. */
  private hard(step: Step, _arrival: BridgeArrival, id?: string): void {
    this.log(step, id, 'knowledge.bridge_violation')
    this.close(`hard violation at ${step}`)
  }

  private close(why: string): void {
    if (this.closed) return
    this.deps.log(`[knowledge-bridge] view=${this.logTag} closed: ${why}`)
    this.destroy()
    this.deps.onClose()
  }

  private drop(step: Step, arrival: BridgeArrival): void {
    const key = arrival.senderKey
    if (!this.dropped.has(key) && this.dropped.size >= BRIDGE_TRACKED_SENDERS) return
    const n = (this.dropped.get(key) ?? 0) + 1
    this.dropped.set(key, n)
    // Sampled: the first, then every hundredth (U6.4).
    if (n === 1 || n % 100 === 0) this.log(step, undefined, `dropped (${n} from this sender)`)
  }

  private remember(id: string): void {
    this.seen.push(id)
    this.seenSet.add(id)
    if (this.seen.length > BRIDGE_DEDUPE_LIMIT) this.seenSet.delete(this.seen.shift()!)
  }

  private recent(list: number[], now: number): number[] {
    return list.filter((t) => now - t < BRIDGE_VIOLATION_WINDOW_MS)
  }

  /** U6.8: type, code, reason, id, the view tag and the step; never the body. */
  private log(step: Step, id: string | undefined, reason: string): void {
    this.deps.log(`[knowledge-bridge] view=${this.logTag} step=${step} reason=${reason}${id ? ` id=${id}` : ''}`)
  }

  // ── host→page ─────────────────────────────────────────────────────────────

  private reply(document: number, inReplyTo: string | undefined, body: ReplyBody): void {
    this.emit({ type: 'reply', ...(inReplyTo ? { in_reply_to: inReplyTo } : {}), payload: body }, document)
  }

  private queueHello(): void {
    this.queuedHello = true
    this.flush()
  }

  private queueState(message: HostMessage): void {
    this.queuedState = message
    this.flush()
  }

  private flush(): void {
    const now = this.deps.now()
    this.hostTokens = Math.min(BRIDGE_HOST_PER_S, this.hostTokens + ((now - this.hostRefilledAt) / 1000) * BRIDGE_HOST_PER_S)
    this.hostRefilledAt = now
    const document = this.document
    if (this.closed || document === null) return
    while (this.hostTokens >= 1 && (this.queuedHello || this.queuedState)) {
      this.hostTokens -= 1
      if (this.queuedHello) {
        this.queuedHello = false
        this.emit({ type: 'hello', payload: { supported: [...BRIDGE_SUPPORTED], view_instance_id: this.viewInstanceId } }, document)
      } else {
        const state = this.queuedState!
        this.queuedState = null
        this.emit(state, document)
      }
    }
    if ((this.queuedHello || this.queuedState) && this.flushTimer === null) {
      const wait = Math.ceil(((1 - this.hostTokens) / BRIDGE_HOST_PER_S) * 1000)
      this.flushTimer = this.deps.setTimer(() => { this.flushTimer = null; this.flush() }, Math.max(1, wait))
    }
  }

  private emit(message: HostMessage, document: number): void {
    if (this.closed || document !== this.document) return
    const generation = Math.max(1, this.deps.admission().generation)
    const text = encodeHostMessage(message, newUlid(), generation)
    if (text === null) {
      this.deps.log(`[knowledge-bridge] view=${this.logTag} not sent: ${message.type} failed the host-side check`)
      return
    }
    this.deps.send(text, document)
  }
}
