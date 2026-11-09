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
}

export interface ScheduleConversations {
  /** Records a run of the schedule in its conversation, starting that
   *  conversation first if it has none. Runs one at a time, in the order
   *  given, so two runs of a new schedule share one new conversation. */
  record(scheduleId: string, runId: string): Promise<ChatHistorySummary | null>
  /** The schedule's conversation, when it has one on file (A6). */
  conversationOf(scheduleId: string): string | null
}

type Bindings = Record<string, string>

export function createScheduleConversations(deps: ScheduleConversationsDeps): ScheduleConversations {
  const file = path.join(deps.dir, FILE_NAME)
  const read = (): Bindings => {
    try {
      const v = JSON.parse(fs.readFileSync(file, 'utf8')) as { format?: unknown; version?: unknown; bindings?: unknown }
      if (v.format !== SCHEDULE_CONVERSATIONS_FORMAT || v.version !== SCHEDULE_CONVERSATIONS_VERSION) return {}
      if (typeof v.bindings !== 'object' || v.bindings === null || Array.isArray(v.bindings)) return {}
      const out: Bindings = {}
      for (const [schedule, conversation] of Object.entries(v.bindings)) {
        if (ID.test(schedule) && typeof conversation === 'string' && CHAT_HISTORY_ID.test(conversation)) out[schedule] = conversation
      }
      return out
    } catch {
      return {}
    }
  }
  const write = (bindings: Bindings): void => {
    const entries = Object.entries(bindings)
    const kept = Object.fromEntries(entries.slice(Math.max(0, entries.length - MAX_BINDINGS)))
    const temp = `${file}.${randomBytes(6).toString('hex')}.tmp`
    try {
      fs.writeFileSync(temp, JSON.stringify({ format: SCHEDULE_CONVERSATIONS_FORMAT, version: SCHEDULE_CONVERSATIONS_VERSION, bindings: kept }))
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
    const bindings = read()
    let conversation = bindings[scheduleId]
    if (!onFile(conversation)) {
      const name = (await deps.scheduleName(scheduleId).catch(() => null))?.trim().slice(0, MAX_NAME) || scheduleId
      conversation = deps.history.create({ title: deps.title(name), title_source: 'schedule' }).id
      // Last in the file: the one kept longest when it is full.
      delete bindings[scheduleId]
      write({ ...bindings, [scheduleId]: conversation })
    }
    return deps.history.appendRun(conversation, runId, 'schedule')
  }

  let queue: Promise<unknown> = Promise.resolve()
  return {
    record(scheduleId, runId) {
      if (!ID.test(scheduleId) || !ID.test(runId)) return Promise.reject(new Error('Invalid schedule run'))
      const done = queue.then(() => recordNow(scheduleId, runId))
      queue = done.catch(() => {})
      return done
    },
    conversationOf(scheduleId) {
      const conversation = read()[scheduleId]
      return onFile(conversation) ? conversation : null
    },
  }
}
