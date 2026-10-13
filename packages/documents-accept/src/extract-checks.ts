// The checks that run the model (acceptance README §3): an extraction asked
// with a sample's ask.json, and the three idempotency rules of ADR-DOC-02
// §5.7 as the REST routes show them.

import crypto from 'node:crypto'
import type { components } from '@agent24/api-client'
import { type CheckResult, type JobWait, check, pages, waitForJob } from './checks'
import { ApiError, type Client } from './client'
import { conform } from './contract'
import type { AskField, Sample } from './samples'

type Schemas = components['schemas']
type Job = Schemas['DocumentsJob']

/** An extraction read to its last page. */
export interface Extraction {
  extractionId: string
  modelId: string
  extractorVersion: string
  values: Schemas['DocumentsExtractedValue'][]
}

const route = (documentId: string) => `/documents/${documentId}/extractions`
const request = (ask: AskField[]) => ({ revision: 1, schema: { fields: ask } })
const said = (error: unknown): string => (error instanceof Error ? error.message : String(error))

/** A job answered for this document must be an extraction of its revision 1. */
function ours(job: Job, documentId: string): Job {
  if (job.kind !== 'extract' || (job.document_id ?? documentId) !== documentId || (job.revision ?? 1) !== 1) throw new Error(`job ${job.job_id} is not an extraction of revision 1 of this document`)
  return job
}

/** An answer of the extraction route whose status does not go with its job: the job is there all the same. */
class Misanswered extends Error {
  constructor(
    message: string,
    readonly job: Job,
  ) {
    super(message)
  }
}

/** One request to the extraction route, and its answer held to what the route
 *  says of itself: 200 with a job that succeeded or was cancelled, 202 with
 *  one that is still to run or to stop, and never one left failed. */
async function extract(client: Client, documentId: string, body: object, headers: Record<string, string> = {}): Promise<{ job: Job; http: number }> {
  const response = await client.send('POST', route(documentId), { json: body, headers })
  const job = ours(conform<Job>('DocumentsJob', await response.json()), documentId)
  const http = job.status === 'succeeded' || job.status === 'cancelled' ? 200 : ['queued', 'running', 'cancelling'].includes(job.status) ? 202 : 0
  if (response.status !== http) throw new Misanswered(`the extraction route answered ${response.status} with a job that is ${job.status}`, job)
  return { job, http }
}

/** A request about one job (cancel, retry), and its answer: 200, with that job. */
async function onJob(client: Client, documentId: string, jobId: string, what: 'cancel' | 'retry'): Promise<Job> {
  const response = await client.send('POST', `/jobs/${jobId}/${what}`)
  const job = conform<Job>('DocumentsJob', await response.json())
  if (job.job_id !== jobId) throw new Error(`asked to ${what} ${jobId}, answered with ${job.job_id}`)
  if (response.status !== 200) throw new Error(`the ${what} answered ${response.status}, not 200`)
  return ours(job, documentId)
}

/** Cancels a job: afterwards it is stopping, stopped, or had already
 *  succeeded. One left queued or running goes on; one left failed or
 *  interrupted would be run again by its next request (§7). */
async function cancel(client: Client, documentId: string, jobId: string): Promise<Job> {
  const job = await onJob(client, documentId, jobId, 'cancel')
  if (!['cancelling', 'cancelled', 'succeeded'].includes(job.status)) throw new Error(`the job is still ${job.status} after its cancel`)
  return job
}

/** Starts the extraction of revision 1 with these questions, or replays the one that was started. */
export async function startExtraction(client: Client, documentId: string, ask: AskField[]): Promise<Job> {
  return (await extract(client, documentId, request(ask))).job
}

/** What every page of one extraction says alike. */
const SAME = ['extraction_id', 'document_id', 'revision', 'content_sha256', 'schema_sha256', 'extractor_version', 'model_id', 'created_at'] as const

/** ACC-EXTRACT: the job of an extraction runs to success, and its values are
 *  read to the last page: the extraction the job names, of revision 1 of this
 *  document and of the sample's bytes, every page alike, its anchors in this
 *  document, and one answer for every question. What the values say is scored
 *  elsewhere (§4). */
