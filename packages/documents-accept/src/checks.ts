// The checks that need no model (acceptance README §3): what the daemon can
// do, importing a sample, and reading back what was stored. Each is one or a
// few calls of the documents routes, and says what it saw.

import crypto from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import type { components } from '@agent24/api-client'
import type { Client } from './client'
import type { Sample } from './samples'

type Schemas = components['schemas']

export interface CheckResult {
  /** The check's id in the trace chain, e.g. `ACC-IMPORT`. */
  id: string
  sample?: string
  /** `unmet`: the environment cannot run the checks, which is not a failure of
   *  the product. `skipped`: the check found nothing to exercise (§3, ACC-IDEM-3). */
  outcome: 'passed' | 'failed' | 'unmet' | 'skipped'
  /** What was wrong, one line each; empty when passed. */
  problems: string[]
  ms: number
}

/** Runs `body`, which lists what is wrong; a route that fails is one more thing wrong. */
export async function check(id: string, sample: string | undefined, body: () => Promise<string[]>): Promise<CheckResult> {
  const start = Date.now()
  let problems: string[]
  try {
    problems = await body()
  } catch (error) {
    problems = [error instanceof Error ? error.message : String(error)]
  }
  return { id, ...(sample ? { sample } : {}), outcome: problems.length === 0 ? 'passed' : 'failed', problems, ms: Date.now() - start }
}

export const MAX_PAGES = 1000
/** Every page of a paginated route, from the first to the one whose
 *  `next_cursor` is null. A route that breaks the contract must not hang a
 *  run: a cursor that is neither a cursor nor null, one that comes twice, or
 *  more pages than any run has is an error, found before the page is handed on. */
export async function* pages<T extends { next_cursor: string | null }>(page: (cursor: string) => Promise<T>): AsyncGenerator<T> {
  const seen = new Set<string>([''])
  for (let cursor: string | null = ''; cursor !== null; ) {
    const got: T = await page(cursor)
    const next: unknown = got.next_cursor
    if (next !== null && typeof next !== 'string') throw new Error('next_cursor is neither a cursor nor null')
    if (next !== null && seen.has(next)) throw new Error(`the pages do not end: cursor ${next} came twice`)
    if (seen.size > MAX_PAGES) throw new Error(`the pages do not end: more than ${MAX_PAGES}`)
    if (next !== null) seen.add(next)
    yield got
    cursor = got.next_cursor
  }
}

const sha256 = (bytes: Uint8Array): string => `sha256:${crypto.createHash('sha256').update(bytes).digest('hex')}`

/** ACC-CAP: storage and the read engine are ready, and extraction is on offer.
 *  What is missing makes the run `unmet`, not failed. */
export async function checkCapabilities(client: Client): Promise<CheckResult & { capabilities?: Schemas['DocumentsCapabilities'] }> {
  let capabilities: Schemas['DocumentsCapabilities'] | undefined
  let lacking = false
  const result = await check('ACC-CAP', undefined, async () => {
    capabilities = await client.json<Schemas['DocumentsCapabilities']>('GET', '/capabilities', 'DocumentsCapabilities')
    const ready = (kind: string) => capabilities?.engines.some((e) => e.kind === kind && e.state === 'ready')
    const missing: string[] = []
    if (capabilities.storage.state !== 'ready') missing.push(`storage is ${capabilities.storage.state}`)
    for (const kind of ['parse', 'render']) if (!ready(kind)) missing.push(`no ${kind} engine is ready`)
    const extract = capabilities.operations.find((o) => o.op === 'extract')
    if (extract?.available !== true) missing.push(`extract is not available (${extract?.reason ?? 'not listed'})`)
    lacking = missing.length > 0
    return missing
  })
  return { ...result, outcome: lacking ? 'unmet' : result.outcome, capabilities }
}

/** The largest chunk an upload takes (ADR-DOC-02 §5.6). */
const CHUNK = 768 * 1024

export interface JobWait {
  sleep?: (ms: number) => Promise<void>
  pollMs?: number
  timeoutMs?: number
}

