// A3-4 — main-process bridge: subscribes to the daemon's existing
// `GET /api/v1/events` WebSocket (rust/apps/agent24d/src/events.rs; already
// shipping, not touched by this PR) and forwards `agentear.event/1` envelopes
// to the renderer via IpcChannels.AgentEarEvent. The renderer is sandboxed
// (contextIsolation, no direct network) so this main-process hop is the same
// shape as every other backend access in this app (backendProxy).
//
// Design doc: docs/design/A3-ATTACHED-MODULE.md §7.1 — the kernel wraps a
// module's `_a24/events/emit` call as a WS `type:"module"` event with
// `module`/`kind`/`payload` fields, `kind` fixed to `"agentear.event"` for
// this module, `payload` = the full `agentear.event/1` object verbatim. The
// kernel does not validate that inner schema (§7.1: "内核不理解、不校验这份
// schema") — this file only unwraps the envelope; agentearSequencer.ts (the
// renderer side, §7.2) does the real de-dup/ordering/shape validation.
//
// A3-2b/A3-3 (attach registry wiring, the commands route) are not merged as
// of this PR, so in practice no `agentear.event/1` frames will arrive yet —
// this bridge is inert until then, and reconnects quietly in the meantime.

import WebSocket from 'ws'
import { getBackendEndpoint } from './backend-manager'

export const AGENTEAR_MODULE_NAME = 'agentear'
export const AGENTEAR_EVENT_KIND = 'agentear.event'

const INITIAL_BACKOFF_MS = 1000
const MAX_BACKOFF_MS = 30000

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}

/** Doubling backoff capped at 30s — same shape the design doc prescribes for
 *  AgentEar's own reconnect to Agent24 (§5.6); reused here for the desktop's
 *  reconnect to the daemon's WS, a distinct connection the design doesn't
 *  itself specify a backoff for. */
export function nextBackoffMs(current: number): number {
  return Math.min(current * 2, MAX_BACKOFF_MS)
}

/** Unwraps one WS text frame to the inner `agentear.event/1` object, or null
 *  if this frame isn't an agentear module event (wrong type, wrong module,
 *  wrong kind, or not parseable JSON at all). Pure — no I/O, easy to test
 *  without a real socket. */
export function parseAgentEarFrame(raw: string): unknown | null {
  let msg: unknown
  try {
    msg = JSON.parse(raw)
  } catch {
    return null
  }
  if (!isPlainObject(msg) || msg['type'] !== 'module') return null
  const outer = msg['payload']
  if (!isPlainObject(outer)) return null
  if (outer['module'] !== AGENTEAR_MODULE_NAME) return null
  if (outer['kind'] !== AGENTEAR_EVENT_KIND) return null
  return outer['payload'] ?? null
}

/** Reconnecting WS client. Kept deliberately dumb: no queueing, no
 *  persistence (design §7.3/Q4 — Agent24 never persists transcript, and
 *  losing in-flight events across a reconnect is acceptable for this P2
 *  prototype's "recent 200 in memory" panel). */
export class AgentEarEventBridge {
  private socket: WebSocket | null = null
  private stopped = true
  private backoffMs = INITIAL_BACKOFF_MS
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null

  constructor(private readonly onEnvelope: (envelope: unknown) => void) {}

  start(): void {
    if (!this.stopped) return
    this.stopped = false
    this.connect()
  }

  stop(): void {
    this.stopped = true
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }
    this.socket?.removeAllListeners()
    this.socket?.close()
    this.socket = null
  }

  private connect(): void {
    if (this.stopped) return
    const endpoint = getBackendEndpoint()
    if (!endpoint) {
      this.scheduleReconnect()
      return
    }
    const headers: Record<string, string> = {}
    if (endpoint.token) headers['Authorization'] = `Bearer ${endpoint.token}`
    const socket = new WebSocket(`ws://127.0.0.1:${endpoint.port}/api/v1/events`, { headers })
    this.socket = socket

    socket.on('open', () => {
      this.backoffMs = INITIAL_BACKOFF_MS
    })
    socket.on('message', (data: WebSocket.RawData) => {
      const envelope = parseAgentEarFrame(data.toString())
      if (envelope !== null) this.onEnvelope(envelope)
    })
    socket.on('close', () => {
      if (this.socket === socket) this.socket = null
      this.scheduleReconnect()
    })
    socket.on('error', () => {
      // 'close' always follows 'error' for ws — avoid scheduling twice.
    })
  }

  private scheduleReconnect(): void {
    if (this.stopped) return
    this.reconnectTimer = setTimeout(() => {
      this.backoffMs = nextBackoffMs(this.backoffMs)
      this.connect()
    }, this.backoffMs)
  }
}
