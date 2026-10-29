// A main-chat turn with Knowledge on (decision 2026-10-30-main-chat-knowledge-path,
// David chose A): `/api/v1/chat` offers no tools, so the turn goes to
// `/api/v1/runs`, the agent loop, where the Knowledge tools are. Main sets the
// run's Knowledge binding on the way (backend-proxy.ts `withKnowledgeBinding`);
// nothing here names one. A run needs a session that exists, so the
// conversation gets a real one first.

export const POLL_MS = 1_000
/** As long as `/api/v1/chat` may take for one reply (backend-proxy.ts). */
export const MAX_WAIT_MS = 10 * 60_000

type Proxy = Window['agent24']['backendProxy']

export type KnowledgeTurn =
  | { kind: 'answer'; text: string }
  | { kind: 'failed'; code: string; message: string }

export interface TurnDeps {
  proxy: Proxy
  /** Told each time the run waits on an approval, and once it no longer does. */
  onAwaitingApproval?: (waiting: boolean) => void
  sleep?: (ms: number) => Promise<void>
  now?: () => number
}

const sleepFor = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms))

function failure(data: unknown, fallback: string): KnowledgeTurn {
  const err = (data as { error?: { code?: string; message?: string } } | null)?.error
  return { kind: 'failed', code: err?.code ?? fallback, message: err?.message ?? fallback }
}

/** A session for the conversation, created once. */
export async function createChatSession(proxy: Proxy): Promise<string | null> {
  const res = await proxy({ method: 'POST', path: '/api/v1/sessions', body: { title: '', channel: 'desktop' } })
  const id = (res.data as { id?: unknown } | null)?.id
  return res.ok && typeof id === 'string' ? id : null
}

/** Asks `prompt` as a run in `sessionId` and waits for its answer. */
export async function askWithKnowledge(prompt: string, sessionId: string, deps: TurnDeps): Promise<KnowledgeTurn> {
  const sleep = deps.sleep ?? sleepFor
  const now = deps.now ?? Date.now
  const created = await deps.proxy({ method: 'POST', path: '/api/v1/runs', body: { prompt, session_id: sessionId } })
  const runId = (created.data as { id?: unknown } | null)?.id
  if (!created.ok || typeof runId !== 'string') return failure(created.data, 'run_not_started')
  const deadline = now() + MAX_WAIT_MS
  let waiting = false
  while (now() < deadline) {
    await sleep(POLL_MS)
    const res = await deps.proxy({ method: 'GET', path: `/api/v1/runs/${encodeURIComponent(runId)}` })
    if (!res.ok) continue
    const run = res.data as { status?: string; output?: { text?: string } | null; error?: { code?: string; message?: string } | null }
    const approval = run.status === 'awaiting_approval'
    if (approval !== waiting) {
      waiting = approval
      deps.onAwaitingApproval?.(approval)
    }
    if (run.status === 'completed') return { kind: 'answer', text: run.output?.text ?? '' }
    if (run.status === 'failed' || run.status === 'cancelled') return failure(run, `run_${run.status}`)
  }
  // Not left running unseen: the turn gave up on it.
  await deps.proxy({ method: 'POST', path: `/api/v1/runs/${encodeURIComponent(runId)}/cancel` })
  return { kind: 'failed', code: 'run_timeout', message: 'run_timeout' }
}
