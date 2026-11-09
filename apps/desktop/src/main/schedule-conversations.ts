// M4 A5: each schedule has a conversation of its own, its "home"
// (design r3 §3.4): the first run of a schedule starts a conversation named
// after it, and every later run of that schedule goes to the same one, as a
// `schedule` run with a card (A4). docs/ui/ASSISTANT-A.md §7.
//
// The daemon says which runs are a schedule's: `run.started` carries the
// schedule's id (null for others), for a run of its tick and for `run_now`
// alike. main hears it on /events (agentear-events.ts), with or without a
// window, and records the run here.
//
// Which conversation is a schedule's is kept in `<userData>/schedule-
// conversations.json`. A conversation deleted since, or a file that cannot
// be read, gets the schedule a new one at its next run.
//
// The daemon does not replay events: a client that missed some (cut off, or
// dropped for lagging behind) is to catch up over REST (agent24d
// events.rs). So each time main connects to /events it asks for the runs and
// records the schedules' runs it has not handled yet (`catchUp`, Codex A5).
// The file keeps which runs those were, not a time: a run once recorded is
// never recorded again, so a conversation the user deleted stays deleted,
// and no clock is compared (Codex A5 r2).

import { randomBytes } from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { CHAT_HISTORY_ID, type ChatHistorySummary, type ChatHistoryTitleSource } from '../shared/chat-history'

export const SCHEDULE_CONVERSATIONS_FORMAT = 'agent24.schedule-conversations'
export const SCHEDULE_CONVERSATIONS_VERSION = 1
const FILE_NAME = 'schedule-conversations.json'
/** A schedule's or a run's id: the daemon's ids are short and plain. */
const ID = /^[A-Za-z0-9_-]{1,128}$/
/** A schedule's name, as far as it goes into a title. */
const MAX_NAME = 200
/** How many schedules' conversations the file keeps: the oldest goes first. */
const MAX_BINDINGS = 1000

export interface ScheduleConversationsDeps {
  /** Where the file goes (the app's userData). */
  dir: string
  /** The chat history store's calls this module uses (A2, A5). */
  history: {
    get(id: unknown): unknown
    create(input: { title: string; title_source: ChatHistoryTitleSource }): ChatHistorySummary
    appendRun(id: unknown, runId: unknown, kind: unknown): ChatHistorySummary | null
  }
  /** The schedule's name, or null when the daemon could not tell. */
  scheduleName(scheduleId: string): Promise<string | null>
  /** The new conversation's title, in the app's language. */
  title(name: string): string
  /** Told of a run a catch-up could not record (it is tried again at the
   *  next one). */
  onCatchUpFailure?(runId: string, error: unknown): void
}

export interface ScheduleConversations {
  /** Records a run of the schedule in its conversation, starting that
   *  conversation first if it has none. Runs one at a time, in the order
   *  given, so two runs of a new schedule share one new conversation. */
  record(scheduleId: string, runId: string): Promise<ChatHistorySummary | null>
  /** The schedule's conversation, when it has one on file (A6). */
  conversationOf(scheduleId: string): string | null
  /** Records the schedules' runs among `runs` (the daemon's
   *  `GET /api/v1/runs`) that were not handled yet, oldest first; one that
   *  fails does not stop the others and is tried again next time. The first
   *  time, the runs there are count as handled: nothing from before is
   *  brought in. */
  catchUp(runs: () => Promise<readonly unknown[]>): Promise<void>
}

type Bindings = Record<string, string>
interface State {
  bindings: Bindings
  /** The schedules' runs recorded already; null until the first catch-up.
   *  Each catch-up keeps only those the daemon still has, so it never holds
   *  more than the daemon does (Codex A5 r3 C1). */
  handled: string[] | null
  /** Runs heard of live before the first catch-up that could not be
   *  recorded: that catch-up records them rather than take them as handled
   *  (Codex A5 r3 C2). */
  retry: string[]
}

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v)

