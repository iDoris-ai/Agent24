// The import in progress, outside the page: leaving the Documents page does
// not stop following its job, and coming back shows where it is (#705).

import type { components } from '@agent24/api-client'
import type { DocumentsImportProgress, DocumentsResponse } from '../../../shared/ipc-types'

type Job = components['schemas']['DocumentsJob']
type Api = Window['agent24']['documents']

export interface ImportState {
  importing: boolean
  progress: DocumentsImportProgress | null
  job: Job | null
  error: string | null
  /** Bumped when an import succeeds, so the list reloads. */
  imported: number
}

/** A job is still working in these states (ADR-DOC-02 §7). */
const WORKING = new Set(['queued', 'running', 'cancelling'])
export const POLL_MS = 1000
const MAX_BACKOFF_MS = 10_000
/** Consecutive failed reads (about five minutes) before giving up on it. */
const MAX_FAILURES = 30

const initial: ImportState = { importing: false, progress: null, job: null, error: null, imported: 0 }
let state = initial
const listeners = new Set<() => void>()

function set(patch: Partial<ImportState>): void {
  state = { ...state, ...patch }
  for (const l of listeners) l()
}

export const getImportState = (): ImportState => state
export function subscribeImport(listener: () => void): () => void {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

/** For tests: forget everything. */
export function resetImportStore(): void {
  state = initial
  wake = null
  listeners.clear()
}

export function describeError(res: DocumentsResponse & { ok: false }): string {
  const cause = res.error.details?.['cause']
  return `${res.error.message}（${res.error.code}${typeof cause === 'string' ? `：${cause}` : ''}）`
}

/** A failed read worth trying again: the daemon or the OS was briefly away.
 * Not when the OS says it is not (ADR-DOC-02 §6: `retryable: false`, such as
 * corrupt storage or a full disk): that needs the user. */
const transient = (res: DocumentsResponse & { ok: false }): boolean =>
  res.status >= 500 && res.error.details?.['retryable'] !== false

/** Ends the current wait between reads early, for the job being followed. */
let wake: { jobId: string; now: () => void } | null = null

/** An OS event about `jobId` (ADR-DOC-02 §7): read the job now rather than
 * at the next tick. Only a hint: what the read says is what counts. */
export function nudge(jobId: string): void {
  if (wake?.jobId === jobId) wake.now()
}

const sleep = (ms: number, jobId: string) =>
  new Promise<void>((r) => {
    const timer = setTimeout(done, ms)
    function done(): void {
      clearTimeout(timer)
      wake = null
      r()
    }
    wake = { jobId, now: done }
  })

/** Picks a file, uploads it and follows its job to the end. One at a time. */
export async function startImport(api: Api): Promise<void> {
  if (state.importing) return
  set({ importing: true, progress: null, job: null, error: null })
  const off = api.onImportProgress((progress) => set({ progress }))
  try {
    const res = await api.importFile()
    if (res === null) return // the user cancelled the dialog
    if (!res.ok) {
      set({ error: describeError(res) })
      return
    }
    await follow(api, res.data)
  } finally {
    off()
    set({ importing: false })
  }
}

/** Reads the job until it stops working, backing off on transient errors. */
async function follow(api: Api, first: Job): Promise<void> {
  let job = first
  let failures = 0
  set({ job })
  while (WORKING.has(job.status)) {
    await sleep(Math.min(POLL_MS * 2 ** failures, MAX_BACKOFF_MS), job.job_id)
    const res = await api.request({ op: 'job', jobId: job.job_id })
    if (!res.ok) {
      if (!transient(res)) {
        set({ error: describeError(res) })
        return
      }
      failures++
      if (failures >= MAX_FAILURES) {
        set({ error: `无法读取导入状态：${describeError(res)}` })
        return
      }
      continue
    }
    failures = 0
    job = res.data
    set({ job })
  }
  if (job.status === 'succeeded') set({ imported: state.imported + 1 })
}
