// The import in progress, outside the page: leaving the Documents page does
// not stop following its job, and coming back shows where it is (#705).

import type { components } from '@agent24/api-client'
import type { DocumentsImportProgress, DocumentsResponse } from '../../../shared/ipc-types'
import { translate } from '../../i18n'
import { documentsMessages } from './Documents.i18n'

type Job = components['schemas']['DocumentsJob']
type Api = Window['agent24']['documents']

export interface ImportState {
  importing: boolean
  progress: DocumentsImportProgress | null
  job: Job | null
  error: string | null
  /** Bumped when an import succeeds, so the list reloads. */
  imported: number
  /** A cancel or retry is on its way: one at a time. */
  acting: boolean
}

/** A job is still working in these states (ADR-DOC-02 §7). */
const WORKING = new Set(['queued', 'running', 'cancelling'])
export const POLL_MS = 1000
const MAX_BACKOFF_MS = 10_000
/** Consecutive failed reads (about five minutes) before giving up on it. */
const MAX_FAILURES = 30

const initial: ImportState = { importing: false, progress: null, job: null, error: null, imported: 0, acting: false }
let state = initial
/** The attempt last counted in `imported`, so a success is counted once. */
let counted: string | null = null
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
  counted = null
  wake = null
  listeners.clear()
}

export function describeError(res: DocumentsResponse & { ok: false }): string {
  const cause = res.error.details?.['cause']
  return translate(documentsMessages, 'error.details', {
    message: res.error.message,
    code: res.error.code,
    cause: typeof cause === 'string' ? translate(documentsMessages, 'error.cause', { cause }) : '',
  })
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

/** A job that failed for what the file itself is fails the same way again
 * (ADR-DOC-02 §6): no retry is offered for it. */
const SAME_AGAIN = new Set([
  'invalid_request', 'not_found', 'payload_too_large', 'unsupported_format', 'parse_failed',
  'upload_checksum_mismatch', 'idempotency_key_reused',
])

/** Reads, after a retry whose answer was lost, while it may still land. */
const RESTART_READS = 10

/** An answer that says the request was refused, so it did not happen. */
const refused = (res: DocumentsResponse & { ok: false }): boolean => res.status >= 400 && res.status < 500

/** How far a job is along within one attempt; a status never goes back. */
const STAGE: Record<string, number> = { queued: 0, running: 1, cancelling: 2 }
const stage = (j: Job): number => STAGE[j.status] ?? 3

/** Takes `next` as the job shown unless it is older news than what was
 * read: answers and reads can land out of order. An ended attempt changes
 * only by a cancel turning it into `cancelled` (ADR-DOC-02 §7). */
function adopt(next: Job): void {
  const shown = state.job
  if (shown && shown.job_id === next.job_id) {
    if (next.attempt < shown.attempt) return
    if (next.attempt === shown.attempt) {
      if (stage(next) < stage(shown)) return
      if (stage(shown) === 3 && next.status !== shown.status && next.status !== 'cancelled') return
    }
  }
  const key = `${next.job_id}#${next.attempt}`
  if (next.status === 'succeeded' && counted !== key) {
    counted = key
    set({ job: next, imported: state.imported + 1 })
  } else {
    set({ job: next })
  }
}

/** The job is still to start or still running: it can be cancelled. */
export const canCancel = (s: ImportState): boolean =>
  s.job !== null && (s.job.status === 'queued' || s.job.status === 'running')

/** The job ended without its document and nothing follows it: it can be
 * restarted (ADR-DOC-02 §7: failed, interrupted or cancelled). */
export function canRetry(s: ImportState): boolean {
  if (s.importing || !s.job) return false
  if (s.job.status === 'failed') return !SAME_AGAIN.has(s.job.error?.code ?? '')
  return s.job.status === 'interrupted' || s.job.status === 'cancelled'
}

/** Asks the OS to cancel the job shown. Cancelling is idempotent; the
 * following goes on and shows where the job ends. */
export async function cancelImport(api: Api): Promise<void> {
  const job = state.job
  if (!job || state.acting || !canCancel(state)) return
  set({ acting: true, error: null })
  try {
    const res = await api.request({ op: 'cancelJob', jobId: job.job_id })
    if (!res.ok) {
      set({ error: describeError(res) })
      return
    }
    adopt(res.data)
    if (state.importing) {
      nudge(job.job_id)
      return
    }
    // Following had stopped (a read error): follow the job to its end.
    set({ importing: true, acting: false })
    try {
      await follow(api, state.job ?? res.data)
    } finally {
      set({ importing: false })
    }
  } finally {
    set({ acting: false })
  }
}

/** The retry's answer was lost and it may still land (openapi
 * documentsRetryJob): the job is read again, a while, for a higher
 * `attempt`; `null` if none came. */
async function awaitRestart(api: Api, before: Job): Promise<Job | null> {
  for (let i = 0; i < RESTART_READS; i++) {
    const read = await api.request({ op: 'job', jobId: before.job_id })
    if (read.ok && read.data.attempt > before.attempt) return read.data
    await sleep(POLL_MS, before.job_id)
  }
  return null
}

/** Restarts the job shown and follows it again. The retry has no
 * idempotency key, so one whose answer is lost is never sent again blindly:
 * the job is read, and a higher `attempt` says it was restarted. */
export async function retryImport(api: Api): Promise<void> {
  const job = state.job
  if (!job || state.acting || !canRetry(state)) return
  set({ importing: true, acting: true, error: null })
  try {
    const res = await api.request({ op: 'retryJob', jobId: job.job_id })
    const restarted = res.ok ? res.data : refused(res) ? null : await awaitRestart(api, job)
    if (!restarted) {
      if (!res.ok) {
        const error = describeError(res)
        set({ error: refused(res) ? error : translate(documentsMessages, 'error.retryUnknown', { error }) })
      }
      return
    }
    set({ acting: false })
    await follow(api, restarted)
  } finally {
    set({ importing: false, acting: false })
  }
}

/** Picks a file, uploads it and follows its job to the end. One at a time. */
export async function startImport(api: Api): Promise<void> {
  if (state.importing || state.acting) return
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
  adopt(first)
  let job = state.job ?? first
  let failures = 0
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
        set({ error: translate(documentsMessages, 'error.readStatus', { error: describeError(res) }) })
        return
      }
      continue
    }
    failures = 0
    adopt(res.data)
    job = state.job ?? res.data
  }
}
