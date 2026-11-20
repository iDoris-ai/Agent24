// K2 (Loop 3): whether the bound Knowledge server answers. The demo stack was
// down for about 25 hours (11-17) and the app said nothing: answers silently
// came back as 「没找到」. Main asks the server's own readiness call for
// Agent24 (`POST <endpoint>/agent24/knowledge/ready`, the one the daemon's
// probe uses; the demo stack serves no other route) every minute while
// Knowledge is on; the settings page shows the result and a chat turn that
// would use Knowledge says so instead of answering blind.
//
// Nothing here writes a log of the key or of what the server answered.

export type ReachabilityState = 'unknown' | 'ok' | 'unreachable' | 'unauthorized' | 'not_found'

export interface KnowledgeReachabilityView {
  state: ReachabilityState
  /** ISO time of the last check, or null before the first. */
  checkedAt: string | null
}

export interface ProbeInputs {
  /** The configured endpoint, e.g. `http://127.0.0.1:18282/api/v1`. */
  endpoint: string
  knowledgeBaseId: string
  key: string
}

export interface ReachabilityDeps {
  /** What to ask, or null while Knowledge is off or incomplete. */
  inputs: () => ProbeInputs | null
  fetch: (url: string, init: RequestInit) => Promise<Response>
  now?: () => Date
  timeoutMs?: number
}

export const CHECK_EVERY_MS = 60_000
const TIMEOUT_MS = 5_000

/** The answer to one GET of the knowledge base, as a state. */
export function stateOfStatus(status: number): ReachabilityState {
  if (status >= 200 && status < 300) return 'ok'
  if (status === 401 || status === 403) return 'unauthorized'
  if (status === 404) return 'not_found'
  return 'unreachable'
}

export class KnowledgeReachability {
  private view: KnowledgeReachabilityView = { state: 'unknown', checkedAt: null }
  private running: Promise<KnowledgeReachabilityView> | null = null
  private timer: ReturnType<typeof setInterval> | null = null
  private readonly listeners = new Set<(view: KnowledgeReachabilityView) => void>()

  constructor(private readonly deps: ReachabilityDeps) {}

  current(): KnowledgeReachabilityView {
    return { ...this.view }
  }

  onChange(listener: (view: KnowledgeReachabilityView) => void): () => void {
    this.listeners.add(listener)
    return () => { this.listeners.delete(listener) }
  }

  /** One check now; a check already under way is shared, not doubled. */
  check(): Promise<KnowledgeReachabilityView> {
    this.running ??= this.probe().finally(() => { this.running = null })
    return this.running
  }

  start(everyMs: number = CHECK_EVERY_MS): void {
    if (this.timer) return
    void this.check()
    this.timer = setInterval(() => { void this.check() }, everyMs)
    this.timer.unref?.()
  }

  stop(): void {
    if (this.timer) clearInterval(this.timer)
    this.timer = null
  }

  private async probe(): Promise<KnowledgeReachabilityView> {
    const inputs = this.deps.inputs()
    let state: ReachabilityState
    if (inputs === null) {
      state = 'unknown'
    } else {
      const base = inputs.endpoint.replace(/\/+$/, '')
      const timeout = AbortSignal.timeout(this.deps.timeoutMs ?? TIMEOUT_MS)
      const requestId = `k2-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`
      try {
        const res = await this.deps.fetch(`${base}/agent24/knowledge/ready`, {
          method: 'POST',
          headers: { 'X-API-Key': inputs.key, 'Content-Type': 'application/json' },
          body: JSON.stringify({ protocol_version: 1, request_id: requestId, generation: 1 }),
          redirect: 'error',
          signal: timeout,
        })
        state = stateOfStatus(res.status)
        if (state === 'ok') {
          // Answered, but alive only if it says so (the daemon's probe reads the same field).
          const body = (await res.json().catch(() => null)) as { live?: unknown } | null
          if (body?.live !== true) state = 'unreachable'
        } else {
          void res.body?.cancel().catch(() => {})
        }
      } catch {
        state = 'unreachable'
      }
    }
    const next: KnowledgeReachabilityView = {
      state,
      checkedAt: inputs === null ? null : (this.deps.now?.() ?? new Date()).toISOString(),
    }
    const changed = next.state !== this.view.state
    this.view = next
    if (changed) for (const listener of this.listeners) listener(this.current())
    return this.current()
  }
}
