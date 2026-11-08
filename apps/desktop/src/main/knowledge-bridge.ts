// KB-C2-04a-2 (#1623): the host side of one view instance's bridge
// (CONTRACT-UI-BRIDGE-v1 §6). Every page message is judged in U6.4's order,
// the first step that does not hold decides (①–⑯), and violations close the
// bridge (U6.6). Host→page messages (`hello`, `state`, `reply`) go out only to
// the document they were made for, within U6.7's budgets. No Electron here:
// knowledge-bridge-channel.ts turns the partition's requests into
// `BridgeArrival`s and carries the encoded messages back to the main frame.
//
// What the accepted messages then do is the handler's. This slice answers
// `status`, `cancel`, `citation` and, when `intent` is wired, `return_to_chat_intent`
// (U7.2). Without `intent`, that message stays `unavailable/not_ready`.

import { createHash, randomBytes } from 'node:crypto'
import { BridgeReadBudget } from './knowledge-bridge-budget'
import type { DraftItem } from './knowledge-draft'
import { plainTitle, type ChunkRead, type CitationRef } from './knowledge-readonly'
import {
  BRIDGE_MAX_MESSAGE_BYTES,
  BRIDGE_SUPPORTED,
  encodeHostMessage,
  envelopeDocumentId,
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
/** U7.2: an intent is accepted only after activation-type input within this. */
export const INTENT_ACTIVATION_MS = 5000
/** U7.2: the confirmation dialog waits at most this long. */
export const INTENT_CONFIRM_MS = 60_000
/** U7.2: after the user declines, further intents are refused for this long. */
export const INTENT_COOLDOWN_MS = 30_000
/** U7.2: while the dialog is up, admission is polled on this period. */
export const INTENT_ADMISSION_POLL_MS = 1000

/** What the confirmation dialog is asked to show (U7.2). Titles are the host's own read. */
export interface IntentAsk {
  count: number
  titles: Array<string | null>
}

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
  /**
   * R2-3: the frame's `top` is itself. A bfcache or prerender document is
   * top-level and not the current main frame; that is a silent ⑤, not ②.
   */
  topLevel?: boolean
}

/** What the main window is shown for one citation (U7.2). The renderer gets fewer fields. */
export interface CitationShown {
  title: string | null
  text: string
  ref: CitationRef
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
  /**
   * U7.2 citation read. Without it, citation stays `unavailable/not_ready`
   * (04a-2). The budget is this view's, never the main conversation's.
   */
  citation?: {
    scope: () => readonly string[] | null
    /** `visible` is the visible set asked again (U7.2); the read checks it before every page. */
    read: (generation: number, ref: CitationRef, signal: AbortSignal, visible: () => boolean) => Promise<ChunkRead>
    show: (shown: CitationShown) => void
  }
  /**
   * U7.2 `return_to_chat_intent`. Without it, the message stays
   * `unavailable/not_ready` and takes no slot (04b-1).
   */
  intent?: {
    /** `channel.lastActivation()`: the last activation-type input, or null. */
    activatedAt: () => number | null
    /** How many more draft attachments fit (16 − current). */
    draftRoom: () => number
    /** Shows the dialog. When `signal` aborts, the dialog closes. */
    /** `unavailable`: no dialog could be shown (no main window, one already up) — not the user's no, so no cooldown. */
    confirm: (ask: IntentAsk, signal: AbortSignal) => Promise<'confirmed' | 'declined' | 'unavailable'>
    /** Adds them in one go. False means they would pass 16 (`attachment_limit`). */
    attach: (items: DraftItem[]) => boolean
  }
}

type Step = '①' | '②' | '③' | '⑤' | '⑥' | '⑦' | '⑧' | '⑨' | '⑪' | '⑫' | '⑬' | '⑭' | '⑮' | '⑯'

