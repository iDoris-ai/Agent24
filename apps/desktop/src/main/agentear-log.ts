// A3-4 — review M5: the sequencer + the "last 200 events" ring buffer live
// HERE, in the main process, not in VoicePanel.tsx. Design §7.2's intent is
// a host-side log; putting it in the renderer meant it reset every time the
// panel unmounted (e.g. navigating to another sidebar page and back), which
// isn't "the host kept a log", it's "the currently-open tab kept a log".
//
// This class owns one AgentEarSequencer + one capped array. main.ts feeds it
// raw (already WS-unwrapped — see agentear-events.ts) `agentear.event/1`
// objects via `ingest()`; the renderer pulls the current state once via the
// `agentear:snapshot` IPC invoke on mount, then receives only the NEW
// envelopes going forward via the existing `AgentEarEvent` push channel
// (main.ts wires `subscribe()`'s callback to `webContents.send`).
//
// Still no persistence anywhere (design §7.3/§8, Q4=a): this is a plain
// in-memory array that's gone the moment agent24d's Electron process exits —
// nothing here writes a file, a log line, or a database row.

import { AgentEarSequencer, pushCapped, MAX_EVENTS_IN_MEMORY } from '../shared/agentearSequencer'
import type { AgentEarEventEnvelope } from '../shared/ipc-types'

export { MAX_EVENTS_IN_MEMORY }

const DEFAULT_GAP_CHECK_INTERVAL_MS = 500

export class AgentEarEventLog {
  private readonly sequencer = new AgentEarSequencer()
  private events: AgentEarEventEnvelope[] = []
  private readonly listeners = new Set<(envelope: AgentEarEventEnvelope) => void>()
  private gapTimer: ReturnType<typeof setInterval> | null = null
  private readonly cap: number

  constructor(opts?: { cap?: number; gapCheckIntervalMs?: number }) {
    this.cap = opts?.cap ?? MAX_EVENTS_IN_MEMORY
    const intervalMs = opts?.gapCheckIntervalMs ?? DEFAULT_GAP_CHECK_INTERVAL_MS
    this.gapTimer = setInterval(() => this.publish(this.sequencer.checkGapTimeouts()), intervalMs)
    // `setInterval` on a Node timer keeps the event loop alive by default —
    // harmless here (main.ts's own lifecycle already keeps the process up),
    // but `unref()` means a bare `node --require` unit test of this class
    // doesn't hang waiting for the timer.
    this.gapTimer.unref?.()
  }

  /** Feed one raw candidate `agentear.event/1` object through the sequencer.
   *  Whatever it newly releases for display is appended to the ring buffer
   *  and fanned out to subscribers (in order). */
  ingest(raw: unknown): void {
    const { delivered } = this.sequencer.ingest(raw)
    this.publish(delivered)
  }

  private publish(delivered: AgentEarEventEnvelope[]): void {
    if (delivered.length === 0) return
    this.events = pushCapped(this.events, delivered, this.cap)
    for (const envelope of delivered) {
      for (const listener of this.listeners) listener(envelope)
    }
  }

  /** Current capped log, oldest first — what `agentear:snapshot` returns. */
  snapshot(): AgentEarEventEnvelope[] {
    return [...this.events]
  }

  /** Called once per newly-delivered envelope, in order, from here on.
   *  Returns an unsubscribe function. */
  subscribe(listener: (envelope: AgentEarEventEnvelope) => void): () => void {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }

  stop(): void {
    if (this.gapTimer) {
      clearInterval(this.gapTimer)
      this.gapTimer = null
    }
  }
}
