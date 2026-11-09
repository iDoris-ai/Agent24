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
// records the schedules' runs started since it last heard of one (`catchUp`,
// Codex A5 C1). The file keeps when that was.

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
/** A catch-up also takes runs started this long before the last one heard
 *  of: the daemon's clock and main's may differ a little. */
const CATCH_UP_SLACK_MS = 60_000

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
}

export interface ScheduleConversations {
  /** Records a run of the schedule in its conversation, starting that
   *  conversation first if it has none. Runs one at a time, in the order
   *  given, so two runs of a new schedule share one new conversation. */
  record(scheduleId: string, runId: string): Promise<ChatHistorySummary | null>
  /** The schedule's conversation, when it has one on file (A6). */
  conversationOf(scheduleId: string): string | null
  /** Records the schedules' runs among `runs` (the daemon's
   *  `GET /api/v1/runs`) started since main last heard of one, oldest
   *  first; a run recorded already stays as it is. The first time, only
   *  notes the time: there is nothing to catch up on yet. */
  catchUp(runs: () => Promise<readonly ScheduleRunRow[]>): Promise<void>
}

/** What a catch-up needs of a run. */
export interface ScheduleRunRow {
  id: string
  schedule_id: string | null
  created_at: string
}

type Bindings = Record<string, string>
interface State { bindings: Bindings; synced_at: string | null }

export function createScheduleConversations(deps: ScheduleConversationsDeps): ScheduleConversations {
  const file = path.join(deps.dir, FILE_NAME)
  const read = (): State => {
    const none: State = { bindings: {}, synced_at: null }
    try {
      const v = JSON.parse(fs.readFileSync(file, 'utf8')) as { format?: unknown; version?: unknown; bindings?: unknown; synced_at?: unknown }
      if (v.format !== SCHEDULE_CONVERSATIONS_FORMAT || v.version !== SCHEDULE_CONVERSATIONS_VERSION) return none
      if (typeof v.bindings !== 'object' || v.bindings === null || Array.isArray(v.bindings)) return none
      const bindings: Bindings = {}
      for (const [schedule, conversation] of Object.entries(v.bindings)) {
        if (ID.test(schedule) && typeof conversation === 'string' && CHAT_HISTORY_ID.test(conversation)) bindings[schedule] = conversation
      }
      const synced = typeof v.synced_at === 'string' && !Number.isNaN(Date.parse(v.synced_at)) ? v.synced_at : null
      return { bindings, synced_at: synced }
    } catch {
      return none
    }
  }
  const write = (state: State): void => {
    const entries = Object.entries(state.bindings)
    const kept = Object.fromEntries(entries.slice(Math.max(0, entries.length - MAX_BINDINGS)))
    const temp = `${file}.${randomBytes(6).toString('hex')}.tmp`
    try {
      fs.writeFileSync(temp, JSON.stringify({
        format: SCHEDULE_CONVERSATIONS_FORMAT, version: SCHEDULE_CONVERSATIONS_VERSION, bindings: kept, synced_at: state.synced_at,
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

  /** `heardAt`: when main heard of the run, kept as the time a later
   *  catch-up starts from; null when the catch-up keeps its own. */
  const recordNow = async (scheduleId: string, runId: string, heardAt: string | null): Promise<ChatHistorySummary | null> => {
    const state = read()
    let conversation = state.bindings[scheduleId]
    if (!onFile(conversation)) {
      const name = (await deps.scheduleName(scheduleId).catch(() => null))?.trim().slice(0, MAX_NAME) || scheduleId
      conversation = deps.history.create({ title: deps.title(name), title_source: 'schedule' }).id
      // Last in the file: the one kept longest when it is full.
      delete state.bindings[scheduleId]
      state.bindings[scheduleId] = conversation
      write({ ...state, synced_at: heardAt ?? state.synced_at })
    } else if (heardAt !== null) {
      write({ ...state, synced_at: heardAt })
    }
    return deps.history.appendRun(conversation, runId, 'schedule')
  }

  const catchUpNow = async (runs: () => Promise<readonly ScheduleRunRow[]>): Promise<void> => {
    const asked = new Date().toISOString()
    const since = read().synced_at
    if (since !== null) {
      const from = Date.parse(since) - CATCH_UP_SLACK_MS
      const due = (await runs())
        .filter(r => typeof r === 'object' && r !== null && typeof r.schedule_id === 'string' && ID.test(r.schedule_id)
          && typeof r.id === 'string' && ID.test(r.id) && typeof r.created_at === 'string' && Date.parse(r.created_at) >= from)
        .sort((a, b) => Date.parse(a.created_at) - Date.parse(b.created_at))
      for (const r of due) await recordNow(r.schedule_id!, r.id, null)
    }
    // Caught up to when the runs were asked for: runs heard of since are
    // in the queue behind this one.
    write({ ...read(), synced_at: asked })
  }

  let queue: Promise<unknown> = Promise.resolve()
  const enqueue = <T>(job: () => Promise<T>): Promise<T> => {
    const done = queue.then(job)
    queue = done.catch(() => {})
    return done
  }
  return {
    record(scheduleId, runId) {
      if (!ID.test(scheduleId) || !ID.test(runId)) return Promise.reject(new Error('Invalid schedule run'))
      const heardAt = new Date().toISOString()
      return enqueue(() => recordNow(scheduleId, runId, heardAt))
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