export async function checkExtract(client: Client, sample: Sample, documentId: string, ask: AskField[], job: Job, wait: JobWait = {}): Promise<CheckResult & { extraction?: Extraction }> {
  let extraction: Extraction | undefined
  const result = await check('ACC-EXTRACT', sample.id, async () => {
    const done = ours(await waitForJob(client, job.job_id, wait), documentId)
    if (done.status !== 'succeeded') return [`the extraction job is ${done.status}: ${done.error?.code ?? 'no error code'}`]
    const id = done.result?.extraction_id
    if (!id) return ['the extraction job succeeded without an extraction_id']
    const values: Extraction['values'] = []
    let first: Schemas['DocumentsExtraction'] | undefined
    for await (const page of pages((cursor) => client.json<Schemas['DocumentsExtraction']>('GET', `/extractions/${id}${cursor ? `?cursor=${encodeURIComponent(cursor)}` : ''}`, 'DocumentsExtraction'))) {
      first ??= page
      const changed = SAME.find((k) => page[k] !== first?.[k])
      if (changed) return [`the pages of values are not of one extraction: ${changed} changed`]
      values.push(...page.values)
    }
    if (first === undefined) return ['the extraction has no page']
    const content = `sha256:${sample.meta.sha256_stored}`
    if (first.extraction_id !== id) return [`asked for extraction ${id}, answered with ${first.extraction_id}`]
    if (first.document_id !== documentId || first.revision !== 1) return [`the extraction is of ${first.document_id} revision ${first.revision}, not of ${documentId} revision 1`]
    if (first.content_sha256 !== content) return [`the extraction is of ${first.content_sha256}, not the sample's sha256_stored`]
    const anchors = values.flatMap((v) => [...(v.anchors ?? []), ...(v.candidates ?? []).flatMap((c) => c.anchors)])
    const astray = anchors.filter((a) => a.document_id !== documentId || a.revision !== 1 || a.content_sha256 !== content || (a.page ?? 1) > sample.meta.pages).length
    if (astray > 0) return [`${astray} of the anchors are not in revision 1 of this document: another document, revision or content, or a page past the last`]
    // Read, so it can be scored, even when it does not answer every question once.
    extraction = { extractionId: id, modelId: first.model_id, extractorVersion: first.extractor_version, values }
    const asked = ask.map((f) => f.key)
    const answered = values.map((v) => v.key)
    const unanswered = asked.filter((k) => !answered.includes(k))
    const unasked = answered.filter((k, i) => !asked.includes(k) || answered.indexOf(k) !== i)
    return [...(unanswered.length > 0 ? [`no answer to: ${unanswered.join(', ')}`] : []), ...(unasked.length > 0 ? [`answered twice, or never asked: ${unasked.join(', ')}`] : [])]
  })
  return { ...result, extraction }
}

/** ACC-IDEM-1: the same request again is the same job, and does not run it
 *  again (§5.7, first rule). Asked of a job that has ended, so that nothing
 *  moves but what the replay moves: a job that succeeded or was cancelled
 *  stays as it is (200); one that failed or was interrupted is queued again
 *  as its next attempt (202), which §5.4 says a replay does. */
export async function checkReplay(client: Client, sample: Sample, documentId: string, ask: AskField[], job: Job, wait: JobWait = {}): Promise<CheckResult & { requeued?: boolean }> {
  let requeued: boolean | undefined
  const result = await check('ACC-IDEM-1', sample.id, async () => {
    const before = await waitForJob(client, job.job_id, wait)
    const { job: after, http } = await extract(client, documentId, request(ask))
    if (after.job_id !== before.job_id) return [`the same request started another job: ${after.job_id}, not ${before.job_id}`]
    requeued = before.status === 'failed' || before.status === 'interrupted'
    const want = requeued ? { http: 202, status: 'queued', attempt: before.attempt + 1 } : { http: 200, status: before.status, attempt: before.attempt }
    const problems: string[] = []
    if (http !== want.http) problems.push(`the replay of a ${before.status} job answered ${http}, not ${want.http}`)
    if (after.status !== want.status || after.attempt !== want.attempt) {
      problems.push(`the replay left the ${before.status} job ${after.status} at attempt ${after.attempt}, not ${want.status} at ${want.attempt}`)
    }
    if (after.result?.extraction_id !== before.result?.extraction_id) problems.push('the replay names another extraction')
    return problems
  })
  return { ...result, ...(requeued === undefined ? {} : { requeued }) }
}

