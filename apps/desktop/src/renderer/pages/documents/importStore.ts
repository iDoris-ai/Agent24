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
  'invalid_request', 'payload_too_large', 'unsupported_format', 'parse_failed',
  'upload_checksum_mismatch', 'idempotency_key_reused',
])

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
    if (state.job?.job_id === job.job_id) set({ job: res.data })
    nudge(job.job_id)
  } finally {
    set({ acting: false })
  }
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
    let restarted: Job
    if (res.ok) {
      restarted = res.data
    } else {
      const read = await api.request({ op: 'job', jobId: job.job_id })
      if (!read.ok || read.data.attempt <= job.attempt) {
        set({ error: describeError(res) })
        return
      }
      restarted = read.data
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
        set({ error: translate(documentsMessages, 'error.readStatus', { error: describeError(res) }) })
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
