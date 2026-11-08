// M4 A3: where each task of the history list stands, for its filters.
//
// A conversation's tasks are runs of the daemon (docs/ui/ASSISTANT-A.md). The
// list asks `GET /api/v1/runs/{id}` once for each task it shows, and again
// when the daemon's /events says that run changed (main forwards them as
// `run:event`). There is no polling; a run is asked again on a timer only
// when its last answer did not come, or when it still waits on an approval
// the daemon said was decided.

import { useEffect, useSyncExternalStore } from 'react'

/** The daemon's run status, or `missing` when it has no such run. A run not
 *  asked yet, or whose answer did not come, is not in the map. */
export type RunStatus = string

/** At most this many runs are asked at once; the others wait their turn. */
export const MAX_ASKING = 4
/** The wait before asking again a run that needs it, doubled each time up
 *  to the cap, and back to the first once an answer changes something. */
export const RETRY_FIRST_MS = 1_000
export const RETRY_CAP_MS = 30_000

let statuses = new Map<string, RunStatus>()
const listeners = new Set<() => void>()
/** Every run the list has asked about: asked again on its next change. */
const wanted = new Set<string>()
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
const settling = new Set<string>()
let retryTimer: ReturnType<typeof setTimeout> | null = null
let retryMs = RETRY_FIRST_MS
let listening = false

const proxy = () => (typeof window === 'undefined' ? undefined : window.agent24?.backendProxy)

function publish(runId: string, status: RunStatus): void {
  if (statuses.get(runId) === status) return
  statuses = new Map(statuses).set(runId, status)
  for (const l of listeners) l()
}

function askLater(runId: string): void {
  again.add(runId)
  if (retryTimer) return
  retryTimer = setTimeout(() => {
    retryTimer = null
    retryMs = Math.min(retryMs * 2, RETRY_CAP_MS)
    const due = [...again]
    again.clear()
    for (const id of due) ask(id)
  }, retryMs)
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
    const status = (res?.data as { status?: unknown } | null | undefined)?.status
    if (res?.ok && typeof status === 'string') {
      if (status !== statuses.get(runId)) retryMs = RETRY_FIRST_MS
      publish(runId, status)
      if (status === 'awaiting_approval' && settling.has(runId)) askLater(runId)
      else settling.delete(runId)
    } else if (res?.status === 404) {
      settling.delete(runId)
      publish(runId, 'missing')
    } else {
      // The daemon away or another error: what was known stays, and the run
      // is asked again on the timer.
      askLater(runId)
    }
  } finally {
    asking.delete(runId)
    if (changedWhileAsking.delete(runId)) ask(runId)
    pump()
  }
}

function listen(): void {
  if (listening) return
  const onRunEvent = typeof window === 'undefined' ? undefined : window.agent24?.onRunEvent
  if (!onRunEvent) return
  listening = true
  onRunEvent(event => {
    // Only runs the list knows about are asked again; the event says which
    // run changed, its answer says how.
    if (wanted.has(event.run_id)) {
      if (event.type === 'approval.resolved') settling.add(event.run_id)
      else if (event.type !== 'approval.required') settling.delete(event.run_id)
      again.delete(event.run_id)
      ask(event.run_id)
    }
    // An event says the daemon is there: runs whose answer did not come are
    // asked now rather than at the end of the wait.
    if (again.size > 0 && retryTimer) {
      clearTimeout(retryTimer)
      retryTimer = null
      retryMs = RETRY_FIRST_MS
      const due = [...again]
      again.clear()
      for (const id of due) ask(id)
    }
  })
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  return () => listeners.delete(listener)
}
const snapshot = () => statuses

/** The status of each of these runs that is known, asking for the others. */
export function useRunStatuses(runIds: readonly string[]): ReadonlyMap<string, RunStatus> {
  const known = useSyncExternalStore(subscribe, snapshot)
  const key = runIds.join('\n')
  useEffect(() => {
    listen()
    for (const runId of runIds) {
      wanted.add(runId)
      if (!statuses.has(runId)) ask(runId)
    }
  }, [key]) // `key` is runIds' content
  return known
}

/** For tests. */
export function resetRunStatusesForTests(): void {
  statuses = new Map()
  wanted.clear()
  queue.length = 0
  asking.clear()
  changedWhileAsking.clear()
  again.clear()
  settling.clear()
  if (retryTimer) clearTimeout(retryTimer)
  retryTimer = null
  retryMs = RETRY_FIRST_MS
  listening = false
  for (const l of listeners) l()
}
