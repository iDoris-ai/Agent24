import { describe, it, expect, vi } from 'vitest'
import {
  parseAgentEarFrame,
  nextBackoffMs,
  AGENTEAR_MODULE_NAME,
  AGENTEAR_EVENT_KIND,
  AgentEarEventBridge,
} from './agentear-events'

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
  // Confirmed against a real agent24d E2E run (AgentEar session): the exact
  // outer WS wrapper is
  //   {"type":"module","payload":{"module":"agentear","kind":"agentear.event","payload":<envelope>}}
  // — moduleFrame() below reproduces this precise shape (not a guess from
  // the design doc's prose alone).
  it('unwraps a well-formed agentear module event to its inner envelope (real E2E-confirmed outer wrapper shape)', () => {
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

// ── M4: stop() while the socket is still CONNECTING must not throw ─────────
// Real `ws` sockets emit a synchronous 'error' when close()d before the
// handshake finishes. This fake reproduces exactly that behavior (and
// nothing else) so the test exercises the real bug without a real socket.
// Defined inside vi.hoisted() — vi.mock's factory below is hoisted above
// this file's own statements, so a plain `class FakeWebSocket {}` declared
// later would still be in its temporal dead zone when the factory runs.
const FakeWebSocket = vi.hoisted(() => {
  // node:events via require(), not the top-of-file `import` — vi.hoisted's
  // callback runs even before this file's own import bindings are live.
  // eslint-disable-next-line @typescript-eslint/no-var-requires
  const { EventEmitter: EE } = require('node:events') as typeof import('node:events')
  return class FakeWebSocket extends EE {
    static readonly CONNECTING = 0
    static readonly OPEN = 1
    static readonly CLOSED = 3
    readyState = 0
    url: string
    opts?: unknown
    constructor(url: string, opts?: unknown) {
      super()
      this.url = url
      this.opts = opts
    }
    close(): void {
      if (this.readyState === FakeWebSocket.CONNECTING) {
        // Mirrors real `ws`: aborting mid-handshake surfaces as an error.
        // EventEmitter THROWS synchronously here if nothing is listening for
        // 'error' — that's the exact crash M4 fixes.
        this.emit('error', new Error('WebSocket was closed before the connection was established'))
      }
      this.readyState = FakeWebSocket.CLOSED
      this.emit('close')
    }
  }
})

vi.mock('ws', () => ({ default: FakeWebSocket }))
vi.mock('./backend-manager', () => ({
  getBackendEndpoint: () => ({ port: 12345, token: 'test-token' }),
}))

describe('AgentEarEventBridge.stop() — M4', () => {
  it('does not throw when the socket is still CONNECTING', () => {
    const bridge = new AgentEarEventBridge(() => {})
    bridge.start()
    expect(() => bridge.stop()).not.toThrow()
  })

  it('is safe to call twice in a row', () => {
    const bridge = new AgentEarEventBridge(() => {})
    bridge.start()
    bridge.stop()
    expect(() => bridge.stop()).not.toThrow()
  })

  it('does not throw when stop() races an already-OPEN socket either', () => {
    const bridge = new AgentEarEventBridge(() => {})
    bridge.start()
    // Reach into the fake to simulate a completed handshake before stopping.
    const openSocket = (bridge as unknown as { socket: FakeWebSocket | null }).socket
    if (openSocket) openSocket.readyState = FakeWebSocket.OPEN
    expect(() => bridge.stop()).not.toThrow()
  })
})