/** ACC-IDEM-2: a rerun is named by its key. It is a new job, to be run; the
 *  key again is the same rerun; the key with another request is refused
 *  (§5.7, second rule). Every job the reruns started is cancelled afterwards,
 *  whatever was found: none of them is the extraction that is scored. */
export async function checkRerunKey(client: Client, sample: Sample, documentId: string, ask: AskField[], job: Job): Promise<CheckResult> {
  return check('ACC-IDEM-2', sample.id, async () => {
    const problems: string[] = []
    const started = new Set<string>()
    const headers = { 'idempotency-key': crypto.randomUUID() }
    const mine = (answered: Job) => void (answered.job_id !== job.job_id && started.add(answered.job_id))
    const rerun = async (fields: AskField[]) => {
      const answered = await extract(client, documentId, { ...request(fields), rerun: true }, headers).catch((error: unknown) => {
        if (error instanceof Misanswered) mine(error.job)
        throw error
      })
      mine(answered.job)
      return answered
    }
    try {
      const first = await rerun(ask)
      // Nothing more is asked under a key that named the first job: it could only start what nobody cancels.
      if (first.job.job_id === job.job_id) return ['the rerun is the job of the first extraction, not a new one']
      if (first.job.status !== 'queued' && first.job.status !== 'running') problems.push(`a new rerun was answered with a job that is ${first.job.status}`)
      const second = await rerun(ask)
      if (second.job.job_id !== first.job.job_id) problems.push(`the same rerun key started another job: ${second.job.job_id}, not ${first.job.job_id}`)
      // Another request the route takes: the same questions, the last with another type hint.
      const last = ask.at(-1)
      const other = [...ask.slice(0, -1), ...(last ? [{ ...last, type: last.type === 'text' ? 'identifier' : 'text' }] : [])]
      try {
        problems.push(`the rerun key was taken with another request, as job ${(await rerun(other)).job.job_id}`)
      } catch (error) {
        if (!(error instanceof ApiError && error.status === 422 && error.code === 'idempotency_key_reused')) throw error
      }
    } catch (error) {
      problems.push(said(error))
    }
    for (const id of started) await cancel(client, documentId, id).catch((error: unknown) => problems.push(`the rerun ${id} could not be cancelled: ${said(error)}`))
    return problems
  })
}

/** ACC-IDEM-3: a cancelled job is not revived by its request, only by an
 *  explicit retry (§5.7, third rule; §7). Cancels `job` and leaves it queued
 *  again: the job to wait for and score. `skipped` when the cancel found the
 *  job already succeeded. `interrupted` says whether the cancel stopped a job
 *  that was running, or one that was only queued or had failed. */
export async function checkCancelledStays(client: Client, sample: Sample, documentId: string, ask: AskField[], job: Job, wait: JobWait = {}): Promise<CheckResult & { interrupted?: boolean }> {
  let cancelled: Job | undefined
  const result = await check('ACC-IDEM-3', sample.id, async () => {
    cancelled = await cancel(client, documentId, job.job_id)
    if (cancelled.status === 'succeeded') return []
    // A running job is cancelling until its worker next looks: wait for the end of that.
    const stopped = await waitForJob(client, job.job_id, wait)
    if (stopped.status !== 'cancelled') return [`the job was ${cancelled.status} when cancelled, and then ended ${stopped.status}`]
    const problems: string[] = []
    const { job: replayed } = await extract(client, documentId, request(ask))
    if (replayed.job_id !== job.job_id) problems.push(`the replay started another job: ${replayed.job_id}`)
    if (replayed.status !== 'cancelled') problems.push(`the replay made the cancelled job ${replayed.status}`)
    if (replayed.attempt !== stopped.attempt) problems.push(`the replay changed the attempt from ${stopped.attempt} to ${replayed.attempt}`)
    const retried = await onJob(client, documentId, job.job_id, 'retry')
    if (retried.status !== 'queued' || retried.attempt !== stopped.attempt + 1) {
      problems.push(`after the retry the job is ${retried.status} at attempt ${retried.attempt}, not queued at ${stopped.attempt + 1}`)
    }
    return problems
  })
  if (cancelled === undefined) return result
  if (cancelled.status === 'succeeded') return { ...result, outcome: 'skipped' }
  return { ...result, interrupted: cancelled.status === 'cancelling' }
}
