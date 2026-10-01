// A3-4 — thin typed wrappers for the voice panel, mirroring the style of
// ../agent/api.ts: every call goes through window.agent24.backendProxy (the
// renderer is sandboxed, no direct fetch). Endpoints per design doc
// docs/design/A3-ATTACHED-MODULE.md §3.2 (GET /api/v1/attached) and §6.1
// (POST /api/v1/os/{name}/commands/{command}). A3-2b/A3-3 (live generation,
// the commands route) are not merged as of this PR — this file is written
// against the frozen wire shape; the desktop tests exercise it with mocks.

import type { AgentEarEventEnvelope, AttachedView } from '../../../shared/ipc-types'
import { errorMessage } from '../agent/api'

export const AGENTEAR_MODULE_NAME = 'agentear'
export const AGENTEAR_COMMAND_SCHEMA = 'agentear.command/1'

export type { AgentEarEventEnvelope, AttachedView }

/** GET /api/v1/attached, filtered to the one module this panel cares about.
 *  Returns null when the module has never been registered (not an error —
 *  AgentEar may simply not be paired yet). */
export async function getAgentEarAttachment(): Promise<AttachedView | null> {
  const res = await window.agent24.backendProxy({ method: 'GET', path: '/api/v1/attached' })
  if (!res.ok) throw new Error(errorMessage(res))
  const data = res.data as { modules?: AttachedView[] } | null
  const modules = data?.modules ?? []
  return modules.find((m) => m.name === AGENTEAR_MODULE_NAME) ?? null
}

// Monotonic-enough unique id generator for command_id (design §6.3: the
// panel is the caller here, not AgentEar's own dedupe cache, but a command_id
// that collided across clicks would make AgentEar's dedupe silently swallow
// the second command). Avoids relying on crypto.randomUUID's availability
// across every renderer test environment.
let commandIdCounter = 0
export function genCommandId(): string {
  commandIdCounter += 1
  return `cmd_${Date.now().toString(36)}_${commandIdCounter}_${Math.random().toString(36).slice(2, 8)}`
}

// ── Retry-reuse of command_id (optional, design §6.3) ───────────────────────
// "调用方拿到 504/502 connection_lost 时可用同一 command_id 重试" — the RESULT
// of that specific attempt is UNKNOWN (the command may or may not have
// reached/executed on AgentEar), so a DELIBERATE retry of the exact same
// content should carry the SAME command_id: AgentEar's own dedupe (§6.3,
// keeps the last 256+ command_ids) then makes a retry that actually landed a
// no-op instead of a second execution.
//
// The memo is populated ONLY AFTER an attempt resolves with an unknown
// result — never up front at call time. This matters for two back-to-back
// calls with identical content that are NOT a retry of anything (e.g. the
// user clicks "停止播放" twice quickly): each must get its own fresh id,
// because neither has failed yet when the other is dispatched. Only once a
// call has actually come back 504/502-connection_lost does the NEXT
// identical-content call reuse that id; any other outcome (success, or a
// definite failure like 400/403/502-module_error) clears the memo instead.
let lastUnknownResultAttempt: { key: string; commandId: string } | null = null

function resultIsUnknown(res: { status: number; data: unknown }): boolean {
  if (res.status === 504) return true // design §6.1 table: timeout, result unknown
  if (res.status === 502) {
    // §6.1 table: 502 covers BOTH `connection_lost` (result unknown, retry
    // may reuse the id) and `module_error` (a definite RPC failure — known,
    // must NOT reuse). Only the former qualifies.
    const code = (res.data as { error?: { code?: unknown } } | null)?.error?.code
    return code === 'connection_lost'
  }
  return false
}

/** Returns the id to send with THIS attempt: the previous attempt's id if it
 *  failed with an unknown result for this exact content, otherwise a fresh
 *  one. Consumes the memo either way — if this new attempt ALSO comes back
 *  unknown, `resolveAttempt` re-populates it for the next one. */
