// M4 A3: where each task of the history list stands, for its filters.
//
// A conversation's tasks are runs of the daemon (docs/ui/ASSISTANT-A.md). The
// list asks `GET /api/v1/runs/{id}` once for each task it shows, and again
// when the daemon's /events says that run changed (main forwards them as
// `run:event`). There is no polling; a run is asked again on a timer only
// when its last answer did not come, or when it still waits on an approval
// the daemon said was decided.

import { useEffect, useSyncExternalStore } from 'react'
import type { RunEvent } from '../../../shared/ipc-types'
import type { RunKnowledge } from '../knowledge-grounding'

/** The daemon's run status, or `missing` when it has no such run. A run not
 *  asked yet, or whose answer did not come, is not in the map. */
export type RunStatus = string

/** M4 A4: what a task's card shows of its run. */
export interface RunView {
  status: RunStatus
  started_at: string | null
  ended_at: string | null
  error: string | null
  /** A completed run's answer, and what its Knowledge tools read (null
   *  without a binding): shown only after `grounding` checked it. */
  output: string | null
  knowledge: RunKnowledge | null
}

/** At most this many runs are asked at once; the others wait their turn. */
export const MAX_ASKING = 4
/** The wait before asking again a run that needs it, doubled each time up
 *  to the cap, and back to the first once an answer changes something. */
export const RETRY_FIRST_MS = 1_000
export const RETRY_CAP_MS = 30_000

let views = new Map<string, RunView>()
let statuses = new Map<string, RunStatus>()
const listeners = new Set<() => void>()
/** The runs shown now, with how many lists show each: asked again on their
 *  next change; one no list shows any more is not asked again (Codex A3 r2). */
const wanted = new Map<string, number>()
/** Runs waiting their turn, and those being asked now; a run that changed
 *  while being asked is asked again once that answer is in, so a late
 *  answer does not stick. */
const queue: string[] = []
const asking = new Set<string>()
const changedWhileAsking = new Set<string>()
/** Runs to ask again on the timer: their answer did not come (the daemon
 *  was away, Codex A3 C2), or they still wait on an approval the daemon
 *  said was decided: the run goes back to running only after the tool,
 *  with no event of its own (Codex A3 C1). */
const again = new Set<string>()
/** The last event of each run: an answer still `awaiting_approval` after an
 *  `approval.resolved` is asked again; after an `approval.required`, it is
 *  a new approval nobody decided yet (Codex A3 r2). Set by events only, so a
 *  late answer cannot undo it. */
const lastEvent = new Map<string, RunEvent['type']>()
let retryTimer: ReturnType<typeof setTimeout> | null = null
let retryMs = RETRY_FIRST_MS
/** An event asks the runs waiting on the timer at once, once until an
 *  answer comes: while the daemon answers no run, events do not set the pace. */
let eagerSpent = false
let listening = false

const proxy = () => (typeof window === 'undefined' ? undefined : window.agent24?.backendProxy)

const same = (a: RunView | undefined, b: RunView) =>
  !!a && a.status === b.status && a.started_at === b.started_at && a.ended_at === b.ended_at && a.error === b.error
  && a.output === b.output && JSON.stringify(a.knowledge) === JSON.stringify(b.knowledge)

function publish(runId: string, view: RunView): void {
  if (same(views.get(runId), view)) return
  views = new Map(views).set(runId, view)
  if (statuses.get(runId) !== view.status) statuses = new Map(statuses).set(runId, view.status)
  for (const l of listeners) l()
}

const text = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null)

function askLater(runId: string): void {
  again.add(runId)
  if (retryTimer) return
  retryTimer = setTimeout(() => {
    retryTimer = null
    retryMs = Math.min(retryMs * 2, RETRY_CAP_MS)
    askAgainNow()
  }, retryMs)
}

function askAgainNow(): void {
  const due = [...again].filter(id => (wanted.get(id) ?? 0) > 0)
  again.clear()
  for (const id of due) ask(id)
}

/** Asks for the run's status now, or when a turn is free. */
function ask(runId: string): void {
  if (asking.has(runId)) { changedWhileAsking.add(runId); return }
  if (queue.includes(runId)) return
  queue.push(runId)
  pump()
}

function pump(): void {
  while (asking.size < MAX_ASKING && queue.length > 0) void answer(queue.shift()!)
}

