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
// Lives in shared/ (not renderer/) since review M5 moved the sequencer +
// ring buffer into the MAIN process (main/agentear-log.ts) so the log
// survives the renderer's VoicePanel unmounting on page navigation — both
// main (tsconfig.electron.json) and renderer (tsconfig.renderer.json)
// include src/shared/**.
//
// Pure logic, no timers of its own — the caller drives time via `now` /
// `checkGapTimeouts(now)` so this is deterministic under test (see
// agentearSequencer.test.ts, which replays the vendored AgentEar fixtures
// unmodified).

import type { AgentEarEventEnvelope } from './ipc-types'

export const AGENTEAR_SCHEMA = 'agentear.event/1'

export const DEFAULT_GAP_TIMEOUT_MS = 2000
export const DEFAULT_MAX_BUFFER_PER_SESSION = 32
export const DEFAULT_MAX_SESSIONS = 16
/** Review M3: bound `seen`'s growth — once `nextSeq` has moved at least this
 *  far past a recorded seq, its dedupe/conflict record is pruned. A late
 *  arrival for a seq older than this window is indistinguishable, from the
 *  sequencer's point of view, from one skipped by a gap timeout — both
 *  surface as `late_after_gap`. */
export const SEEN_RETENTION_WINDOW = 64

export type IngestOutcome =
  | 'accepted' // newly accepted (may or may not be immediately deliverable)
  | 'duplicate' // retry of an event already accepted at this (session_id, seq)
  | 'rejected_conflict' // different event_id already occupies this (session_id, seq)
  | 'unknown_version' // `schema` is not a version this sequencer understands
  | 'malformed' // not a well-formed envelope at all (missing required fields)
  | 'buffer_overflow' // session's out-of-order buffer was full; this arrival was dropped, UNRECORDED (a later retry can still be accepted — review M2)
  | 'late_after_gap' // seq < nextSeq but with no `seen` record — arrived after its gap was already skipped (checkGapTimeouts) or its `seen` entry aged out (SEEN_RETENTION_WINDOW)

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
  lateAfterGap: number
  sessionsEvicted: number
}

interface SessionState {
  nextSeq: number
  /** seq -> event_id. Bounded by SEEN_RETENTION_WINDOW (review M3) — pruned
   *  to entries >= nextSeq - SEEN_RETENTION_WINDOW after every delivery, so
   *  this can't grow without bound over a long-lived session. */
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
  private readonly maxSessions: number
  readonly stats: SequencerStats = {
    unknownVersion: 0,
    malformed: 0,
    duplicate: 0,
    conflict: 0,
    gapTimeouts: 0,
    bufferOverflow: 0,
    lateAfterGap: 0,
    sessionsEvicted: 0,
  }

  constructor(opts?: { gapTimeoutMs?: number; maxBufferPerSession?: number; maxSessions?: number }) {
    this.gapTimeoutMs = opts?.gapTimeoutMs ?? DEFAULT_GAP_TIMEOUT_MS
    this.maxBufferPerSession = opts?.maxBufferPerSession ?? DEFAULT_MAX_BUFFER_PER_SESSION
    this.maxSessions = opts?.maxSessions ?? DEFAULT_MAX_SESSIONS
  }

  /** Review M3: `sessions` is an LRU of at most `maxSessions` entries. A
   *  `Map` iterates in insertion order, so "touch" = delete+re-insert (moves
   *  to the MRU end) and "evict" = drop the first key (the LRU end). AgentEar
   *  only ever runs one live session at a time in practice (a new attach
   *  generation gets a new session_id, §5.6) — this bound exists so a long
   *  history of reconnects can't grow `sessions` without limit, not because
   *  concurrent sessions are expected. */
  private sessionFor(id: string): SessionState {
    const existing = this.sessions.get(id)
    if (existing) {
      this.sessions.delete(id)
      this.sessions.set(id, existing)
      return existing
    }
    const fresh: SessionState = { nextSeq: 1, seen: new Map(), buffer: new Map(), gapStartedAt: null }
    this.sessions.set(id, fresh)
    if (this.sessions.size > this.maxSessions) {
      const lruKey = this.sessions.keys().next().value
      if (lruKey !== undefined) {
        this.sessions.delete(lruKey)
        this.stats.sessionsEvicted += 1
      }
    }
    return fresh
  }

