import { describe, it, expect, beforeAll } from 'vitest'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import {
  AgentEarSequencer,
  DEFAULT_GAP_TIMEOUT_MS,
  DEFAULT_MAX_BUFFER_PER_SESSION,
  DEFAULT_MAX_SESSIONS,
  SEEN_RETENTION_WINDOW,
  pushCapped,
} from './agentearSequencer'
import type { AgentEarEventEnvelope } from './ipc-types'

// Fixtures are the AgentEar contracts/ directory, vendored verbatim (design
// §7.2: "vendored 副本 + 记录 commit", not a live cross-repo read) — see
// apps/desktop/test/fixtures/agentear/SOURCE for the pinned commit.
const FIXTURES_ROOT = resolve(__dirname, '../../test/fixtures/agentear')
const CONTRACTS = resolve(FIXTURES_ROOT, 'contracts')

function readJson(relPath: string): unknown {
  return JSON.parse(readFileSync(resolve(CONTRACTS, relPath), 'utf8'))
}

interface SequenceFixture {
  description: string
  events: AgentEarEventEnvelope[]
  expect: {
    accepted_event_ids?: string[]
    rejected_event_ids?: string[]
    delivered_seq?: number[]
  }
}

// First assertion (design §7.2): the vendor copy exists and is pinned to a
// real 40-char commit sha, not floating on a branch head.
beforeAll(() => {
  const source = readFileSync(resolve(FIXTURES_ROOT, 'SOURCE'), 'utf8')
  const match = /source_commit:\s*([0-9a-f]+)/.exec(source)
  expect(match).not.toBeNull()
  expect(match![1]).toHaveLength(40)
})

describe('AgentEarSequencer — vendored AgentEar fixtures', () => {
  it('dedupe_retry.json: a retried event_id/seq is accepted once, not duplicated', () => {
    const fx = readJson('fixtures/sequences/dedupe_retry.json') as SequenceFixture
    const seq = new AgentEarSequencer()
    const acceptedIds: string[] = []
    for (const ev of fx.events) {
      const r = seq.ingest(ev)
      if (r.outcome === 'accepted') acceptedIds.push(ev.event_id)
    }
    expect(acceptedIds).toEqual(fx.expect.accepted_event_ids)
    expect(seq.stats.duplicate).toBe(1)
  })

  it('reorder.json: out-of-order arrivals are delivered in seq order with no duplicates', () => {
    const fx = readJson('fixtures/sequences/reorder.json') as SequenceFixture
    const seq = new AgentEarSequencer()
    const delivered: AgentEarEventEnvelope[] = []
    for (const ev of fx.events) {
      const r = seq.ingest(ev)
      delivered.push(...r.delivered)
    }
    expect(delivered.map((e) => e.seq)).toEqual(fx.expect.delivered_seq)
    expect(new Set(delivered.map((e) => e.seq)).size).toBe(delivered.length)
  })

  it('seq_conflict.json: the late arrival at an occupied (session_id, seq) is rejected', () => {
    const fx = readJson('fixtures/sequences/seq_conflict.json') as SequenceFixture
    const seq = new AgentEarSequencer()
    const accepted: string[] = []
    const rejected: string[] = []
    for (const ev of fx.events) {
      const r = seq.ingest(ev)
      if (r.outcome === 'accepted' || r.outcome === 'duplicate') accepted.push(ev.event_id)
      if (r.outcome === 'rejected_conflict') rejected.push(ev.event_id)
    }
    expect(accepted).toEqual(fx.expect.accepted_event_ids)
    expect(rejected).toEqual(fx.expect.rejected_event_ids)
  })

  it('event/invalid/unknown_version.json is discarded as unknown_version, not delivered', () => {
    const bad = readJson('fixtures/event/invalid/unknown_version.json')
    const seq = new AgentEarSequencer()
    const r = seq.ingest(bad)
    expect(r.outcome).toBe('unknown_version')
    expect(r.delivered).toEqual([])
    expect(seq.stats.unknownVersion).toBe(1)
  })

  it('a full run of valid event fixtures is all accepted; seq 4 is intentionally absent from this fixture set, so it buffers behind the gap until the gap times out', () => {
    // These are independent AgentEar contract SAMPLES (one per event shape),
    // not a recorded session — seq jumps 3 -> 5 (no seq 4 fixture exists).
    // That's exactly the "gap that must not block forever" case §7.2 covers:
    // everything still gets ACCEPTED, but delivery of seq >= 5 waits for the
    // gap timeout (checkGapTimeouts) rather than never showing up.
    const files = [
      'fixtures/event/valid/turn_listening.json',
      'fixtures/event/valid/transcript_zh.json',
      'fixtures/event/valid/transcript_with_hash.json',
      'fixtures/event/valid/proposal.json',
      'fixtures/event/valid/speech_started_host.json',
      'fixtures/event/valid/speech_completed_turn.json',
      'fixtures/event/valid/speech_failed.json',
      'fixtures/event/valid/error_host_timeout.json',
    ]
    const seq = new AgentEarSequencer({ gapTimeoutMs: 100 })
    const delivered: AgentEarEventEnvelope[] = []
    for (const f of files) {
      const ev = readJson(f) as AgentEarEventEnvelope
      const r = seq.ingest(ev, 0)
      expect(r.outcome).toBe('accepted')
      delivered.push(...r.delivered)
    }
    expect(delivered.map((e) => e.seq)).toEqual([1, 2, 3])

    delivered.push(...seq.checkGapTimeouts(1000))
    expect(delivered).toHaveLength(files.length)
    expect(delivered.map((e) => e.seq)).toEqual([1, 2, 3, 5, 6, 7, 8, 9])
  })
})

