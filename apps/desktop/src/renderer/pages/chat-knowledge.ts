// A main-chat turn with Knowledge on (decision 2026-10-30-main-chat-knowledge-path,
// David chose A): `/api/v1/chat` offers no tools, so the turn goes to
// `/api/v1/runs`, the agent loop, where the Knowledge tools are. Main sets the
// run's Knowledge binding on the way (backend-proxy.ts `withKnowledgeBinding`);
// nothing here names one. A run needs a session that exists, so the
// conversation gets a real one first.

import type { RunKnowledge } from './knowledge-grounding'

/** M1 (Loop 5): a 4-bit model answers 「烧开水要多久」 from what it knows, never
 * searching (0/3 on 0.6.72; 9/10 with this said in the turn, 5/10 as a system
 * message, 2/5 in the tool description). Said in front of a plain question. */
export const KNOWLEDGE_HINT = '（知识库已开启：回答事实、数据、做法类问题前，先用 knowledge_search 在知识库里查，'
  + '依据查到的内容回答并注明出处；查不到再说明并按常识回答。闲聊、改写或翻译用户给的文字不用查。）'

/** 「记住…」 is kept as the user said it: personal memory takes only a turn that
 * starts with it (agent24-agent retain.rs), so nothing goes in front. */
const REMEMBER = /^\s*(请你|麻烦你|你帮我|请帮我|帮我|请|麻烦|你)?\s*(记一下|记住|记好|记着)/

/** The run's prompt for a plain turn with Knowledge in effect. */
export function knowledgePrompt(text: string): string {
  return REMEMBER.test(text) ? text : `${KNOWLEDGE_HINT}\n${text}`
}

export const POLL_MS = 1_000
/** As long as `/api/v1/chat` may take for one reply (backend-proxy.ts). */
export const MAX_WAIT_MS = 10 * 60_000

type Proxy = Window['agent24']['backendProxy']

/** `knowledge`: what the run's Knowledge tools did, as the daemon tells it
 * (null when the run had no binding). */
export type KnowledgeTurn =
  | { kind: 'answer'; text: string; knowledge: RunKnowledge | null; runId: string }
  | { kind: 'failed'; code: string; message: string }
  /** M4 A4: the turn stopped waiting, the run goes on: it is the
   *  conversation's task from now on, its card shows how it does. */
  | { kind: 'ongoing'; runId: string; status: string }

export interface TurnDeps {
  proxy: Proxy
  /** M4 A2: told the run's id once the daemon has made it. */
  onRunStarted?: (runId: string) => void
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

/** Asks `prompt` as a run in `sessionId` and waits for its answer.
 * `headroomConfirmed`: the user confirmed iDoris's reserved memory for this
 * turn (RunCreate `idoris_headroom_confirmed`), as on `/api/v1/chat`. */
export async function askWithKnowledge(
  prompt: string,
  sessionId: string,
  deps: TurnDeps,
  headroomConfirmed = false,
  skillId: string | null = null,
): Promise<KnowledgeTurn> {
  const sleep = deps.sleep ?? sleepFor
  const now = deps.now ?? Date.now
  const body: { prompt: string; session_id: string; idoris_headroom_confirmed?: boolean; skill_id?: string } =
    headroomConfirmed ? { prompt, session_id: sessionId, idoris_headroom_confirmed: true } : { prompt, session_id: sessionId }
  // Every round of this conversation names the skill again. The daemon does not remember it.
  if (skillId) body.skill_id = skillId
  const created = await deps.proxy({ method: 'POST', path: '/api/v1/runs', body })
  const runId = (created.data as { id?: unknown } | null)?.id
  if (!created.ok || typeof runId !== 'string') return failure(created.data, 'run_not_started')
  deps.onRunStarted?.(runId)
  const deadline = now() + MAX_WAIT_MS
  let waiting = false
  let last: string | null = null
  while (now() < deadline) {
    await sleep(POLL_MS)
    const res = await deps.proxy({ method: 'GET', path: `/api/v1/runs/${encodeURIComponent(runId)}` })
    if (!res.ok) continue
    const run = res.data as { status?: string; output?: { text?: string } | null; error?: { code?: string; message?: string } | null; knowledge?: RunKnowledge | null }
    if (typeof run.status === 'string') last = run.status
    const approval = run.status === 'awaiting_approval'
    if (approval !== waiting) {
      waiting = approval
      deps.onAwaitingApproval?.(approval)
    }
    if (run.status === 'completed') return { kind: 'answer', text: run.output?.text ?? '', knowledge: run.knowledge ?? null, runId }
    if (run.status === 'failed' || run.status === 'cancelled') return failure(run, `run_${run.status}`)
  }
  // M4 A4: a run still going on is left to go on, as the conversation's
  // task with a card; one never seen is not left running unseen.
  if (last === 'queued' || last === 'running' || last === 'awaiting_approval') return { kind: 'ongoing', runId, status: last }
  await deps.proxy({ method: 'POST', path: `/api/v1/runs/${encodeURIComponent(runId)}/cancel` })
  return { kind: 'failed', code: 'run_timeout', message: 'run_timeout' }
}
