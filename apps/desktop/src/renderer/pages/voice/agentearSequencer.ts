// A3-4 — host-side (session_id, seq) de-dup/reorder for `agentear.event/1`
// (design doc docs/design/A3-ATTACHED-MODULE.md §7.2).
//
// The kernel is a dumb relay: it does not understand this schema, so
// de-duplication and ordering happen here, in the consumer. Rules (§7.2):
//   - unknown `schema` version                                 -> discard, count
//   - same (session_id, seq) + same event_id (retry)           -> discard (accepted, not re-delivered)
//   - same (session_id, seq) + different event_id (protocol violation) -> reject the late one, count
//   - out of order: buffer up to 32 per session, wait at most 2s for the
//     gap, then skip it and deliver what's buffered
//
// Pure logic, no timers of its own — the caller drives time via `now` /
// `checkGapTimeouts(now)` so this is deterministic under test (see
// agentearSequencer.test.ts, which replays the vendored AgentEar fixtures
// unmodified).

import type { AgentEarEventEnvelope } from '../../../shared/ipc-types'

export const AGENTEAR_SCHEMA = 'agentear.event/1'

export const DEFAULT_GAP_TIMEOUT_MS = 2000
export const DEFAULT_MAX_BUFFER_PER_SESSION = 32

export type IngestOutcome =
  | 'accepted' // newly accepted (may or may not be immediately deliverable)
  | 'duplicate' // retry of an event already accepted at this (session_id, seq)
  | 'rejected_conflict' // different event_id already occupies this (session_id, seq)
  | 'unknown_version' // `schema` is not a version this sequencer understands
  | 'malformed' // not a well-formed envelope at all (missing required fields)

export interface IngestResult {
  outcome: IngestOutcome
  /** Envelopes newly released for display by this ingest, in seq order.
   *  Empty unless this ingest closed a gap (or was itself the next-in-line). */
  delivered: AgentEarEventEnvelope[]
}

export interface SequencerStats {
  unknownVersion: number
  malformed: number
  duplicate: number
  conflict: number
  gapTimeouts: number
  bufferOverflow: number
}

interface SessionState {
  nextSeq: number
  /** seq -> event_id, kept for the lifetime of the sequencer (not just while
   *  buffered) so a late conflicting retry after delivery is still caught. */
  seen: Map<number, string>
  buffer: Map<number, AgentEarEventEnvelope>
  /** ms timestamp the current gap (waiting on `nextSeq`) started, or null when
   *  there is no outstanding gap. */
  gapStartedAt: number | null
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}

/** Structural validation only — payload shape by `type` is AgentEar's
 *  contract (schema/agentear.event.v1.schema.json), not something the host
 *  re-validates (design §7.1: "内核不理解、不校验这份 schema"). We only need
 *  the envelope fields the sequencer itself keys on. Returns a plain boolean
 *  (not a type predicate narrowing to AgentEarEventEnvelope): that interface
 *  has no index signature, so TS can't prove every object satisfying this
 *  check structurally matches it — the caller casts after this passes. */
function isWellFormedEnvelope(v: Record<string, unknown>): boolean {
  return (
    typeof v['event_id'] === 'string' && v['event_id'].length > 0 &&
    typeof v['session_id'] === 'string' && v['session_id'].length > 0 &&
    typeof v['seq'] === 'number' && Number.isInteger(v['seq']) && v['seq'] >= 1 &&
    typeof v['type'] === 'string' &&
    isPlainObject(v['payload'])
  )
}

export class AgentEarSequencer {
  private readonly sessions = new Map<string, SessionState>()
  private readonly gapTimeoutMs: number
  private readonly maxBufferPerSession: number
  readonly stats: SequencerStats = {
    unknownVersion: 0,
    malformed: 0,
    duplicate: 0,
    conflict: 0,
    gapTimeouts: 0,
    bufferOverflow: 0,
  }

  constructor(opts?: { gapTimeoutMs?: number; maxBufferPerSession?: number }) {
    this.gapTimeoutMs = opts?.gapTimeoutMs ?? DEFAULT_GAP_TIMEOUT_MS
    this.maxBufferPerSession = opts?.maxBufferPerSession ?? DEFAULT_MAX_BUFFER_PER_SESSION
  }