describe('AgentEarSequencer — de-dup and reorder invariants (out-of-order + duplicate seq input)', () => {
  const SID = 'ses_x'
  const mk = (seq: number, eventId = `evt_${seq}`): AgentEarEventEnvelope => ({
    schema: 'agentear.event/1',
    event_id: eventId,
    session_id: SID,
    seq,
    type: 'turn',
    payload: { phase: 'listening' },
  })

  it('shuffled + duplicated seq input is delivered sorted with no duplicates', () => {
    const s = new AgentEarSequencer()
    const input = [mk(3), mk(1), mk(2), mk(2), mk(1), mk(5), mk(4)]
    const delivered: AgentEarEventEnvelope[] = []
    for (const ev of input) delivered.push(...s.ingest(ev).delivered)
    expect(delivered.map((e) => e.seq)).toEqual([1, 2, 3, 4, 5])
    expect(new Set(delivered.map((e) => e.seq)).size).toBe(5)
  })

  it('a gap that never fills does not block later, already-contiguous events forever — it times out', () => {
    const s = new AgentEarSequencer({ gapTimeoutMs: 100 })
    const r1 = s.ingest(mk(1))
    expect(r1.delivered.map((e) => e.seq)).toEqual([1])
    const r3 = s.ingest(mk(3), 0)
    expect(r3.delivered).toEqual([])

    expect(s.checkGapTimeouts(50)).toEqual([])

    const flushed = s.checkGapTimeouts(200)
    expect(flushed.map((e) => e.seq)).toEqual([3])
    expect(s.stats.gapTimeouts).toBe(1)
  })

  it('does not block display of later events while a gap is outstanding — only that seq is withheld', () => {
    const s = new AgentEarSequencer()
    s.ingest(mk(1))
    const r3 = s.ingest(mk(3))
    const r4 = s.ingest(mk(4))
    expect(r3.delivered).toEqual([])
    expect(r4.delivered).toEqual([])
    const r2 = s.ingest(mk(2))
    expect(r2.delivered.map((e) => e.seq)).toEqual([2, 3, 4])
  })

  it('honors the documented defaults (2s gap timeout, 32-entry buffer, 16 sessions)', () => {
    expect(DEFAULT_GAP_TIMEOUT_MS).toBe(2000)
    expect(DEFAULT_MAX_BUFFER_PER_SESSION).toBe(32)
    expect(DEFAULT_MAX_SESSIONS).toBe(16)
  })

  it('a duplicate of an already-delivered seq is a no-op, not a re-delivery', () => {
    const s = new AgentEarSequencer()
    s.ingest(mk(1, 'evt_a'))
    const dup = s.ingest(mk(1, 'evt_a'))
    expect(dup.outcome).toBe('duplicate')
    expect(dup.delivered).toEqual([])
  })

  it('malformed input (missing required fields) is rejected, not thrown', () => {
    const s = new AgentEarSequencer()
    expect(s.ingest(null).outcome).toBe('malformed')
    expect(s.ingest({ schema: 'agentear.event/1' }).outcome).toBe('malformed')
    expect(s.ingest('not an object').outcome).toBe('malformed')
  })
})