  /** Drops `session.seen` entries far enough behind `nextSeq` that a
   *  duplicate/conflict there is no longer worth remembering (review M3). */
  private pruneSeen(session: SessionState): void {
    const threshold = session.nextSeq - SEEN_RETENTION_WINDOW
    if (threshold <= 0) return
    for (const seq of session.seen.keys()) {
      if (seq < threshold) session.seen.delete(seq)
    }
  }

  /** Drain `session.buffer` starting at `session.nextSeq`, advancing it past
   *  every contiguous entry found. Returns the drained envelopes in order.
   *  Review M1: every call here represents `nextSeq` having just advanced
   *  (either by the direct match in `ingest`, or by the skip in
   *  `checkGapTimeouts`) — so `gapStartedAt` is unconditionally rewritten
   *  from `now`, never preserved from a stale, already-resolved gap. */
  private drain(session: SessionState, now: number): AgentEarEventEnvelope[] {
    const out: AgentEarEventEnvelope[] = []
    for (;;) {
      const next = session.buffer.get(session.nextSeq)
      if (!next) break
      session.buffer.delete(session.nextSeq)
      out.push(next)
      session.nextSeq += 1
    }
    session.gapStartedAt = session.buffer.size > 0 ? now : null
    this.pruneSeen(session)
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

    if (envelope.seq === session.nextSeq) {
      // Review M2: record in `seen` only once we're actually KEEPING this
      // event (delivered here, or buffered below) — never for one we're
      // about to drop (buffer_overflow), so a retry of a dropped event isn't
      // mistaken for a duplicate of something that was never delivered.
      session.seen.set(envelope.seq, envelope.event_id)
      session.nextSeq += 1
      const delivered = [envelope, ...this.drain(session, now)]
      return { outcome: 'accepted', delivered }
    }

    if (envelope.seq > session.nextSeq) {
      if (session.buffer.size >= this.maxBufferPerSession) {
        // Design doesn't specify eviction precisely for this edge case (not
        // covered by the vendored fixtures); drop the newcomer rather than
        // an already-buffered one so earlier-arriving events keep priority.
        // Distinct outcome (not `accepted`) and NOT recorded in `seen` —
        // review M2: a retry of this exact event must still be acceptable
        // once buffer space frees up or it becomes the next-in-line.
        this.stats.bufferOverflow += 1
        return { outcome: 'buffer_overflow', delivered: [] }
      }
      session.seen.set(envelope.seq, envelope.event_id)
      session.buffer.set(envelope.seq, envelope)
      session.gapStartedAt ??= now
      return { outcome: 'accepted', delivered: [] }
    }

    // envelope.seq < session.nextSeq with no `seen` hit: reachable after
    // checkGapTimeouts skipped this seq, or after pruneSeen aged its record
    // out (review M1/M3) — NOT unreachable as originally assumed. Counted
    // and reported distinctly rather than silently swallowed as `accepted`.
    this.stats.lateAfterGap += 1
    return { outcome: 'late_after_gap', delivered: [] }
  }

  /** Call periodically (or with a controlled clock in tests) so a session
   *  stuck waiting on a missing seq for more than `gapTimeoutMs` skips it
   *  and delivers whatever is buffered after it (§7.2). Review M1: the skip
   *  goes through the same `drain` as `ingest`, so a second gap right behind
   *  the skipped one gets its OWN fresh `gapTimeoutMs` budget starting now,
   *  rather than inheriting the just-resolved gap's stale start time. */
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
      out.push(...this.drain(session, now))
    }
    return out
  }
}

// ── In-memory event ring buffer (design §7.3: "最近 200 条", never persisted) ──
// Review M5: lives alongside the sequencer since main/agentear-log.ts holds
// both together; the renderer keeps its own capped mirror too (defense in
// depth against a very long-lived mount outliving what the snapshot handed
// it), so this is exported from shared/ rather than duplicated.

export const MAX_EVENTS_IN_MEMORY = 200

/** Appends `items` to `prev`, keeping only the most recent `cap`. */
export function pushCapped(
  prev: AgentEarEventEnvelope[],
  items: AgentEarEventEnvelope[],
  cap: number = MAX_EVENTS_IN_MEMORY,
): AgentEarEventEnvelope[] {
  if (items.length === 0) return prev
  const merged = [...prev, ...items]
  return merged.length > cap ? merged.slice(merged.length - cap) : merged
}
