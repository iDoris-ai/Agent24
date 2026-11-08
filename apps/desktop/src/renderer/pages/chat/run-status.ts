// M4 A3: where each task of the history list stands, for its filters.
//
// A conversation's tasks are runs of the daemon (docs/ui/ASSISTANT-A.md). The
// list asks `GET /api/v1/runs/{id}` once for each task it shows, and again
// when the daemon's /events says that run changed (main forwards them as
// `run:event`). There is no polling.

import { useEffect, useSyncExternalStore } from 'react'

/** The daemon's run status, or `missing` when it has no such run. A run not
 *  asked yet, or whose answer did not come, is not in the map. */
export type RunStatus = string

let statuses = new Map<string, RunStatus>()
const listeners = new Set<() => void>()
/** Runs being asked for now, and runs that changed while being asked (asked
 *  again once the answer is in, so a late answer does not stick). */
const asking = new Set<string>()
const changedWhileAsking = new Set<string>()
/** Every run the list has asked about: asked again on its next change, also
 *  when its first answer never came (the daemon was away). */
const wanted = new Set<string>()
let listening = false

const proxy = () => (typeof window === 'undefined' ? undefined : window.agent24?.backendProxy)

function publish(runId: string, status: RunStatus): void {
  if (statuses.get(runId) === status) return
  statuses = new Map(statuses).set(runId, status)
  for (const l of listeners) l()
}

async function ask(runId: string): Promise<void> {
  const backend = proxy()
  if (!backend) return
  if (asking.has(runId)) { changedWhileAsking.add(runId); return }
  asking.add(runId)
  try {
    const res = await backend({ method: 'GET', path: `/api/v1/runs/${encodeURIComponent(runId)}` }).catch(() => null)
    const status = (res?.data as { status?: unknown } | null | undefined)?.status
    if (res?.ok && typeof status === 'string') publish(runId, status)
    else if (res?.status === 404) publish(runId, 'missing')
    // Otherwise (daemon away, another error) it stays unknown: shown under
    // no filter but "all", and asked again on its next change or showing.
  } finally {
    asking.delete(runId)
    if (changedWhileAsking.delete(runId)) void ask(runId)
  }
}

function listen(): void {
  if (listening) return
  const onRunEvent = typeof window === 'undefined' ? undefined : window.agent24?.onRunEvent
  if (!onRunEvent) return
  listening = true
  // Only runs the list knows about are asked again; the event says which
  // run changed, its answer says how.
  onRunEvent(event => { if (wanted.has(event.run_id)) void ask(event.run_id) })
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
      if (!statuses.has(runId)) void ask(runId)
    }
  }, [key]) // `key` is runIds' content
  return known
}

/** For tests. */
export function resetRunStatusesForTests(): void {
  statuses = new Map()
  asking.clear()
  changedWhileAsking.clear()
  wanted.clear()
  listening = false
  for (const l of listeners) l()
}