describe('AgentEarSequencer — M1: gap timer resets per-gap, not carried over from a resolved gap', () => {
  const SID = 'ses_gap'
  const mk = (seq: number, eventId = `evt_${seq}`): AgentEarEventEnvelope => ({
    schema: 'agentear.event/1', event_id: eventId, session_id: SID, seq, type: 'turn', payload: {},
  })

  it('two adjacent gaps each get their own full timeout budget', () => {
    const s = new AgentEarSequencer({ gapTimeoutMs: 2000 })
    // T=0: seq1 delivered directly. seq3 and seq6 buffered (gap waiting on seq2), started at T=0.
    s.ingest(mk(1), 0)
    s.ingest(mk(3), 0)
    s.ingest(mk(6), 0)

    // T=1500 (1.5s into gap #1): seq2 arrives, resolving gap #1 (delivers 2,3)
    // and immediately opening gap #2 (waiting on seq4/5, blocking seq6).
    const r2 = s.ingest(mk(2), 1500)
    expect(r2.delivered.map((e) => e.seq)).toEqual([2, 3])

    // T=3000: only 1.5s have passed since gap #2 started at T=1500 — WITHOUT
    // the fix, gap #2 would incorrectly inherit gap #1's T=0 start and look
    // like it has been open for 3s, wrongly timing out here.
    expect(s.checkGapTimeouts(3000)).toEqual([])
    expect(s.stats.gapTimeouts).toBe(0)

    // T=3600: now a full 2.1s have passed since gap #2 actually started (T=1500).
    const flushed = s.checkGapTimeouts(3600)
    expect(flushed.map((e) => e.seq)).toEqual([6])
    expect(s.stats.gapTimeouts).toBe(1)
  })

  it('a late arrival for a seq already skipped by a gap timeout is reported as late_after_gap, not silently accepted', () => {
    const s = new AgentEarSequencer({ gapTimeoutMs: 100 })
    s.ingest(mk(1), 0)
    s.ingest(mk(4), 0) // buffers, waiting on 2 and 3
    const flushed = s.checkGapTimeouts(200)
    expect(flushed.map((e) => e.seq)).toEqual([4]) // 2,3 skipped

    // seq2 (one of the skipped ones) now shows up late.
    const late = s.ingest(mk(2), 250)
    expect(late.outcome).toBe('late_after_gap')
    expect(late.delivered).toEqual([])
    expect(s.stats.lateAfterGap).toBe(1)
  })
})

describe('AgentEarSequencer — M2: buffer_overflow does not poison a later retry', () => {
  const SID = 'ses_overflow'
  const mk = (seq: number, eventId = `evt_${seq}`): AgentEarEventEnvelope => ({
    schema: 'agentear.event/1', event_id: eventId, session_id: SID, seq, type: 'turn', payload: {},
  })

  it('an event dropped for buffer overflow is still accepted when it later matches nextSeq', () => {
    const s = new AgentEarSequencer({ maxBufferPerSession: 2 })
    // nextSeq=1. seq2, seq3 fill the 2-slot buffer.
    expect(s.ingest(mk(2)).outcome).toBe('accepted')
    expect(s.ingest(mk(3)).outcome).toBe('accepted')
    // seq4 overflows — buffer already at capacity (2).
    const overflowed = s.ingest(mk(4, 'evt_4_first_attempt'))
    expect(overflowed.outcome).toBe('buffer_overflow')
    expect(s.stats.bufferOverflow).toBe(1)

    // seq1 arrives, draining 1,2,3 — nextSeq becomes 4, buffer now empty.
    const r1 = s.ingest(mk(1))
    expect(r1.delivered.map((e) => e.seq)).toEqual([1, 2, 3])

    // Retrying the SAME event (same event_id) that overflowed earlier must
    // be accepted now, not swallowed as a "duplicate" of something that was
    // actually never delivered (the bug this closes: seen.set happened
    // before the overflow check, poisoning every future retry).
    const retry = s.ingest(mk(4, 'evt_4_first_attempt'))
    expect(retry.outcome).toBe('accepted')
    expect(retry.delivered.map((e) => e.seq)).toEqual([4])
  })

  it('buffer_overflow is a distinct outcome from accepted', () => {
    const s = new AgentEarSequencer({ maxBufferPerSession: 1 })
    s.ingest(mk(5))
    const overflowed = s.ingest(mk(6))
    expect(overflowed.outcome).toBe('buffer_overflow')
    expect(overflowed.delivered).toEqual([])
  })
})

