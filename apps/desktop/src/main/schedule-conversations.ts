// M4 A5: each schedule has a conversation of its own, its "home"
// (design r3 §3.4): the first run of a schedule starts a conversation named
// after it, and every later run of that schedule goes to the same one, as a
// `schedule` run with a card (A4). docs/ui/ASSISTANT-A.md §7. M6 C1: the
// conversation is the schedule's (`initiator = 'schedule'`), so each run
// recorded in it is unseen until the user opens it (docs/ui/ASSISTANT-C.md §3).
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
import { CHAT_HISTORY_ID, type ChatHistoryInitiator, type ChatHistorySummary, type ChatHistoryTitleSource } from '../shared/chat-history'

export const SCHEDULE_CONVERSATIONS_FORMAT = 'agent24.schedule-conversations'
export const SCHEDULE_CONVERSATIONS_VERSION = 1
const FILE_NAME = 'schedule-conversations.json'
/** A schedule's or a run's id: the daemon's ids are short and plain. */
const ID = /^[A-Za-z0-9_-]{1,128}$/
/** A schedule's name, as far as it goes into a title. */
const MAX_NAME = 200
/** How many schedules' conversations the file keeps: the oldest goes first. */
const MAX_BINDINGS = 1000
/** How many full conversations of a schedule's are kept as its own (#1719 F-A). */
const MAX_FORMER = 10
/** chat-history.ts: a conversation with 1000 runs takes no more schedule runs. */
const FULL = 'Too many chat history runs'