async function answer(runId: string): Promise<void> {
  const backend = proxy()
  if (!backend) return
  asking.add(runId)
  try {
    const res = await backend({ method: 'GET', path: `/api/v1/runs/${encodeURIComponent(runId)}` }).catch(() => null)
    const run = (res?.data ?? null) as {
      status?: unknown; started_at?: unknown; created_at?: unknown; ended_at?: unknown
      error?: { message?: unknown } | null; output?: { text?: unknown } | null; knowledge?: RunKnowledge | null
    } | null
    const status = run?.status
    if (res?.ok && typeof status === 'string') {
      if (status !== statuses.get(runId)) retryMs = RETRY_FIRST_MS
      eagerSpent = false
      publish(runId, {
        status,
        started_at: text(run?.started_at) ?? text(run?.created_at),
        ended_at: text(run?.ended_at),
        error: text(run?.error?.message),
        output: status === 'completed' && typeof run?.output?.text === 'string' ? run.output.text : null,
        knowledge: status === 'completed' ? run?.knowledge ?? null : null,
      })
      if (status === 'awaiting_approval' && lastEvent.get(runId) === 'approval.resolved') askLater(runId)
    } else if (res?.status === 404) {
      eagerSpent = false
      publish(runId, { status: 'missing', started_at: null, ended_at: null, error: null, output: null, knowledge: null })
    } else {
      // The daemon away or another error: what was known stays, and the run
      // is asked again on the timer.
      askLater(runId)
    }
  } finally {
    asking.delete(runId)
    if (changedWhileAsking.delete(runId) && (wanted.get(runId) ?? 0) > 0) ask(runId)
    pump()
  }
}

function listen(): void {
  if (listening) return
  const onRunEvent = typeof window === 'undefined' ? undefined : window.agent24?.onRunEvent
  if (!onRunEvent) return
  listening = true
  onRunEvent(event => {
    // Only runs a list shows are asked again; the event says which run
    // changed, its answer says how.
    if ((wanted.get(event.run_id) ?? 0) > 0) {
      lastEvent.set(event.run_id, event.type)
      again.delete(event.run_id)
      ask(event.run_id)
    }
    // An event says the daemon is there: runs whose answer did not come are
    // asked now rather than at the end of the wait, once until an answer
    // comes; the wait itself goes on doubling.
    if (again.size > 0 && retryTimer && !eagerSpent) {
      eagerSpent = true
      clearTimeout(retryTimer)
      retryTimer = null
      askAgainNow()
    }
  })
}

/** No list shows the run any more: it is not asked again. */
function drop(runId: string): void {
  wanted.delete(runId)
  again.delete(runId)
  lastEvent.delete(runId)
  changedWhileAsking.delete(runId)
  const at = queue.indexOf(runId)
  if (at >= 0) queue.splice(at, 1)
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  return () => listeners.delete(listener)
}
const snapshot = () => statuses
const viewSnapshot = () => views

/** The status of each of these runs that is known, asking for the others. */
export function useRunStatuses(runIds: readonly string[]): ReadonlyMap<string, RunStatus> {
  const known = useSyncExternalStore(subscribe, snapshot)
  useWanted(runIds)
  return known
}

/** M4 A4: what is known of each of these runs, asking for the others. */
export function useRunViews(runIds: readonly string[]): ReadonlyMap<string, RunView> {
  const known = useSyncExternalStore(subscribe, viewSnapshot)
  useWanted(runIds)
  return known
}

/** M4 A4: asks for the run again now, e.g. after the user acted on it. */
export function askRunAgain(runId: string): void {
  if ((wanted.get(runId) ?? 0) > 0) ask(runId)
}

function useWanted(runIds: readonly string[]): void {
  const key = runIds.join('\n')
  useEffect(() => {
    listen()
    for (const runId of runIds) {
      const was = wanted.get(runId)
      wanted.set(runId, (was ?? 0) + 1)
      // Shown anew after no list showed it: what was known may be old, its
      // events were not listened to meanwhile (Codex A3 r3 C2).
      if (was === undefined || !statuses.has(runId)) ask(runId)
    }
    return () => {
      for (const runId of runIds) {
        const n = (wanted.get(runId) ?? 1) - 1
        wanted.set(runId, n)
        // Let go only once no list shows it after this commit: when the ids
        // change, React cleans up before the next effect registers them
        // again, and a run still shown keeps its wait for an answer and its
        // last event (Codex A3 r3 C1).
        if (n === 0) queueMicrotask(() => { if (wanted.get(runId) === 0) drop(runId) })
      }
    }
  }, [key]) // `key` is runIds' content
}

/** For tests. */
export function resetRunStatusesForTests(): void {
  statuses = new Map()
  views = new Map()
  wanted.clear()
  queue.length = 0
  asking.clear()
  changedWhileAsking.clear()
  again.clear()
  lastEvent.clear()
  eagerSpent = false
  if (retryTimer) clearTimeout(retryTimer)
  retryTimer = null
  retryMs = RETRY_FIRST_MS
  listening = false
  for (const l of listeners) l()
}
