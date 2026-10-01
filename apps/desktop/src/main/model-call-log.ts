// ME4-desktop-model-ui — main-process ring buffer for `model.call` WS events
// (agent24d/src/model_callback.rs broadcasts one per completed
// `_a24/model/complete` call). Mirrors agentear-log.ts's shape deliberately:
// a small in-memory-only log (no persistence anywhere, same rule as the
// AgentEar transcript — closing Agent24 loses it), fed by
// AgentEarEventBridge's second callback (main/agentear-events.ts), read by
// the "语音" panel via `model-call:snapshot` (pull, once on mount) and
// `model-call:event` (push, ongoing).
//
// Kept deliberately simpler than AgentEarEventLog: `model.call` events have
// no session/seq to de-dup or reorder — each broadcast is already a complete,
// independent fact — so there is no sequencer here, just a capped array.

import type { ModelCallEnvelope } from '../shared/ipc-types'

/** "最近几条" — small on purpose; this is a live status indicator, not a log. */
export const MAX_MODEL_CALLS_IN_MEMORY = 5

export class ModelCallLog {
  private calls: ModelCallEnvelope[] = []
  private readonly listeners = new Set<(call: ModelCallEnvelope) => void>()

  ingest(call: ModelCallEnvelope): void {
    this.calls = [...this.calls, call].slice(-MAX_MODEL_CALLS_IN_MEMORY)
    for (const listener of this.listeners) listener(call)
  }

  /** Current capped log, oldest first — what `model-call:snapshot` returns. */
  snapshot(): ModelCallEnvelope[] {
    return [...this.calls]
  }

  /** Called once per newly-ingested call, in order. Returns an unsubscribe function. */
  subscribe(listener: (call: ModelCallEnvelope) => void): () => void {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }
}