  private sessionFor(id: string): SessionState {
    let s = this.sessions.get(id)
    if (!s) {
      s = { nextSeq: 1, seen: new Map(), buffer: new Map(), gapStartedAt: null }
      this.sessions.set(id, s)
    }
    return s
  }

  /** Drain `session.buffer` starting at `session.nextSeq`, advancing it past
   *  every contiguous entry found. Returns the drained envelopes in order. */
  private drain(session: SessionState): AgentEarEventEnvelope[] {
    const out: AgentEarEventEnvelope[] = []
    for (;;) {
      const next = session.buffer.get(session.nextSeq)
      if (!next) break
      session.buffer.delete(session.nextSeq)
      out.push(next)
      session.nextSeq += 1
    }
    session.gapStartedAt = session.buffer.size > 0 ? (session.gapStartedAt ?? Date.now()) : null
    return out
  }

  ingest(raw: unknown, now: number = Date.now()): IngestResult {
    if (!isPlainObject(raw)) {
      this.stats.malformed += 1
      return { outcome: 'malformed', delivered: [] }
    }
    if (raw['schema'] !== AGENTEAR_SCHEMA) {
      this.stats.unknownVersion += 1
      return { outcome: 'unknown_version', delivered: [] }
    }
    if (!isWellFormedEnvelope(raw)) {
      this.stats.malformed += 1
      return { outcome: 'malformed', delivered: [] }
    }
    const envelope = raw as unknown as AgentEarEventEnvelope
    const session = this.sessionFor(envelope.session_id)

    const existingEventId = session.seen.get(envelope.seq)
    if (existingEventId !== undefined) {
      if (existingEventId === envelope.event_id) {
        this.stats.duplicate += 1
        return { outcome: 'duplicate', delivered: [] }
      }
      // §7.2: reject the LATE arrival; the first writer at this (session_id,
      // seq) wins. Recorded as a protocol violation, not surfaced to the panel.
      this.stats.conflict += 1
      return { outcome: 'rejected_conflict', delivered: [] }
    }

    session.seen.set(envelope.seq, envelope.event_id)

    if (envelope.seq === session.nextSeq) {
      session.nextSeq += 1
      const delivered = [envelope, ...this.drain(session)]
      return { outcome: 'accepted', delivered }
    }

    if (envelope.seq > session.nextSeq) {
      if (session.buffer.size >= this.maxBufferPerSession) {
        // Design doesn't specify eviction precisely for this edge case (not
        // covered by the vendored fixtures); drop the newcomer rather than
        // an already-buffered one so earlier-arriving events keep priority.
        this.stats.bufferOverflow += 1
        return { outcome: 'accepted', delivered: [] }
      }
      session.buffer.set(envelope.seq, envelope)
      session.gapStartedAt ??= now
      return { outcome: 'accepted', delivered: [] }
    }

    // envelope.seq < session.nextSeq with no `seen` hit is unreachable: any
    // seq below nextSeq was necessarily delivered (and thus recorded in
    // `seen`) on the way past. Defensive fallback: treat as accepted no-op.
    return { outcome: 'accepted', delivered: [] }
  }

  /** Call periodically (or with a controlled clock in tests) so a session
   *  stuck waiting on a missing seq for more than `gapTimeoutMs` skips it
   *  and delivers whatever is buffered after it (§7.2). */
  checkGapTimeouts(now: number = Date.now()): AgentEarEventEnvelope[] {
    const out: AgentEarEventEnvelope[] = []
    for (const session of this.sessions.values()) {
      if (session.gapStartedAt === null) continue
      if (now - session.gapStartedAt < this.gapTimeoutMs) continue
      if (session.buffer.size === 0) {
        session.gapStartedAt = null
        continue
      }
      this.stats.gapTimeouts += 1
      // Skip straight to the earliest buffered seq, then drain contiguously.
      const bufferedSeqs = [...session.buffer.keys()].sort((a, b) => a - b)
      session.nextSeq = bufferedSeqs[0]!
      out.push(...this.drain(session))
    }
    return out
  }
}
