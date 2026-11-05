// Client for the local agent24d v1 HTTP API — the daemon this bridge drives.
// Deliberately thin: create a run, wait for it to finish, and (for the approval
// flow) list/decide approvals. Everything a WeChat message needs to become an
// agent run and come back.

import type { components } from '@agent24/api-client'
import { CONFIG, type DaemonEndpoint } from './config.js'

type RunStatus = components['schemas']['RunStatus']
type Run = components['schemas']['Run']

export interface RunResult {
  status: RunStatus
  text?: string
  error?: string
  runId: string
}

export interface PendingApproval {
  id: string
  run_id: string
  summary: string
  available_decisions: string[]
}

export class Agent24Client {
  constructor(private readonly ep: DaemonEndpoint) {}

  private headers(): Record<string, string> {
    // Last line of defence: no bearer, no request (every method builds headers before fetch).
    if (!this.ep.token) throw new Error('agent24d bearer token missing: refusing to send an unauthenticated request')
    const h: Record<string, string> = { 'Content-Type': 'application/json', Authorization: `Bearer ${this.ep.token}` }
    return h
  }

  async createSession(title: string): Promise<string> {
    const res = await fetch(`${this.ep.base}/api/v1/sessions`, {
      method: 'POST',
      headers: this.headers(),
      body: JSON.stringify({ title, channel: 'wechat' }),
    })
    if (!res.ok) throw new Error(`create session failed: HTTP ${res.status}`)
    const s = (await res.json()) as { id: string }
    return s.id
  }

  /** Start a run and wait (bounded) for a terminal state. Returns the outcome;
   * `awaiting_approval` means it parked on a human decision (surface it to WeChat). */
  async runToCompletion(prompt: string, sessionId?: string): Promise<RunResult> {
    const create = await fetch(`${this.ep.base}/api/v1/runs`, {
      method: 'POST',
      headers: this.headers(),
      body: JSON.stringify(sessionId ? { prompt, session_id: sessionId } : { prompt }),
    })
    if (!create.ok) {
      return { status: 'failed', error: `run create failed: HTTP ${create.status}`, runId: '' }
    }
    const run = (await create.json()) as { id: string }
    return this.awaitRun(run.id)
  }

  async awaitRun(runId: string): Promise<RunResult> {
    const deadline = Date.now() + CONFIG.RUN_WAIT_TIMEOUT_MS
    while (Date.now() < deadline) {
      const res = await fetch(`${this.ep.base}/api/v1/runs/${runId}`, { headers: this.headers() })
      if (!res.ok) return { status: 'failed', error: `run poll failed: HTTP ${res.status}`, runId }
      const run = (await res.json()) as Pick<Run, 'status' | 'output' | 'error'>
      switch (run.status) {
        case 'completed':
          return { status: 'completed', text: run.output?.text ?? '', runId }
        case 'failed':
          return {
            status: 'failed',
            error: run.error ? `${run.error.code}: ${run.error.message}` : 'run failed',
            runId,
          }
        case 'cancelled':
          return { status: 'cancelled', runId }
        case 'awaiting_approval':
          // Parked on a human decision — hand back so the bridge can surface it.
          return { status: 'awaiting_approval', runId }
        default:
          await sleep(CONFIG.RUN_POLL_INTERVAL_MS)
      }
    }
    return { status: 'running', runId }
  }

  async pendingApprovals(): Promise<PendingApproval[]> {
    const res = await fetch(`${this.ep.base}/api/v1/approvals?status=pending`, {
      headers: this.headers(),
    })
    if (!res.ok) return []
    const body = (await res.json()) as { approvals?: PendingApproval[] }
    return body.approvals ?? []
  }

  async decide(approvalId: string, decision: string, reason?: string): Promise<boolean> {
    const res = await fetch(`${this.ep.base}/api/v1/approvals/${approvalId}`, {
      method: 'POST',
      headers: this.headers(),
      body: JSON.stringify(reason ? { type: decision, reason } : { type: decision }),
    })
    return res.ok
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms))
}