/** U6.7: messages in flight per document. `cancel` does not take a slot. */
const BRIDGE_IN_FLIGHT = 4

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
  /** Decision #37: the current document's id, new on every commit; sent in every host message, echoed by the page. */
  private documentId: string | null = null
  // Per document (U6.3: cleared on every new document).
  private seen: string[] = []
  private seenSet = new Set<string>()
  private helloRequests = 0
  // Per instance.
  private tokens = BRIDGE_BURST
  private refilledAt: number
  private rateBackoff: Backoff | null = null
  /** ⑯'s own `overloaded` window: the same backpressure rule as ⑥ (U6.6, same limit). */
  private inflightBackoff: Backoff | null = null
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
  /** ⑯: in flight for the current document. `document` is the one the task was admitted on. */
  private readonly inflight = new Map<string, { type: string; document: number; abort: AbortController }>()
  /** Replies finished while the document is pending (R2-1). Dropped by a new document and by destroy. */
  private held: Array<{ document: number; go: () => void }> = []
  /** U7.2: intents are refused until this instant after the user declines. */
  private intentCooldownUntil = 0
  private readonly budget: BridgeReadBudget

  constructor(private readonly deps: BridgeDeps) {
    this.viewInstanceId = deps.viewInstanceId ?? randomBytes(16).toString('base64url')
    this.logTag = createHash('sha256').update('kb-view-v1').update('\0').update(this.viewInstanceId).digest('base64url').slice(0, 16)
    this.refilledAt = deps.now()
    this.hostRefilledAt = deps.now()
    this.budget = new BridgeReadBudget({ now: deps.now, setTimer: deps.setTimer, clearTimer: deps.clearTimer })
  }

  isClosed(): boolean {
    return this.closed
  }

  /** The current document's id (decision #37), null between documents. */
  currentDocumentId(): string | null {
    return this.document === null ? null : this.documentId
  }

  /** U6.3: the main frame committed a new document (reloads too). Tables go, and it gets `hello`. */
  newDocument(document: number): void {
    if (this.closed) return
    // The old document's in-flight reads are aborted and none of their replies go out (U6.3).
    this.held = []
    this.failInflight()
    this.document = document
    this.documentId = randomBytes(16).toString('base64url')
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
    // R2-1: a hello still waiting on the host budget is sent after resume.
    this.queuedState = null
  }

  /** A navigation that had started did not commit: the document that is still shown is current again, tables as they were. */
  resumeDocument(document: number): void {
    if (this.closed) return
    this.document = document
    const held = this.held
    this.held = []
    for (const item of held) {
      if (item.document === document) item.go()
    }
    if (this.lastState) this.queueState(this.lastState)
    else this.flush()
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
    this.held = []
    this.failInflight()
    this.budget.destroy()
    if (this.flushTimer !== null) this.deps.clearTimer(this.flushTimer)
    this.flushTimer = null
  }

  /** One request to the bridge path, judged in U6.4's order. */
  receive(arrival: BridgeArrival): void {
    const now = this.deps.now()
    // ①–⑤: silent; from inside the view ②③ are hard violations (U6.6).
    if (this.closed || !arrival.fromView) return this.drop('①', arrival)
    if (arrival.mainFrame === null) return this.drop('⑤', arrival)
    // R2-3: top-level but not the current main frame (bfcache, prerender) is silent.
    if (!arrival.mainFrame && arrival.topLevel === true) return this.drop('⑤', arrival)
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
    // ⑤, the part only the body can say (decision #37): a message of another
    // document — one late from the document this one replaced — goes as
    // silently as one the metadata already placed elsewhere. Before ⑨, so it
    // touches neither the tables nor the counts of this document.
    const documentId = envelopeDocumentId(value)
    if (documentId !== undefined && documentId !== this.documentId) return this.drop('⑤', arrival)
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
    // ⑯ in flight (U6.7). `cancel` takes no slot. A full table is uncounted (§9.3),
    // and the id stays out of the dedupe table (U6.4: only what passes ⑯ is remembered).
    if (message.payload.type !== 'cancel') {
      if (this.inflight.size >= BRIDGE_IN_FLIGHT) return this.inflightFull(document, id)
      // U6.7: at most one return_to_chat_intent in flight. Same ⑯ backpressure as a full table.
      if (message.payload.type === 'return_to_chat_intent' && this.intentInFlight()) return this.inflightFull(document, id)
      // U6.6: within ⑯'s window every arrival under that limit counts, admitted or not.
      const b = this.inflightBackoff
      if (b) {
        const now = this.deps.now()
        if (now < b.until) this.strike(b, now)
        if (this.closed) return
      }
    }
    this.remember(message.id)
    this.handle(message, document)
  }

  // ── handling (04a-2's share) ──────────────────────────────────────────────

  private handle(message: PageMessage, document: number): void {
    const type = message.payload.type
    // `status` is for the host's display only (U7.2); this slice shows nothing of it.
    if (type === 'status') return this.reply(document, message.id, { result: 'done' })
    if (type === 'cancel') return this.cancel(message, document)
    if (type === 'citation') return this.cite(message, document)
    if (type === 'return_to_chat_intent') return this.intend(message, document)
    this.reply(document, message.id, { result: 'rejected', code: 'unavailable', reason: 'not_ready' })
  }

  /** U7.2: the target's `rejected/cancelled` goes out first, then this message's `accepted`. A miss is only `accepted`. */
  private cancel(message: PageMessage, document: number): void {
    if (message.payload.type !== 'cancel') return
    const target = this.inflight.get(message.payload.target_id)
    if (target && target.document === document) {
      this.inflight.delete(message.payload.target_id)
      this.reply(document, message.payload.target_id, { result: 'rejected', code: 'cancelled' })
      target.abort.abort()
    }
    this.reply(document, message.id, { result: 'accepted' })
  }

  private cite(message: PageMessage, document: number): void {
    if (message.payload.type !== 'citation') return
    const citation = this.deps.citation
    if (!citation) return this.reply(document, message.id, { result: 'rejected', code: 'unavailable', reason: 'not_ready' })
    const ref = message.payload.citation
    // U7.2: outside the visible set is the same answer as missing, and no read.
    if (!this.visible(ref)) {
      return this.reply(document, message.id, { result: 'done', code: 'not_found' })
    }
    const abort = new AbortController()
    const generation = this.deps.admission().generation
    this.inflight.set(message.id, { type: 'citation', document, abort })
    this.reply(document, message.id, { result: 'accepted' })
    void this.finishCitation(message.id, document, generation, ref, abort)
  }

  private async finishCitation(id: string, document: number, generation: number, ref: CitationRef, abort: AbortController): Promise<void> {
    const citation = this.deps.citation
    if (!citation) return
    const admittedDocumentId = this.documentId
    // The set may change while the read waits or runs (settings): asked again at the start, before every page and before showing.
    const visible = (): boolean => this.visible(ref)
    const outcome = await this.budget.run((signal) => citation.read(generation, ref, signal, visible), abort.signal)
    // Already removed (cancel, a new document, destroy): that path owns the reply, or there is none.
    if (!this.inflight.has(id)) return
    this.inflight.delete(id)
    if (outcome.kind === 'cancelled') return
    const go = (): void => {
      if (this.closed || this.document !== document || this.documentId !== admittedDocumentId) return
      if (outcome.kind === 'overloaded') {
        this.reply(document, id, { result: 'rejected', code: 'unavailable', reason: 'overloaded', retry_after_ms: BRIDGE_BACKOFF_MS })
        return
      }
      // A read that threw: still one terminal reply, never a page left waiting after `accepted`.
      if (outcome.kind === 'failed') {
        this.reply(document, id, { result: 'done', code: 'internal' })
        return
      }
      const value = outcome.value
      if (value.ok && !visible()) {
        this.reply(document, id, { result: 'done', code: 'not_found' })
        return
      }
      if (value.ok) {
        if (this.deps.admission().generation >= 1) citation.show({ title: value.title, text: value.text, ref })
        this.reply(document, id, { result: 'done' })
        return
      }
      this.reply(document, id, value.reason ? { result: 'done', code: value.code, reason: value.reason } : { result: 'done', code: value.code })
    }
    if (this.document === null) {
      this.held.push({ document, go })
      return
    }
    go()
  }

  /** U7.2: the citation's knowledge base is in the last visible set (settings ∩ the exchange). */
  private visible(ref: CitationRef): boolean {
    const scope = this.deps.citation?.scope() ?? null
    return scope !== null && scope.includes(ref.knowledge_base_id)
  }

  private intentInFlight(): boolean {
    for (const job of this.inflight.values()) if (job.type === 'intent') return true
    return false
  }

  /**
   * U7.2 admission, after ⑯. Activation, cooldown and the room check take no
   * slot and are not violations (§9.3). Anything else is accepted and occupies
   * one in-flight slot (`return_to_chat_intent` ≤ 1, U6.7).
   */
  private intend(message: PageMessage, document: number): void {
    if (message.payload.type !== 'return_to_chat_intent') return
    const intent = this.deps.intent
    if (!intent) return this.reply(document, message.id, { result: 'rejected', code: 'unavailable', reason: 'not_ready' })
    const activated = intent.activatedAt()
    if (activated === null || this.deps.now() - activated > INTENT_ACTIVATION_MS) {
      return this.reply(document, message.id, { result: 'rejected', code: 'forbidden', reason: 'user_activation_required' })
    }
    if (this.deps.now() < this.intentCooldownUntil) {
      return this.reply(document, message.id, { result: 'rejected', code: 'forbidden', reason: 'cooldown' })
    }
    const citations = message.payload.citations
    if (citations.length > intent.draftRoom()) {
      return this.reply(document, message.id, { result: 'rejected', code: 'invalid_request', reason: 'attachment_limit' })
    }
    const abort = new AbortController()
    const generation = this.deps.admission().generation
    this.inflight.set(message.id, { type: 'intent', document, abort })
    this.reply(document, message.id, { result: 'accepted' })
    void this.finishIntent(message.id, document, generation, citations, abort)
  }

  private async finishIntent(id: string, document: number, generation: number, citations: CitationRef[], abort: AbortController): Promise<void> {
    const intent = this.deps.intent
    if (!intent) return
    const admittedDocumentId = this.documentId
    const titles: Array<string | null> = []
    for (const ref of citations) {
      if (!this.inflight.has(id)) return
      if (!this.visible(ref)) { titles.push(null); continue }
      const citation = this.deps.citation
      if (!citation) { titles.push(null); continue }
      const visible = (): boolean => this.visible(ref)
      const outcome = await this.budget.run((signal) => citation.read(generation, ref, signal, visible), abort.signal)
      if (!this.inflight.has(id) || outcome.kind === 'cancelled') return
      if (outcome.kind === 'overloaded') return this.intentOverloaded(id, document, admittedDocumentId)
      titles.push(outcome.kind === 'done' && outcome.value.ok ? plainTitle(outcome.value.title) : null)
    }
    if (!this.inflight.has(id)) return

    const decision = await this.waitForConfirm(id, abort, intent, { count: citations.length, titles })
    if (decision === 'aborted') {
      if (this.inflight.has(id)) this.inflight.delete(id)
      return
    }
    if (decision === 'timeout') {
      this.deliver(document, admittedDocumentId, () => {
        this.reply(document, id, { result: 'rejected', code: 'forbidden', reason: 'confirmation_timeout' })
      })
      return
    }
    if (decision === 'not_ready' || decision === 'unavailable') {
      this.inflight.delete(id)
      this.deliver(document, admittedDocumentId, () => {
        this.reply(document, id, { result: 'rejected', code: 'unavailable', reason: 'not_ready' })
      })
      return
    }
    if (decision === 'declined') {
      this.intentCooldownUntil = this.deps.now() + INTENT_COOLDOWN_MS
      this.inflight.delete(id)
      this.deliver(document, admittedDocumentId, () => {
        this.reply(document, id, { result: 'rejected', code: 'forbidden', reason: 'user_declined' })
      })
      return
    }

    // One result per citation, in citation order: the first failure's code is the one the page gets.
    const results: Array<{ item: DraftItem; ref: CitationRef } | { code: string }> = []
    for (const ref of citations) {
      if (!this.inflight.has(id)) return
      if (!this.visible(ref)) { results.push({ code: 'not_found' }); continue }
      const citation = this.deps.citation
      if (!citation) { results.push({ code: 'not_found' }); continue }
      const visible = (): boolean => this.visible(ref)
      const outcome = await this.budget.run((signal) => citation.read(generation, ref, signal, visible), abort.signal)
      if (!this.inflight.has(id) || outcome.kind === 'cancelled') return
      // §9.3 / U7.2 bridge budget: a read still queued after 10 s ends the intent as `overloaded`, nothing attached.
      if (outcome.kind === 'overloaded') return this.intentOverloaded(id, document, admittedDocumentId)
      if (outcome.kind === 'failed') { results.push({ code: 'internal' }); continue }
      const value = outcome.value
      if (value.ok) {
        results.push({
          item: {
            title: plainTitle(value.title),
            text: value.text,
            ref: { knowledge_base_id: ref.knowledge_base_id, knowledge_id: ref.knowledge_id, chunk_id: ref.chunk_id },
          },
          ref,
        })
        continue
      }
      results.push({ code: value.code })
    }
    // #1624 C1: the set may have changed during a later read; every passed item is asked again just before attaching.
    const passed: DraftItem[] = []
    let firstCode: string | null = null
    for (const result of results) {
      const code = 'code' in result ? result.code : this.visible(result.ref) ? null : 'not_found'
      if (code === null && 'item' in result) passed.push(result.item)
      else if (firstCode === null) firstCode = code
    }
    if (!this.inflight.has(id)) return
    // Terminal from here: a later cancel misses (U7.2, already done once attached).
    this.inflight.delete(id)
    if (passed.length === 0) {
      this.deliver(document, admittedDocumentId, () => {
        this.reply(document, id, firstCode === null ? { result: 'done' } : { result: 'done', code: firstCode })
      })
      return
    }
    const attached = intent.attach(passed)
    this.deliver(document, admittedDocumentId, () => {
      if (!attached) {
        this.reply(document, id, { result: 'rejected', code: 'invalid_request', reason: 'attachment_limit' })
        return
      }
      this.reply(document, id, firstCode === null ? { result: 'done' } : { result: 'done', code: firstCode })
    })
  }

  /** §9.3: a bridge read queued past 10 s. Terminal for the intent, no dialog or attachment, no cooldown. */
  private intentOverloaded(id: string, document: number, admittedDocumentId: string | null): void {
    const job = this.inflight.get(id)
    if (!job) return
    this.inflight.delete(id)
    job.abort.abort()
    this.deliver(document, admittedDocumentId, () => {
      this.reply(document, id, { result: 'rejected', code: 'unavailable', reason: 'overloaded', retry_after_ms: BRIDGE_BACKOFF_MS })
    })
  }

  /** The dialog, the 60s timeout, and the 1s admission poll (U7.2). The signal abort is someone else's reply. */
  private waitForConfirm(
    id: string,
    abort: AbortController,
    intent: NonNullable<BridgeDeps['intent']>,
    ask: IntentAsk,
  ): Promise<'confirmed' | 'declined' | 'unavailable' | 'timeout' | 'not_ready' | 'aborted'> {
    return new Promise((resolve) => {
      let settled = false
      let reason: 'timeout' | 'not_ready' | null = null
      let timeout: unknown = null
      let poll: unknown = null
      const stop = (): void => {
        if (timeout !== null) { this.deps.clearTimer(timeout); timeout = null }
        if (poll !== null) { this.deps.clearTimer(poll); poll = null }
      }
      const finish = (value: 'confirmed' | 'declined' | 'unavailable' | 'timeout' | 'not_ready' | 'aborted'): void => {
        if (settled) return
        settled = true
        abort.signal.removeEventListener('abort', onAbort)
        stop()
        resolve(value)
      }
      const onAbort = (): void => {
        finish(reason === 'timeout' ? 'timeout' : reason === 'not_ready' ? 'not_ready' : 'aborted')
      }
      const pollAdmission = (): void => {
        poll = null
        if (settled || !this.inflight.has(id)) return
        if (this.deps.admission().open) {
          poll = this.deps.setTimer(pollAdmission, INTENT_ADMISSION_POLL_MS)
          return
        }
        reason = 'not_ready'
        this.inflight.delete(id)
        abort.abort()
      }
      abort.signal.addEventListener('abort', onAbort)
      if (abort.signal.aborted) { finish('aborted'); return }
      timeout = this.deps.setTimer(() => {
        timeout = null
        if (settled) return
        if (!this.inflight.has(id)) { finish('aborted'); return }
        reason = 'timeout'
        this.inflight.delete(id)
        abort.abort()
      }, INTENT_CONFIRM_MS)
      poll = this.deps.setTimer(pollAdmission, INTENT_ADMISSION_POLL_MS)
      intent.confirm(ask, abort.signal).then(
        (answer) => finish(answer),
        () => finish(reason === 'timeout' ? 'timeout' : reason === 'not_ready' ? 'not_ready' : 'aborted'),
      )
    })
  }

  /** A terminal reply for `document`, held while a navigation has not committed (U6.3, as a citation's is). */
  private deliver(document: number, admittedDocumentId: string | null, go: () => void): void {
    const run = (): void => {
      if (this.closed || this.document !== document || this.documentId !== admittedDocumentId) return
      go()
    }
    if (this.document === null) {
      this.held.push({ document, go: run })
      return
    }
    run()
  }

  /** Abort every in-flight task and drop it, so a later completion sends nothing. */
  private failInflight(): void {
    const jobs = [...this.inflight.values()]
    this.inflight.clear()
    for (const job of jobs) job.abort.abort()
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

  /**
   * ⑯ full: every such message gets `overloaded` (§9.3, uncounted itself). As
   * for ⑥, three more past the grace and within `retry_after_ms` are one soft
   * violation (U6.6: messages under the same limit).
   */
  private inflightFull(document: number, id: string | undefined): void {
    const now = this.deps.now()
    const b = this.inflightBackoff
    const within = b !== null && now < b.until
    if (!within) this.inflightBackoff = { from: now, until: now + BRIDGE_BACKOFF_MS, strikes: 0, struck: false }
    this.log('⑯', id, 'overloaded')
    // The reply goes first, as for every other refusal: a strike may close the bridge.
    this.reply(document, id, { result: 'rejected', code: 'unavailable', reason: 'overloaded', retry_after_ms: BRIDGE_BACKOFF_MS })
    if (within) this.strike(b!, now)
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
    if (this.closed || document !== this.document || this.documentId === null) return
    const generation = this.deps.admission().generation
    if (generation < 1) {
      this.deps.log(`[knowledge-bridge] view=${this.logTag} not sent: generation`)
      return
    }
    const text = encodeHostMessage(message, newUlid(), generation, this.documentId)
    if (text === null) {
      this.deps.log(`[knowledge-bridge] view=${this.logTag} not sent: ${message.type} failed the host-side check`)
      return
    }
    this.deps.send(text, document)
  }
}