describe('AgentEarSequencer — M3: seen/session bounds', () => {
  it('prunes seen entries older than SEEN_RETENTION_WINDOW, reclassifying a stale duplicate as late_after_gap', () => {
    const s = new AgentEarSequencer()
    const SID = 'ses_long'
    const mk = (seq: number): AgentEarEventEnvelope => ({
      schema: 'agentear.event/1', event_id: `evt_${seq}`, session_id: SID, seq, type: 'turn', payload: {},
    })
    // Deliver far enough that seq=1's `seen` entry ages out of the window.
    for (let seq = 1; seq <= SEEN_RETENTION_WINDOW + 5; seq++) s.ingest(mk(seq))

    // A retry of seq=1 (same event_id) can no longer be recognized as a
    // duplicate — its record was pruned — so it's reported as late_after_gap,
    // not incorrectly re-delivered and not crashing.
    const late = s.ingest(mk(1))
    expect(late.outcome).toBe('late_after_gap')
    expect(late.delivered).toEqual([])
  })

  it('does not prune a seen entry still within the retention window — conflict detection keeps working', () => {
    const s = new AgentEarSequencer()
    const SID = 'ses_window'
    const mk = (seq: number, eventId: string): AgentEarEventEnvelope => ({
      schema: 'agentear.event/1', event_id: eventId, session_id: SID, seq, type: 'turn', payload: {},
    })
    for (let seq = 1; seq <= 10; seq++) s.ingest(mk(seq, `evt_${seq}`))
    // seq=10 is well within SEEN_RETENTION_WINDOW of nextSeq=11 — a
    // different event_id at the same seq must still be a conflict.
    const conflict = s.ingest(mk(10, 'evt_10_impostor'))
    expect(conflict.outcome).toBe('rejected_conflict')
  })

  it('evicts the least-recently-used session once maxSessions is exceeded', () => {
    const s = new AgentEarSequencer({ maxSessions: 2 })
    const mk = (session: string, seq: number, eventId: string): AgentEarEventEnvelope => ({
      schema: 'agentear.event/1', event_id: eventId, session_id: session, seq, type: 'turn', payload: {},
    })
    s.ingest(mk('A', 1, 'a1')) // sessions: [A]
    s.ingest(mk('B', 1, 'b1')) // sessions: [A, B] (at capacity)
    s.ingest(mk('C', 1, 'c1')) // over capacity -> evicts LRU (A)
    expect(s.stats.sessionsEvicted).toBe(1)

    // A's state was wiped: re-sending its original seq=1 event is treated as
    // a FRESH session (freshly accepted + delivered again), not a duplicate
    // of the pre-eviction state.
    const rSentAgain = s.ingest(mk('A', 1, 'a1'))
    expect(rSentAgain.outcome).toBe('accepted')
    expect(rSentAgain.delivered.map((e) => e.seq)).toEqual([1])
  })

  it('touching a session keeps it alive, protecting it from eviction ahead of a truly idle one', () => {
    const s = new AgentEarSequencer({ maxSessions: 2 })
    const mk = (session: string, seq: number, eventId: string): AgentEarEventEnvelope => ({
      schema: 'agentear.event/1', event_id: eventId, session_id: session, seq, type: 'turn', payload: {},
    })
    s.ingest(mk('A', 1, 'a1')) // [A]
    s.ingest(mk('B', 1, 'b1')) // [A, B]
    s.ingest(mk('A', 2, 'a2')) // touch A -> [B, A] (B now LRU)
    s.ingest(mk('C', 1, 'c1')) // evicts B, not A

    // A's state survived (a duplicate of a1 is still recognized as such).
    const dupA = s.ingest(mk('A', 1, 'a1'))
    expect(dupA.outcome).toBe('duplicate')

    // B's state did NOT survive (fresh session, seq=1 delivered again).
    const freshB = s.ingest(mk('B', 1, 'b1'))
    expect(freshB.outcome).toBe('accepted')
  })
})

describe('pushCapped — ring buffer unit', () => {
  const mk = (seq: number): AgentEarEventEnvelope => ({
    schema: 'agentear.event/1', event_id: `e${seq}`, session_id: 's', seq, type: 'turn', payload: {},
  })

  it('keeps only the most recent `cap` items', () => {
    let acc: AgentEarEventEnvelope[] = []
    for (let i = 1; i <= 10; i++) acc = pushCapped(acc, [mk(i)], 4)
    expect(acc.map((e) => e.seq)).toEqual([7, 8, 9, 10])
  })

  it('is a no-op for an empty batch', () => {
    const acc = [mk(1)]
    expect(pushCapped(acc, [], 4)).toBe(acc)
  })
})