export function createScheduleConversations(deps: ScheduleConversationsDeps): ScheduleConversations {
  const file = path.join(deps.dir, FILE_NAME)
  const read = (): State => {
    const none: State = { bindings: {}, handled: null, retry: [] }
    try {
      const v = JSON.parse(fs.readFileSync(file, 'utf8')) as unknown
      if (!isObject(v) || v.format !== SCHEDULE_CONVERSATIONS_FORMAT || v.version !== SCHEDULE_CONVERSATIONS_VERSION) return none
      if (!isObject(v.bindings)) return none
      const bindings: Bindings = {}
      for (const [schedule, conversation] of Object.entries(v.bindings)) {
        if (ID.test(schedule) && typeof conversation === 'string' && CHAT_HISTORY_ID.test(conversation)) bindings[schedule] = conversation
      }
      const ids = (list: unknown): string[] | null =>
        (Array.isArray(list) ? list.filter((id): id is string => typeof id === 'string' && ID.test(id)) : null)
      return { bindings, handled: ids(v.handled), retry: ids(v.retry) ?? [] }
    } catch {
      return none
    }
  }
  const write = (state: State): void => {
    const entries = Object.entries(state.bindings)
    const bindings = Object.fromEntries(entries.slice(Math.max(0, entries.length - MAX_BINDINGS)))
    const temp = `${file}.${randomBytes(6).toString('hex')}.tmp`
    try {
      fs.writeFileSync(temp, JSON.stringify({
        format: SCHEDULE_CONVERSATIONS_FORMAT, version: SCHEDULE_CONVERSATIONS_VERSION, bindings, handled: state.handled, retry: state.retry,
      }))
      fs.renameSync(temp, file)
    } finally {
      fs.rmSync(temp, { force: true })
    }
  }
  const onFile = (conversation: string | undefined): conversation is string => {
    if (conversation === undefined) return false
    try { return deps.history.get(conversation) !== null } catch { return false }
  }

  const recordNow = async (scheduleId: string, runId: string): Promise<ChatHistorySummary | null> => {
    const state = read()
    // Recorded already (by a catch-up, or heard of twice): its conversation
    // may have been deleted since, and stays so (Codex A5 r3 C3).
    if (state.handled?.includes(runId)) { heard.delete(runId); return null }
    try {
      const summary = await recordIn(state, scheduleId, runId)
      heard.delete(runId)
      return summary
    } catch (err) {
      const now = read()
      if (now.handled === null && !now.retry.includes(runId)) write({ ...now, retry: [...now.retry, runId] })
      throw err
    }
  }
  const recordIn = async (state: State, scheduleId: string, runId: string): Promise<ChatHistorySummary | null> => {
    let conversation = state.bindings[scheduleId]
    if (!onFile(conversation)) {
      const name = (await deps.scheduleName(scheduleId).catch(() => null))?.trim().slice(0, MAX_NAME) || scheduleId
      conversation = deps.history.create({ title: deps.title(name), title_source: 'schedule' }).id
      // Last in the file: the one kept longest when it is full.
      delete state.bindings[scheduleId]
      state.bindings[scheduleId] = conversation
      write(state)
    }
    const summary = deps.history.appendRun(conversation, runId, 'schedule')
    // Handled once it is in its conversation; one that could not be is not,
    // and the next catch-up tries it again.
    const after = read()
    if (after.handled !== null && !after.handled.includes(runId)) write({ ...after, handled: [...after.handled, runId] })
    return summary
  }

  const catchUpNow = async (runs: () => Promise<readonly unknown[]>): Promise<void> => {
    const due = (await runs())
      .filter((r): r is { id: string; schedule_id: string; created_at: string } =>
        isObject(r) && typeof r.id === 'string' && ID.test(r.id) && typeof r.schedule_id === 'string' && ID.test(r.schedule_id)
        && typeof r.created_at === 'string')
      // Oldest first: a schedule's conversation starts with its first run.
      .sort((a, b) => (Date.parse(a.created_at) || 0) - (Date.parse(b.created_at) || 0))
    const state = read()
    const there = new Set(due.map(r => r.id))
    if (state.handled === null) {
      // The first time: what is there already is from before main recorded
      // schedules' runs, and stays out; but not the runs heard of live since
      // that could not be recorded.
      // Nor those heard of live and not recorded yet: one heard of while
      // this catch-up waited for the runs waits behind it (PR-Daemon #1704 N1).
      const retry = new Set([...state.retry, ...heard])
      write({ ...state, handled: due.map(r => r.id).filter(id => !retry.has(id)), retry: [] })
    } else {
      // Only what the daemon still has: a run it no longer lists cannot come
      // back in a catch-up.
      write({ ...state, handled: state.handled.filter(id => there.has(id)) })
    }
    const handled = new Set(read().handled)
    for (const r of due) {
      if (handled.has(r.id)) continue
      try {
        await recordNow(r.schedule_id, r.id)
      } catch (err) {
        deps.onCatchUpFailure?.(r.id, err)
      }
    }
  }

  /** Runs heard of live and not recorded yet, from the moment they are
   *  heard of, before their turn in the queue. */
  const heard = new Set<string>()
  let queue: Promise<unknown> = Promise.resolve()
  const enqueue = <T>(job: () => Promise<T>): Promise<T> => {
    const done = queue.then(job)
    queue = done.catch(() => {})
    return done
  }
  return {
    record(scheduleId, runId) {
      if (!ID.test(scheduleId) || !ID.test(runId)) return Promise.reject(new Error('Invalid schedule run'))
      heard.add(runId)
      return enqueue(() => recordNow(scheduleId, runId))
    },
    catchUp(runs) {
      return enqueue(() => catchUpNow(runs))
    },
    conversationOf(scheduleId) {
      const conversation = read().bindings[scheduleId]
      return onFile(conversation) ? conversation : null
    },
  }
}
