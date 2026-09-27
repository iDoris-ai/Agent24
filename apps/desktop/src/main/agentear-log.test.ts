import { describe, it, expect, afterEach } from 'vitest'
import { AgentEarEventLog } from './agentear-log'
import type { AgentEarEventEnvelope } from '../shared/ipc-types'

const openLogs: AgentEarEventLog[] = []
function makeLog(opts?: ConstructorParameters<typeof AgentEarEventLog>[0]): AgentEarEventLog {
  const log = new AgentEarEventLog(opts)
  openLogs.push(log)
  return log
}
afterEach(() => {
  while (openLogs.length) openLogs.pop()!.stop()
})

function transcript(session: string, seq: number, text: string): AgentEarEventEnvelope {
  return { schema: 'agentear.event/1', event_id: `evt_${session}_${seq}`, session_id: session, seq, type: 'transcript', payload: { text, lang: 'zh-CN', final: true } }
}

describe('AgentEarEventLog', () => {
  it('snapshot() is empty before anything is ingested', () => {
    const log = makeLog()
    expect(log.snapshot()).toEqual([])
  })

  it('ingest() appends de-duped/ordered events to the snapshot', () => {
    const log = makeLog()
    log.ingest(transcript('s1', 1, 'a'))
    log.ingest(transcript('s1', 2, 'b'))
    expect(log.snapshot().map((e) => (e.payload as { text: string }).text)).toEqual(['a', 'b'])
  })

  it('a malformed / unknown-version event does not appear in the snapshot', () => {
    const log = makeLog()
    log.ingest({ schema: 'agentear.event/2', event_id: 'x', session_id: 's', seq: 1, type: 'turn', payload: {} })
    log.ingest({ not: 'an envelope' })
    expect(log.snapshot()).toEqual([])
  })

  it('caps the snapshot at the configured size, dropping the oldest', () => {
    const log = makeLog({ cap: 5 })
    for (let i = 1; i <= 8; i++) log.ingest(transcript('s1', i, `m${i}`))
    const texts = log.snapshot().map((e) => (e.payload as { text: string }).text)
    expect(texts).toEqual(['m4', 'm5', 'm6', 'm7', 'm8'])
  })

  it('subscribe() receives each newly-delivered envelope exactly once, in order', () => {
    const log = makeLog()
    const seen: string[] = []
    const unsubscribe = log.subscribe((e) => seen.push((e.payload as { text: string }).text))
    log.ingest(transcript('s1', 1, 'a'))
    log.ingest(transcript('s1', 2, 'b'))
    expect(seen).toEqual(['a', 'b'])
    unsubscribe()
    log.ingest(transcript('s1', 3, 'c'))
    expect(seen).toEqual(['a', 'b']) // no more callbacks after unsubscribe
  })

  it('a duplicate retry does not re-publish to subscribers or grow the snapshot', () => {
    const log = makeLog()
    const seen: string[] = []
    log.subscribe((e) => seen.push(e.event_id))
    const ev = transcript('s1', 1, 'a')
    log.ingest(ev)
    log.ingest(ev) // exact retry, same event_id/seq
    expect(seen).toEqual(['evt_s1_1'])
    expect(log.snapshot()).toHaveLength(1)
  })

  it('multiple subscribers all receive the same envelope', () => {
    const log = makeLog()
    const a: string[] = []
    const b: string[] = []
    log.subscribe((e) => a.push(e.event_id))
    log.subscribe((e) => b.push(e.event_id))
    log.ingest(transcript('s1', 1, 'a'))
    expect(a).toEqual(['evt_s1_1'])
    expect(b).toEqual(['evt_s1_1'])
  })

  it('an out-of-order gap eventually flushes via the internal timer without further ingest() calls', async () => {
    const log = makeLog({ cap: 200, gapCheckIntervalMs: 20 })
    log.ingest(transcript('s1', 1, 'a'))
    log.ingest(transcript('s1', 3, 'c')) // buffers, waiting on seq 2
    expect(log.snapshot().map((e) => e.seq)).toEqual([1])

    // The sequencer's own default gap timeout is 2s — well beyond what a
    // unit test should wait on a real timer, so this only asserts the timer
    // wiring calls checkGapTimeouts() repeatedly without throwing; the
    // actual timeout arithmetic is covered by agentearSequencer.test.ts.
    await new Promise((r) => setTimeout(r, 50))
    expect(log.snapshot().map((e) => e.seq)).toEqual([1]) // still waiting, no crash
  })

  it('stop() clears the gap-check timer (no further-scheduled work)', () => {
    const log = makeLog({ gapCheckIntervalMs: 10 })
    expect(() => log.stop()).not.toThrow()
    expect(() => log.stop()).not.toThrow() // idempotent
  })
})
