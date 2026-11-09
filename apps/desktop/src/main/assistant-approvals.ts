// M4 A6: approvals of runs no conversation asked for (PM, 11-09, option A).
//
// With the approvals page gone (A6), an approval is decided on its run's
// card. A run a schedule started has its card in the schedule's
// conversation (A5), and one the window asked in a desktop session is the
// page's to make a task of (A4). The others (`agent24` on the command line,
// another channel) would have no card: main puts each, on its first
// approval, as a task in one conversation of the assistant's own, titled
// "Started by the assistant" (`title_source = 'assistant'`), where its card
// can confirm or deny. docs/ui/ASSISTANT-A.md §8.
//
// Which run is whose comes from the daemon: `GET /api/v1/runs/{id}` gives
// its schedule and session, `GET /api/v1/sessions/{id}` the session's
// channel (the window's are `desktop`, chat-knowledge.ts). A schedule's run
// is left to A5 only when A5 has it in the schedule's conversation: one its
// first catch-up took as handled, or that could not be recorded, comes here
// (Codex A6 C5). When the conversation is full, a new one is started.
//
// The daemon does not replay events, so each time main connects to /events
// it asks for the pending approvals (`GET /api/v1/approvals?status=pending`)
// and routes the runs it has not handled yet, as A5 does with schedules'
// runs. Unlike A5, the first time is not skipped: an approval waiting from
// before has no other place to be decided in the window.
//
// The file keeps the conversation and the runs handled: a run once handled
// is not routed again, so a conversation the user deleted stays deleted
// until a run not handled yet comes.

import { randomBytes } from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { CHAT_HISTORY_ID, type ChatHistorySummary, type ChatHistoryTitleSource } from '../shared/chat-history'

export const ASSISTANT_APPROVALS_FORMAT = 'agent24.assistant-approvals'
export const ASSISTANT_APPROVALS_VERSION = 1
const FILE_NAME = 'assistant-approvals.json'
/** A run's or a session's id: the daemon's ids are short and plain. */
const ID = /^[A-Za-z0-9_-]{1,128}$/
/** How many handled runs the file keeps: the oldest goes first. A run past
 *  it comes back only with a new approval pending, and goes to the
 *  conversation it is in already (appendRun keeps a run once). */
const MAX_HANDLED = 5000
/** What chat-history's appendRun throws for a conversation with no room. */
const FULL = 'Too many chat history runs'

/** Where a run goes: `schedule` when it is in its schedule's conversation
 *  already (A5), `desktop` when the window asked it, `other` else. */
export type RunOwner = 'schedule' | 'desktop' | 'other'

export interface AssistantApprovalsDeps {
  /** Where the file goes (the app's userData). */
  dir: string
  /** The chat history store's calls this module uses. */
  history: {
    get(id: unknown): unknown
    create(input: { title: string; title_source: ChatHistoryTitleSource }): ChatHistorySummary
    appendRun(id: unknown, runId: unknown, kind: unknown): ChatHistorySummary | null
  }
  /** Whose the run is; throws when the daemon could not tell. */
  ownerOf(runId: string): Promise<RunOwner>
  /** The conversation's title, in the app's language. */
  title(): string
  /** Told of a run a catch-up could not route (tried again at the next). */
  onCatchUpFailure?(runId: string, error: unknown): void
  /** Told whether a run found to be no other conversation's was recorded
   *  (A5's `settle`: one that was not goes back to its schedule's). */
  settled?(runId: string, recorded: boolean): Promise<void>
}

export interface AssistantApprovals {
  /** Routes the run of an approval just required: records it in the
   *  assistant's conversation when no other one has its card. Runs one at
   *  a time, in the order given. */
  route(runId: string): Promise<ChatHistorySummary | null>
  /** Routes the runs of `approvals` (the daemon's pending ones) not handled
   *  yet; one that fails does not stop the others. */
  catchUp(approvals: () => Promise<readonly unknown[]>): Promise<void>
}

interface State {
  conversation: string | null
  handled: string[]
}

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v)

