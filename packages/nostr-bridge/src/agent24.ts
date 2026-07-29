// Client for the local agent24d v1 HTTP API — the daemon this bridge drives.
// An inbound agent message becomes a gated run here (same C4/H1–H4 approval gate
// as any other run), and the run's output goes back over Nostr. Mirrors the F3
// wechat-bridge client; deliberately thin.

export interface DaemonEndpoint {
  base: string
  token: string
}

export interface RunResult {
  status: 'completed' | 'failed' | 'cancelled' | 'awaiting_approval' | 'running' | 'queued'
  text?: string
  error?: string
  runId: string
}

export class Agent24Client {
  constructor(private readonly ep: DaemonEndpoint) {}

  private headers(): Record<string, string> {
    const h: Record<string, string> = { 'Content-Type': 'application/json' }
    if (this.ep.token) h.Authorization = `Bearer ${this.ep.token}`
    return h
  }

  async createSession(title: string): Promise<string> {
    const res = await fetch(`${this.ep.base}/api/v1/sessions`, {
      method: 'POST',
      headers: this.headers(),
      body: JSON.stringify({ title, channel: 'nostr' }),
    })
    if (!res.ok) throw new Error(`create session failed: HTTP ${res.status}`)
    const s = (await res.json()) as { id: string }
    return s.id
  }

  /** Start a run and wait (bounded) for a terminal state. `awaiting_approval`
   * means it parked on a human decision — surfaced back over the channel. */
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

  async awaitRun(runId: string, timeoutMs = 600_000, pollMs = 1_500): Promise<RunResult> {
    const deadline = Date.now() + timeoutMs
    while (Date.now() < deadline) {
      const res = await fetch(`${this.ep.base}/api/v1/runs/${runId}`, { headers: this.headers() })
      if (!res.ok) return { status: 'failed', error: `run poll failed: HTTP ${res.status}`, runId }
      const run = (await res.json()) as {
        status: RunResult['status']
        output?: { text: string } | null
        error?: { code: string; message: string } | null
      }
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
          return { status: 'awaiting_approval', runId }
        default:
          await sleep(pollMs)
      }
    }
    return { status: 'running', runId }
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms))
}