export interface ScheduleConversationsDeps {
  /** Where the file goes (the app's userData). */
  dir: string
  /** The chat history store's calls this module uses (A2, A5). */
  history: {
    get(id: unknown): unknown
    create(input: { title: string; title_source: ChatHistoryTitleSource; initiator: ChatHistoryInitiator }): ChatHistorySummary
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
  /** For an approval of the run (A6), once the runs heard of before are
   *  recorded: true when the run is in its schedule's conversation; else
   *  false, and the run is the caller's: it is taken as handled here, so no
   *  later catch-up records it in the schedule's conversation too (an
   *  approval of a run the first catch-up took as handled, or that could not
   *  be recorded, has no card there; Codex A6 C5, r2 C1). */
  claim(scheduleId: string, runId: string): Promise<boolean>
  /** After a claim the caller lost: whether it recorded the run. One it
   *  could not is given back, to be recorded here as before the claim. */
  settle(runId: string, recorded: boolean): Promise<void>
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
  /** A schedule's conversations before its current one, which filled up
   *  (#1719 F-A): their runs are still the schedule's, for `claim`. */
  former: Record<string, string[]>
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
    const none: State = { bindings: {}, former: {}, handled: null, retry: [] }
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
      const former: Record<string, string[]> = {}
      for (const [schedule, list] of Object.entries(isObject(v.former) ? v.former : {})) {
        const kept = Array.isArray(list) ? list.filter((c): c is string => typeof c === 'string' && CHAT_HISTORY_ID.test(c)) : []
        if (schedule in bindings && kept.length > 0) former[schedule] = kept
      }
      return { bindings, former, handled: ids(v.handled), retry: ids(v.retry) ?? [] }
    } catch {
      return none
    }
  }
  const write = (state: State): void => {
    const entries = Object.entries(state.bindings)
    const bindings = Object.fromEntries(entries.slice(Math.max(0, entries.length - MAX_BINDINGS)))
    const former = Object.fromEntries(Object.entries(state.former)
      .filter(([schedule]) => schedule in bindings)
      .map(([schedule, list]) => [schedule, list.slice(-MAX_FORMER)]))
    const temp = `${file}.${randomBytes(6).toString('hex')}.tmp`
    try {
      fs.writeFileSync(temp, JSON.stringify({
        format: SCHEDULE_CONVERSATIONS_FORMAT, version: SCHEDULE_CONVERSATIONS_VERSION, bindings, former, handled: state.handled, retry: state.retry,
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
    if (claimed.has(runId)) return null
    try {
      const summary = await recordIn(state, scheduleId, runId)
      heard.delete(runId)
      return summary
    } catch (err) {
      const now = read()
      if (now.handled === null && !now.retry.includes(runId)) write({ ...now, retry: [...now.retry, runId] })
      // Once the first catch-up is done, nothing more is asked of `heard`.
      if (now.handled !== null) heard.delete(runId)
      throw err
    }
  }
  const holds = (conversation: string | undefined, runId: string): boolean => {
    try {
      const record = deps.history.get(conversation) as { runs?: { run_id?: unknown }[] } | null
      return record?.runs?.some(run => run.run_id === runId) ?? false
    } catch {
      return false
    }
  }
  const inFormer = (state: State, scheduleId: string, runId: string): boolean =>
    (state.former[scheduleId] ?? []).some(c => holds(c, runId))

  /** A new conversation for the schedule, now its home; `full` is the one it
   *  replaces because it filled up, kept as the schedule's. */
  const startConversation = async (scheduleId: string, full: string | null): Promise<string> => {
    const name = (await deps.scheduleName(scheduleId).catch(() => null))?.trim().slice(0, MAX_NAME) || scheduleId
    const conversation = deps.history.create({ title: deps.title(name), title_source: 'schedule', initiator: 'schedule' }).id
    const state = read()
    // Last in the file: the one kept longest when it is full.
    delete state.bindings[scheduleId]
    state.bindings[scheduleId] = conversation
    if (full !== null) state.former[scheduleId] = [...(state.former[scheduleId] ?? []).filter(c => c !== full), full]
    write(state)
    return conversation
  }
  const recordIn = async (state: State, scheduleId: string, runId: string): Promise<ChatHistorySummary | null> => {
    // In one of the full ones already (its card written, `handled` not): only
    // `handled` is left to do (Codex #1719 C1).
    if (inFormer(state, scheduleId, runId)) {
      const now = read()
      if (now.handled !== null && !now.handled.includes(runId)) write({ ...now, handled: [...now.handled, runId] })
      return null
    }
    let conversation = state.bindings[scheduleId]
    if (!onFile(conversation)) conversation = await startConversation(scheduleId, null)
    let summary: ChatHistorySummary | null
    try {
      summary = deps.history.appendRun(conversation, runId, 'schedule')
    } catch (err) {
      // Full (1000 runs): a new one takes the schedule's runs from now on.
      // The full one keeps its runs, still the schedule's (#1719 F-A).
      if (!(err instanceof Error && err.message === FULL)) throw err
      conversation = await startConversation(scheduleId, conversation)
      summary = deps.history.appendRun(conversation, runId, 'schedule')
    }
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
      const retry = new Set([...state.retry, ...heard].filter(id => !claimed.has(id)))
      write({ ...state, handled: due.map(r => r.id).filter(id => !retry.has(id)), retry: [] })
      // Asked of no more (Codex A6 r2 C3); those of it this catch-up does not
      // record stay out of `handled`, and are tried at the next.
      heard.clear()
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
  /** Runs claimed (A6) and not settled yet: whether, before the claim, A5
   *  was to record them, so they are given back if the caller cannot. */
  const claimed = new Map<string, { eligible: boolean }>()
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
    claim(scheduleId, runId) {
      return enqueue(async () => {
        const state = read()
        // In its schedule's conversation, or one of the full ones before it (#1719 F-A).
        if (holds(state.bindings[scheduleId], runId) || inFormer(state, scheduleId, runId)) return true
        // The first catch-up takes it as handled with the rest, once it is
        // neither to retry nor heard of; a live record of it is not made
        // (Codex A6 r3 C1). Given back if the caller cannot record it.
        const handledBefore = state.handled !== null && state.handled.includes(runId)
        // A5's to record before the claim: not handled yet, or, before the
        // first catch-up, waiting for it (to retry, or heard of and not
        // recorded). One recorded live then is not (Codex #1719 C2, r2 C2).
        const eligible = state.handled !== null ? !handledBefore : state.retry.includes(runId) || heard.has(runId)
        const handled = state.handled === null || handledBefore ? state.handled : [...state.handled, runId]
        write({ ...state, handled, retry: state.retry.filter(id => id !== runId) })
        // Only once on file: a claim that could not be written leaves nothing
        // reserved, and the run is A5's as before (#1719 C2).
        claimed.set(runId, { eligible })
        heard.delete(runId)
        return false
      })
    },
    settle(runId, recorded) {
      return enqueue(async () => {
        const was = claimed.get(runId)
        claimed.delete(runId)
        if (!was || recorded || !was.eligible) return
        // The caller could not record it, and it was A5's: it is again, as
        // things stand now (Codex A6 r3 C2). Out of `handled`, which the
        // first catch-up may have put it in while it was claimed (#1719 C1);
        // before that catch-up, back to retry, so that it does not take it as
        // handled. One that was not A5's stays as it is: a conversation the
        // user deleted is not brought back (Codex #1719 C2, r2 C2).
        const state = read()
        if (state.handled !== null) {
          if (state.handled.includes(runId)) write({ ...state, handled: state.handled.filter(id => id !== runId) })
        } else if (!state.retry.includes(runId)) {
          write({ ...state, retry: [...state.retry, runId] })
        }
      })
    },
    conversationOf(scheduleId) {
      const conversation = read().bindings[scheduleId]
      return onFile(conversation) ? conversation : null
    },
  }
}
