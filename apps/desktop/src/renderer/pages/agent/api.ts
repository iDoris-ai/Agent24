// Thin typed wrappers over window.agent24.backendProxy for the v1 agent
// endpoints (runs / schedules / approvals). The renderer is sandboxed, so
// every call goes through the IPC proxy — never a direct fetch.

export type RunStatus =
  | 'queued'
  | 'running'
  | 'awaiting_approval'
  | 'completed'
  | 'failed'
  | 'cancelled'

export interface Run {
  id: string
  session_id: string | null
  status: RunStatus
  input: { prompt: string; model_override?: string | null }
  output?: { text: string } | null
  error?: { code: string; message: string } | null
  usage: { total_tokens: number }
  schedule_id?: string | null
  created_at: string
  started_at?: string | null
  ended_at?: string | null
}

export type ScheduleSpec =
  | { type: 'cron'; expr: string; tz?: string | null }
  | { type: 'every'; secs: number }
  | { type: 'at'; ts: string }

export interface ScheduleAction {
  type: 'agent_run'
  prompt: string
  session_id?: string | null
  model_override?: string | null
}

export interface Schedule {
  id: string
  name: string
  enabled: boolean
  spec: ScheduleSpec
  action: ScheduleAction
  delivery: unknown[]
  last_run_at: string | null
  next_run_at: string | null
  consecutive_failures: number
}

export interface Approval {
  id: string
  run_id: string
  tool_call_id: string
  kind: string
  summary: string
  payload: Record<string, unknown>
  available_decisions: string[]
  status: string
  expires_at: string
  created_at: string
}

export interface Decision {
  type: string
  reason?: string
}

/** Envelope-aware error message extractor for a failed proxy response.
 *  Handles the v1 `{error:{message}}` shape and the desktop IPC fallback's
 *  `{error: "string"}` (connection/proxy failures) before defaulting to the
 *  HTTP status. */
export function errorMessage(res: { status: number; data: unknown }): string {
  const err = (res.data as { error?: unknown } | null)?.error
  if (typeof err === 'string' && err) return err
  const msg = (err as { message?: string } | undefined)?.message
  return msg ?? `HTTP ${res.status}`
}

async function get<T>(path: string, pick: string): Promise<T> {
  const res = await window.agent24.backendProxy({ method: 'GET', path })
  if (!res.ok) throw new Error(errorMessage(res))
  return (res.data as Record<string, T>)[pick]
}

export const listRuns = (): Promise<Run[]> => get<Run[]>('/api/v1/runs', 'runs')

export const getRun = async (id: string): Promise<Run> => {
  const res = await window.agent24.backendProxy({ method: 'GET', path: `/api/v1/runs/${id}` })
  if (!res.ok) throw new Error(errorMessage(res))
  return res.data as Run
}

export const cancelRun = async (id: string): Promise<void> => {
  const res = await window.agent24.backendProxy({
    method: 'POST',
    path: `/api/v1/runs/${id}/cancel`,
  })
  if (!res.ok) throw new Error(errorMessage(res))
}

export const listSchedules = (): Promise<Schedule[]> =>
  get<Schedule[]>('/api/v1/schedules', 'schedules')

export const createSchedule = async (body: {
  name: string
  spec: ScheduleSpec
  action: ScheduleAction
  enabled?: boolean
}): Promise<Schedule> => {
  const res = await window.agent24.backendProxy({ method: 'POST', path: '/api/v1/schedules', body })
  if (!res.ok) throw new Error(errorMessage(res))
  return res.data as Schedule
}

export const deleteSchedule = async (id: string): Promise<void> => {
  const res = await window.agent24.backendProxy({
    method: 'DELETE',
    path: `/api/v1/schedules/${id}`,
  })
  if (!res.ok) throw new Error(errorMessage(res))
}

export const updateSchedule = async (
  id: string,
  patch: Partial<{ name: string; enabled: boolean; spec: ScheduleSpec; action: ScheduleAction }>,
): Promise<Schedule> => {
  const res = await window.agent24.backendProxy({
    method: 'PATCH',
    path: `/api/v1/schedules/${id}`,
    body: patch,
  })
  if (!res.ok) throw new Error(errorMessage(res))
  return res.data as Schedule
}

export const runScheduleNow = async (id: string): Promise<string> => {
  const res = await window.agent24.backendProxy({
    method: 'POST',
    path: `/api/v1/schedules/${id}/run_now`,
  })
  if (!res.ok) throw new Error(errorMessage(res))
  return (res.data as { run_id: string }).run_id
}

export const listPendingApprovals = (): Promise<Approval[]> =>
  get<Approval[]>('/api/v1/approvals?status=pending', 'approvals')

export const decideApproval = async (id: string, decision: Decision): Promise<void> => {
  // Fail-closed at the API boundary: SPEC-002 requires a non-empty reason for
  // deny, and the daemon 400s an empty one. Enforce here so no caller (not
  // just the current UI) can send an invalid deny.
  if (decision.type === 'deny' && (decision.reason ?? '').trim() === '') {
    throw new Error('拒绝需要填写原因')
  }
  const res = await window.agent24.backendProxy({
    method: 'POST',
    path: `/api/v1/approvals/${id}`,
    body: decision,
  })
  // 409 (already resolved) is not fatal — a concurrent resolution won
  if (!res.ok && res.status !== 409) throw new Error(errorMessage(res))
}
