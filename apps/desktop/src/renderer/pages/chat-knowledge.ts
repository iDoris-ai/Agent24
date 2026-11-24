// A main-chat turn with Knowledge on (decision 2026-10-30-main-chat-knowledge-path,
// David chose A): `/api/v1/chat` offers no tools, so the turn goes to
// `/api/v1/runs`, the agent loop, where the Knowledge tools are. Main sets the
// run's Knowledge binding on the way (backend-proxy.ts `withKnowledgeBinding`);
// nothing here names one. A run needs a session that exists, so the
// conversation gets a real one first.

import type { RunKnowledge } from './knowledge-grounding'
import { asksAcrossLibraries } from './chat/cross-knowledge'
import { runDocumentsOf, type RunDocuments } from './chat/DocumentsOutcome'

/** M1 (Loop 5): a 4-bit model answered 「烧开水要多久」 from what it knew (0/3 on
 * 0.6.72, 3/5 with a hint to search on 0.6.73). David 2026-11-21: retrieval
 * first. A plain turn names its words as the run's `knowledge_query`, which
 * the daemon searches before the model is asked; this says what came of it. */
export const KNOWLEDGE_HINT = '（知识库已开启：系统已先用这句话在知识库里查过，查到的内容在检索结果里。'
  + '和问题相关就依据它回答并注明出处；不相关或没有检索结果，就按常识回答，不要硬套。回答里不要提工具名。）'

/** 「记住…」 is kept as the user said it: personal memory takes only a turn that
 * starts with it (agent24-agent retain.rs), so nothing goes in front. */
const REMEMBER = /^\s*(请你|麻烦你|你帮我|请帮我|帮我|请|麻烦|你)?\s*(记一下|记住|记好|记着)/

/** The run's prompt for a plain turn with Knowledge in effect. */
export function knowledgePrompt(text: string): string {
  if (REMEMBER.test(text)) return text
  return `${KNOWLEDGE_HINT}\n${asksAcrossLibraries(text) ? `${CROSS_HINT}\n` : ''}${text}`
}

/** L7-K1: a question across libraries (cross-knowledge.ts). Scope is not
 * touched: the search already covers every library the user may use. */
export const CROSS_HINT = '（这句话涉及不止一个知识库：检索结果里每条带 knowledge_base_id，按它分组，'
  + '分别说明各库怎么说，再对照；每一点都要来自检索结果，哪个库没有就写没有，不要编。）'

/** Asked to write something new (「写一篇八百字散文」「帮我起草一段介绍」): the
 * knowledge bases have nothing to add, so nothing is searched first (General,
 * 0.6.75). The model can still search if it wants to. */
const WRITE = /^(请|麻烦)?(你)?(帮我|给我)?(写|起草|创作|编写|编一|作一)/

/** What the daemon searches first (RunCreate `knowledge_query`): the user's
 * own words; none on a 「记住…」 turn or when asked to write something new. */
export function knowledgeQuery(text: string): string | null {
  const q = text.trim()
  return q.length === 0 || REMEMBER.test(q) || WRITE.test(q) ? null : q
}

export const POLL_MS = 1_000
/** As long as `/api/v1/chat` may take for one reply (backend-proxy.ts). */
export const MAX_WAIT_MS = 10 * 60_000

type Proxy = Window['agent24']['backendProxy']

/** `knowledge`: what the run's Knowledge tools did, as the daemon tells it
 * (null when the run had no binding). */
export type KnowledgeTurn =
  | { kind: 'answer'; text: string; knowledge: RunKnowledge | null; runId: string; documents: RunDocuments | null }
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
  query: string | null = null,
): Promise<KnowledgeTurn> {
  const sleep = deps.sleep ?? sleepFor
  const now = deps.now ?? Date.now
  const body: { prompt: string; session_id: string; idoris_headroom_confirmed?: boolean; skill_id?: string; knowledge_query?: string } =
    headroomConfirmed ? { prompt, session_id: sessionId, idoris_headroom_confirmed: true } : { prompt, session_id: sessionId }
  // Every round of this conversation names the skill again. The daemon does not remember it.
  if (skillId) body.skill_id = skillId
  // M1 retrieval first (David 2026-11-21): searched before the model is asked.
  if (query) body.knowledge_query = query
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
    const run = res.data as { status?: string; output?: { text?: string } | null; error?: { code?: string; message?: string } | null; knowledge?: RunKnowledge | null; documents?: unknown }
    if (typeof run.status === 'string') last = run.status
    const approval = run.status === 'awaiting_approval'
    if (approval !== waiting) {
      waiting = approval
      deps.onAwaitingApproval?.(approval)
    }
    if (run.status === 'completed') return { kind: 'answer', text: run.output?.text ?? '', knowledge: run.knowledge ?? null, runId, documents: runDocumentsOf(run.documents) }
    if (run.status === 'failed' || run.status === 'cancelled') return failure(run, `run_${run.status}`)
  }
  // M4 A4: a run still going on is left to go on, as the conversation's
  // task with a card; one never seen is not left running unseen.
  if (last === 'queued' || last === 'running' || last === 'awaiting_approval') return { kind: 'ongoing', runId, status: last }
  await deps.proxy({ method: 'POST', path: `/api/v1/runs/${encodeURIComponent(runId)}/cancel` })
  return { kind: 'failed', code: 'run_timeout', message: 'run_timeout' }
}