/** Polls a job until it is no longer queued, running or cancelling. */
export async function waitForJob(client: Client, jobId: string, wait: JobWait = {}): Promise<Schemas['DocumentsJob']> {
  const sleep = wait.sleep ?? ((ms: number) => new Promise<void>((done) => setTimeout(done, ms)))
  const deadline = Date.now() + (wait.timeoutMs ?? 10 * 60_000)
  for (;;) {
    const job = await client.json<Schemas['DocumentsJob']>('GET', `/jobs/${jobId}`, 'DocumentsJob')
    if (job.job_id !== jobId) throw new Error(`asked for job ${jobId}, answered with ${job.job_id}`)
    if (!['queued', 'running', 'cancelling'].includes(job.status)) return job
    if (Date.now() >= deadline) throw new Error(`job ${jobId} is still ${job.status}`)
    await sleep(wait.pollMs ?? 500)
  }
}

/** ACC-IMPORT: uploads the sample's source in chunks and imports it. Every run
 *  uploads under a new key, so every run gets a new document (§1). */
export async function checkImport(client: Client, sample: Sample, wait: JobWait = {}): Promise<CheckResult & { documentId?: string }> {
  let documentId: string | undefined
  const result = await check('ACC-IMPORT', sample.id, async () => {
    const bytes = fs.readFileSync(sample.source)
    const upload = await client.json<Schemas['DocumentsUpload']>('POST', '/uploads', 'DocumentsUpload', {
      json: { total_size: bytes.length, sha256: sha256(bytes), filename: path.basename(sample.source) },
      headers: { 'idempotency-key': crypto.randomUUID() },
    })
    let received = upload
    for (let offset = 0; offset < bytes.length; offset += CHUNK) {
      const chunk = bytes.subarray(offset, offset + CHUNK)
      received = await client.json<Schemas['DocumentsUpload']>('POST', `/uploads/${upload.upload_id}/chunks`, 'DocumentsUpload', {
        bytes: chunk,
        headers: { 'upload-offset': String(offset), 'chunk-sha256': sha256(chunk) },
      })
    }
    if (received.status !== 'complete' || received.received !== bytes.length) {
      return [`the upload is ${received.status} after its last chunk, with ${received.received} of ${bytes.length} bytes`]
    }
    const started = await client.json<Schemas['DocumentsJob']>('POST', '/imports', 'DocumentsJob', { json: { upload_id: upload.upload_id, title: sample.id } })
    const job = await waitForJob(client, started.job_id, wait)
    if (job.status !== 'succeeded') return [`the import job is ${job.status}: ${job.error?.code ?? 'no error code'}`]
    documentId = job.result?.document_id
    return documentId ? [] : ['the import job succeeded without a document_id']
  })
  return { ...result, documentId }
}

/** ACC-GET: the document is there, alone and in the list, at revision 1, and
 *  what is stored is the sample's source, byte for byte. */
export async function checkGet(client: Client, sample: Sample, documentId: string): Promise<CheckResult> {
  return check('ACC-GET', sample.id, async () => {
    const problems: string[] = []
    const document = await client.json<Schemas['DocumentDetail']>('GET', `/documents/${documentId}`, 'DocumentDetail')
    if (document.document_id !== documentId) problems.push(`asked for ${documentId}, answered with ${document.document_id}`)
    if (document.head_revision !== 1) problems.push(`head_revision is ${document.head_revision}`)
    if (document.head?.content_sha256 !== `sha256:${sample.meta.sha256_stored}`) {
      problems.push(`the stored content is ${document.head?.content_sha256}, not the sample's sha256_stored`)
    }
    const list = (cursor: string) => client.json<Schemas['DocumentList']>('GET', `/documents${cursor ? `?cursor=${encodeURIComponent(cursor)}` : ''}`, 'DocumentList')
    for await (const page of pages(list)) if (page.documents.some((d) => d.document_id === documentId)) return problems
    return [...problems, 'the document is not in the list']
  })
}