function commandIdFor(key: string): string {
  if (lastUnknownResultAttempt && lastUnknownResultAttempt.key === key) {
    const id = lastUnknownResultAttempt.commandId
    lastUnknownResultAttempt = null
    return id
  }
  return genCommandId()
}

function resolveAttempt(key: string, commandId: string, res: { status: number; data: unknown }): void {
  lastUnknownResultAttempt = resultIsUnknown(res) ? { key, commandId } : null
}

export interface SpeakPayload {
  text: string
  lang: 'zh-CN' | 'en-US' | 'th-TH'
  voice?: string
  style?: string
}

/** POST /api/v1/os/agentear/commands/speak — body is a full `agentear.command/1`
 *  envelope (design §6). Returns the command_id used, so the caller can match
 *  it against later `speech`/`error` events carrying the same id. */
export async function sendSpeak(payload: SpeakPayload): Promise<string> {
  const key = JSON.stringify({ type: 'speak', payload })
  const command_id = commandIdFor(key)
  const res = await window.agent24.backendProxy({
    method: 'POST',
    path: `/api/v1/os/${AGENTEAR_MODULE_NAME}/commands/speak`,
    body: { schema: AGENTEAR_COMMAND_SCHEMA, command_id, type: 'speak', payload },
  })
  resolveAttempt(key, command_id, res)
  if (!res.ok) throw new Error(errorMessage(res))
  return command_id
}

/** POST /api/v1/os/agentear/commands/stop_playback. `session_id` omitted =
 *  stop all playback (schema default). */
export async function sendStopPlayback(sessionId?: string): Promise<string> {
  const payload = sessionId ? { session_id: sessionId } : {}
  const key = JSON.stringify({ type: 'stop_playback', payload })
  const command_id = commandIdFor(key)
  const res = await window.agent24.backendProxy({
    method: 'POST',
    path: `/api/v1/os/${AGENTEAR_MODULE_NAME}/commands/stop_playback`,
    body: { schema: AGENTEAR_COMMAND_SCHEMA, command_id, type: 'stop_playback', payload },
  })
  resolveAttempt(key, command_id, res)
  if (!res.ok) throw new Error(errorMessage(res))
  return command_id
}

/** True for a `proposal` event whose action is `builtin` (design §7.1: these
 *  are already executed locally by AgentEar; the host only ever displays
 *  them, labeled as such, and must never offer an execute action for one —
 *  P3's confirm/execute gate must exclude them for the same reason). */
export function isBuiltinProposal(envelope: AgentEarEventEnvelope): boolean {
  if (envelope.type !== 'proposal') return false
  const action = (envelope.payload as { action?: unknown }).action
  return isPlainObject(action) && action['type'] === 'builtin'
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}

// ── Tier usage (design §7.3 panel scope: "本模块 GET /api/v1/usage?module=agentear
// 的 tier 统计"; endpoint already ships — rust/apps/agent24d/src/routes.rs
// `get_usage`/`ByServed`, not touched by this PR). ──────────────────────────────

export interface UsageCounts {
  calls_ok: number
  calls_failed: number
  calls_cancelled: number
  prompt_tokens: number
  completion_tokens: number
  total_tokens: number
}

export interface ByServedUsage {
  local: UsageCounts
  remote: UsageCounts
  none: UsageCounts
}

export interface ModuleUsageResponse {
  module: string
  totals: UsageCounts
  by_served: ByServedUsage
  daily: unknown[]
  cost_usd: number | null
}

export function totalCalls(c: UsageCounts): number {
  return c.calls_ok + c.calls_failed + c.calls_cancelled
}

export async function getAgentEarUsage(): Promise<ByServedUsage | null> {
  const res = await window.agent24.backendProxy({
    method: 'GET',
    path: `/api/v1/usage?module=${AGENTEAR_MODULE_NAME}`,
  })
  if (!res.ok) throw new Error(errorMessage(res))
  const data = res.data as ModuleUsageResponse | null
  return data?.by_served ?? null
}
