// Hermetic fake of the agent24d v1 HTTP API — the other half of the F3 harness.
// The REAL Agent24Client (which uses fetch) drives this in-process server, so the
// bridge's session/run/approval HTTP paths are exercised end-to-end without a
// running Rust daemon.
//
// Run outcomes are scripted: the test supplies plan(prompt) → RunPlan. An
// `approval` plan parks the run; once the bridge POSTs the decision, the run
// advances to `then` (mirroring H3 durable resume: the same run continues).

import http from 'node:http'
import type { AddressInfo } from 'node:net'

export type RunPlan =
  | { kind: 'completed'; text: string }
  | { kind: 'failed'; error: string }
  | { kind: 'approval'; summary: string; then: RunPlan }

export type Planner = (prompt: string, sessionId?: string) => RunPlan

interface RunState {
  id: string
  status: 'completed' | 'failed' | 'awaiting_approval'
  text?: string
  error?: string
  /** Continuation applied when the parked approval is approved. */
  then?: RunPlan
}

interface ApprovalState {
  id: string
  run_id: string
  summary: string
  available_decisions: string[]
  pending: boolean
}

export interface DecisionRecord {
  approvalId: string
  type: string
}

export class FakeDaemon {
  private readonly server: http.Server
  private readonly runs = new Map<string, RunState>()
  private readonly approvals = new Map<string, ApprovalState>()
  private seq = 0
  /** Prompts received via POST /runs, in order — for assertions. */
  readonly prompts: string[] = []
  /** Decisions received via POST /approvals/{id}, in order. */
  readonly decisions: DecisionRecord[] = []
  port = 0

  constructor(private readonly plan: Planner) {
    this.server = http.createServer((req, res) => void this.handle(req, res))
  }

  async listen(): Promise<this> {
    await new Promise<void>((resolve) => this.server.listen(0, '127.0.0.1', resolve))
    this.port = (this.server.address() as AddressInfo).port
    return this
  }

  get baseUrl(): string {
    return `http://127.0.0.1:${this.port}`
  }

  async close(): Promise<void> {
    await new Promise<void>((resolve, reject) =>
      this.server.close((err) => (err ? reject(err) : resolve())),
    )
  }

  private async handle(req: http.IncomingMessage, res: http.ServerResponse): Promise<void> {
    const method = req.method ?? 'GET'
    const path = (req.url ?? '').split('?')[0] ?? ''
    const body = await readJson(req)

    if (method === 'POST' && path === '/api/v1/sessions') {
      send(res, { id: `sess-${++this.seq}` })
      return
    }
    if (method === 'POST' && path === '/api/v1/runs') {
      send(res, { id: this.createRun(body as { prompt?: string; session_id?: string }) })
      return
    }
    if (method === 'GET' && path.startsWith('/api/v1/runs/')) {
      const run = this.runs.get(path.slice('/api/v1/runs/'.length))
      if (!run) return send(res, { status: 'failed', error: { code: 'not_found', message: 'no run' } }, 404)
      send(res, { status: run.status, output: run.text != null ? { text: run.text } : null, error: run.error ? { code: 'run_failed', message: run.error } : null })
      return
    }
    if (method === 'GET' && path.startsWith('/api/v1/approvals')) {
      const approvals = [...this.approvals.values()]
        .filter((a) => a.pending)
        .map((a) => ({ id: a.id, run_id: a.run_id, summary: a.summary, available_decisions: a.available_decisions }))
      send(res, { approvals })
      return
    }
    if (method === 'POST' && path.startsWith('/api/v1/approvals/')) {
      this.decide(path.slice('/api/v1/approvals/'.length), body as { type?: string })
      send(res, { ok: true })
      return
    }
    send(res, {}, 404)
  }

  private createRun(body: { prompt?: string; session_id?: string }): string {
    const prompt = body.prompt ?? ''
    this.prompts.push(prompt)
    const id = `run-${++this.seq}`
    const run: RunState = { id, status: 'completed' }
    this.applyPlan(run, this.plan(prompt, body.session_id))
    this.runs.set(id, run)
    return id
  }

  private applyPlan(run: RunState, plan: RunPlan): void {
    switch (plan.kind) {
      case 'completed':
        run.status = 'completed'
        run.text = plan.text
        run.error = undefined
        run.then = undefined
        return
      case 'failed':
        run.status = 'failed'
        run.error = plan.error
        run.text = undefined
        run.then = undefined
        return
      case 'approval': {
        run.status = 'awaiting_approval'
        run.then = plan.then
        const id = `ap-${++this.seq}`
        this.approvals.set(id, {
          id,
          run_id: run.id,
          summary: plan.summary,
          available_decisions: ['approve', 'deny'],
          pending: true,
        })
        return
      }
    }
  }

  private decide(approvalId: string, body: { type?: string }): void {
    const type = body.type ?? 'deny'
    this.decisions.push({ approvalId, type })
    const approval = this.approvals.get(approvalId)
    if (!approval || !approval.pending) return
    approval.pending = false
    const run = this.runs.get(approval.run_id)
    if (!run) return
    if (type === 'approve' && run.then) {
      this.applyPlan(run, run.then) // resume: the same run continues (H3)
    } else if (type !== 'approve') {
      run.status = 'completed'
      run.text = '' // denied step: run ends without further output
      run.then = undefined
    }
  }
}

function readJson(req: http.IncomingMessage): Promise<unknown> {
  return new Promise((resolve) => {
    let raw = ''
    req.on('data', (c) => (raw += c))
    req.on('end', () => {
      try {
        resolve(raw ? JSON.parse(raw) : {})
      } catch {
        resolve({})
      }
    })
  })
}

function send(res: http.ServerResponse, payload: unknown, status = 200): void {
  res.writeHead(status, { 'Content-Type': 'application/json' })
  res.end(JSON.stringify(payload))
}
