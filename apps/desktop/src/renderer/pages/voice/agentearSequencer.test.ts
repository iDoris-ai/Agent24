// @vitest-environment node
import { describe, it, expect, beforeAll } from 'vitest'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import {
  AgentEarSequencer,
  DEFAULT_GAP_TIMEOUT_MS,
  DEFAULT_MAX_BUFFER_PER_SESSION,
} from './agentearSequencer'
import type { AgentEarEventEnvelope } from '../../../shared/ipc-types'

// Fixtures are the AgentEar contracts/ directory, vendored verbatim (design
// §7.2: "vendored 副本 + 记录 commit", not a live cross-repo read) — see
// apps/desktop/test/fixtures/agentear/SOURCE for the pinned commit.
const FIXTURES_ROOT = resolve(__dirname, '../../../../test/fixtures/agentear')
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
      // duplicate retries are NOT re-added, matching fixture's expectation
      // that evt_0002 appears once in accepted_event_ids despite arriving twice
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
    // no duplicates in the delivered stream
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
    // seq 1-3 delivered immediately; 5-9 buffered behind the missing seq 4
    expect(delivered.map((e) => e.seq)).toEqual([1, 2, 3])

    // after the gap times out, the rest is delivered in order — none lost
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
    // out of order, with a couple of exact retries mixed in
    const input = [mk(3), mk(1), mk(2), mk(2), mk(1), mk(5), mk(4)]
    const delivered: AgentEarEventEnvelope[] = []
    for (const ev of input) delivered.push(...s.ingest(ev).delivered)
    expect(delivered.map((e) => e.seq)).toEqual([1, 2, 3, 4, 5])
    expect(new Set(delivered.map((e) => e.seq)).size).toBe(5)
  })

  it('a gap that never fills does not block later, already-contiguous events forever — it times out', () => {
    const s = new AgentEarSequencer({ gapTimeoutMs: 100 })
    // seq 1 arrives, seq 2 never arrives, seq 3 arrives and must wait
    const r1 = s.ingest(mk(1))
    expect(r1.delivered.map((e) => e.seq)).toEqual([1])
    const r3 = s.ingest(mk(3), 0)
    expect(r3.delivered).toEqual([]) // buffered, waiting on seq 2

    // before timeout: still nothing new
    expect(s.checkGapTimeouts(50)).toEqual([])

    // after the 100ms gap timeout: skip seq 2, deliver seq 3
    const flushed = s.checkGapTimeouts(200)
    expect(flushed.map((e) => e.seq)).toEqual([3])
    expect(s.stats.gapTimeouts).toBe(1)
  })

  it('does not block display of later events while a gap is outstanding — only that seq is withheld', () => {
    const s = new AgentEarSequencer()
    s.ingest(mk(1))
    // seq 3 and 4 arrive before seq 2 — both buffered, not delivered yet
    const r3 = s.ingest(mk(3))
    const r4 = s.ingest(mk(4))
    expect(r3.delivered).toEqual([])
    expect(r4.delivered).toEqual([])
    // seq 2 arrives — 2, 3, 4 all release together, in order
    const r2 = s.ingest(mk(2))
    expect(r2.delivered.map((e) => e.seq)).toEqual([2, 3, 4])
  })

  it('honors the documented defaults (2s gap timeout, 32-entry buffer)', () => {
    expect(DEFAULT_GAP_TIMEOUT_MS).toBe(2000)
    expect(DEFAULT_MAX_BUFFER_PER_SESSION).toBe(32)
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