export function createAssistantApprovals(deps: AssistantApprovalsDeps): AssistantApprovals {
  const file = path.join(deps.dir, FILE_NAME)
  const read = (): State => {
    const none: State = { conversation: null, handled: [] }
    try {
      const v = JSON.parse(fs.readFileSync(file, 'utf8')) as unknown
      if (!isObject(v) || v.format !== ASSISTANT_APPROVALS_FORMAT || v.version !== ASSISTANT_APPROVALS_VERSION) return none
      return {
        conversation: typeof v.conversation === 'string' && CHAT_HISTORY_ID.test(v.conversation) ? v.conversation : null,
        handled: Array.isArray(v.handled) ? v.handled.filter((id): id is string => typeof id === 'string' && ID.test(id)) : [],
      }
    } catch {
      return none
    }
  }
  const write = (state: State): void => {
    const temp = `${file}.${randomBytes(6).toString('hex')}.tmp`
    try {
      fs.writeFileSync(temp, JSON.stringify({
        format: ASSISTANT_APPROVALS_FORMAT, version: ASSISTANT_APPROVALS_VERSION,
        conversation: state.conversation, handled: state.handled.slice(-MAX_HANDLED),
      }))
      fs.renameSync(temp, file)
    } finally {
      fs.rmSync(temp, { force: true })
    }
  }
  const onFile = (conversation: string | null): conversation is string => {
    if (conversation === null) return false
    try { return deps.history.get(conversation) !== null } catch { return false }
  }
  const handle = (runId: string): void => {
    const now = read()
    if (!now.handled.includes(runId)) write({ ...now, handled: [...now.handled, runId] })
  }

  const routeNow = async (runId: string): Promise<ChatHistorySummary | null> => {
    if (read().handled.includes(runId)) return null
    // Not handled when the daemon cannot tell: the next catch-up asks again.
    const owner = await deps.ownerOf(runId)
    if (owner !== 'other') {
      // Its card is the schedule's conversation's (A5) or the page's (A4).
      handle(runId)
      return null
    }
    let summary: ChatHistorySummary | null
    try {
      summary = recordIn(runId)
    } catch (err) {
      await deps.settled?.(runId, false).catch(() => {})
      throw err
    }
    // In the conversation from here on, whatever fails after.
    await deps.settled?.(runId, true).catch(() => {})
    // Handled once it is in the conversation; one that could not be is not.
    handle(runId)
    return summary
  }
  const recordIn = (runId: string): ChatHistorySummary | null => {
    const state = read()
    let conversation = state.conversation
    if (!onFile(conversation)) {
      conversation = deps.history.create({ title: deps.title(), title_source: 'assistant' }).id
      write({ ...state, conversation })
    }
    let summary: ChatHistorySummary | null
    try {
      summary = deps.history.appendRun(conversation, runId, 'task')
    } catch (err) {
      // Full (its runs are all tasks): a new one takes the runs from now on
      // (Codex A6 C6).
      if (!(err instanceof Error && err.message === FULL)) throw err
      conversation = deps.history.create({ title: deps.title(), title_source: 'assistant' }).id
      write({ ...read(), conversation })
      summary = deps.history.appendRun(conversation, runId, 'task')
    }
    return summary
  }

  const catchUpNow = async (approvals: () => Promise<readonly unknown[]>): Promise<void> => {
    const runs = new Set<string>()
    for (const a of await approvals()) {
      if (isObject(a) && typeof a.run_id === 'string' && ID.test(a.run_id)) runs.add(a.run_id)
    }
    // Those handled already, routeNow leaves as they are.
    for (const runId of runs) {
      try {
        await routeNow(runId)
      } catch (err) {
        deps.onCatchUpFailure?.(runId, err)
      }
    }
  }

  let queue: Promise<unknown> = Promise.resolve()
  const enqueue = <T>(job: () => Promise<T>): Promise<T> => {
    const done = queue.then(job)
    queue = done.catch(() => {})
    return done
  }
  return {
    route(runId) {
      if (!ID.test(runId)) return Promise.reject(new Error('Invalid run'))
      return enqueue(() => routeNow(runId))
    },
    catchUp(approvals) {
      return enqueue(() => catchUpNow(approvals))
    },
  }
}
