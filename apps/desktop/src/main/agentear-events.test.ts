import { describe, it, expect } from 'vitest'
import { parseAgentEarFrame, nextBackoffMs, AGENTEAR_MODULE_NAME, AGENTEAR_EVENT_KIND } from './agentear-events'

const AGENTEAR_ENVELOPE = {
  schema: 'agentear.event/1',
  event_id: 'evt_1',
  session_id: 'ses_1',
  seq: 1,
  type: 'transcript',
  payload: { text: 'hi', lang: 'en-US', final: true },
}

function moduleFrame(overrides: Partial<{ type: string; module: string; kind: string; payload: unknown }> = {}): string {
  return JSON.stringify({
    v: 1,
    seq: 0,
    ts: '2026-09-27T00:00:00Z',
    type: overrides.type ?? 'module',
    payload: {
      module: overrides.module ?? AGENTEAR_MODULE_NAME,
      kind: overrides.kind ?? AGENTEAR_EVENT_KIND,
      payload: 'payload' in overrides ? overrides.payload : AGENTEAR_ENVELOPE,
    },
  })
}

describe('parseAgentEarFrame', () => {
  it('unwraps a well-formed agentear module event to its inner envelope', () => {
    expect(parseAgentEarFrame(moduleFrame())).toEqual(AGENTEAR_ENVELOPE)
  })

  it('returns null for a non-module WS event (e.g. run.started)', () => {
    const frame = JSON.stringify({ v: 1, seq: 0, ts: 't', type: 'run.started', payload: { run_id: 'r1' } })
    expect(parseAgentEarFrame(frame)).toBeNull()
  })

  it('returns null for a module event from a different module', () => {
    expect(parseAgentEarFrame(moduleFrame({ module: 'some-other-module' }))).toBeNull()
  })

  it('returns null for an agentear module event with an unrelated kind', () => {
    expect(parseAgentEarFrame(moduleFrame({ kind: 'agentear.debug' }))).toBeNull()
  })

  it('returns null for unparseable JSON', () => {
    expect(parseAgentEarFrame('{not json')).toBeNull()
  })

  it('returns null for a JSON array or scalar (not an object)', () => {
    expect(parseAgentEarFrame('[1,2,3]')).toBeNull()
    expect(parseAgentEarFrame('"just a string"')).toBeNull()
    expect(parseAgentEarFrame('42')).toBeNull()
  })

  it('returns null when payload is missing or not an object', () => {
    expect(parseAgentEarFrame(JSON.stringify({ type: 'module' }))).toBeNull()
    expect(parseAgentEarFrame(JSON.stringify({ type: 'module', payload: 'x' }))).toBeNull()
  })
})

describe('nextBackoffMs', () => {
  it('doubles', () => {
    expect(nextBackoffMs(1000)).toBe(2000)
    expect(nextBackoffMs(2000)).toBe(4000)
  })

  it('caps at 30s', () => {
    expect(nextBackoffMs(20000)).toBe(30000)
    expect(nextBackoffMs(30000)).toBe(30000)
  })
})
