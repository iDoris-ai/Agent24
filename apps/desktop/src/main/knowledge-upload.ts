// L7-K2 A3 (KB-C4-04 first round): upload md, markdown and txt into the
// default knowledge base. The path, the bytes, the credential and the URL stay here.

import { createHash, randomBytes } from 'node:crypto'
import path from 'node:path'
import type { KnowledgeUpload } from '../shared/ipc-types'

export interface UploadDeps {
  provider: () => {
    origin: string
    credential: () => string | null
    fetch: (url: string, init: RequestInit) => Promise<Response>
    newRequestId: () => string
  } | null
  knowledgeBaseId: () => string | null
  readFile: (p: string) => Promise<Buffer>
  stat: (p: string) => Promise<{ size: number; isFile: boolean }>
  now?: () => number
  setTimer?: (fn: () => void, ms: number) => unknown
  clearTimer?: (t: unknown) => void
  onChange: (uploads: KnowledgeUpload[]) => void
}

type UploadProvider = NonNullable<ReturnType<UploadDeps['provider']>>

const MAX_BYTES = 10_485_760
const MAX_RESPONSE_BYTES = 256 * 1024
const MAX_UPLOADS = 50
const INGEST_TIMEOUT_MS = 60_000
const POLL_TIMEOUT_MS = 15_000
const POLL_MS = 2_000
const ALLOWED = new Set(['md', 'markdown', 'txt'])
const JOB_STATUS = new Set(['saved', 'processing', 'indexed', 'failed', 'cancelled'])
const REASON = /^[a-z_]{1,64}$/

interface Slot {
  upload: KnowledgeUpload
  /** Bumped when a cancel starts, so an older poll cannot overwrite it. */
  epoch: number
  /** Polls spent waiting for the cost of a stopped job to become final; capped at COST_WAIT_POLLS. */
  costPolls?: number
}

/** A stopped job's cost is read again for at most this many polls (10 minutes at 2 s), then left «暂未结算». */
const COST_WAIT_POLLS = 300

interface ParsedJob {
  jobId: string
  status: 'saved' | 'processing' | 'indexed' | 'failed' | 'cancelled'
  enrichment: string | null
  costFinal: boolean
  costZero: boolean
  costUnknown: boolean
  originalRetained: boolean
  failureReason: string | null
}

type Posted =
  | { kind: 'http'; status: number; json: unknown }
  | { kind: 'unreadable'; status: number }
  | { kind: 'unreachable' }

function isObj(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function reasonOf(value: unknown): string | null {
  return typeof value === 'string' && REASON.test(value) ? value : null
}

function extensionOf(fileName: string): string {
  const dot = fileName.lastIndexOf('.')
  if (dot <= 0 || dot === fileName.length - 1) return ''
  return fileName.slice(dot + 1).toLowerCase()
}

function endpoint(origin: string, suffix: string): string {
  return `${origin.replace(/\/+$/, '')}/api/v1/agent24/knowledge/${suffix}`
}

/** costZero is final && at least one item && every item is exact 0 (W5.10): an empty list says nothing. */
function costOf(cost: unknown): { costFinal: boolean; costZero: boolean; costUnknown: boolean } {
  if (!isObj(cost) || typeof cost.final !== 'boolean' || !Array.isArray(cost.items)) {
    return { costFinal: false, costZero: false, costUnknown: false }
  }
  let zero = true
  let unknown = false
  for (const item of cost.items) {
    if (!isObj(item) || item.measured !== 'exact' || item.units !== 0) zero = false
    if (isObj(item) && item.measured === 'unknown') unknown = true
  }
  return { costFinal: cost.final, costZero: cost.final && cost.items.length > 0 && zero, costUnknown: unknown }
}

function parseJob(value: unknown): ParsedJob | null {
  if (!isObj(value)) return null
  const jobId = value.job_id
  const status = value.status
  if (typeof jobId !== 'string' || jobId.length < 1 || jobId.length > 256) return null
  if (typeof status !== 'string' || !JOB_STATUS.has(status)) return null
  const enrichment =
    typeof value.enrichment === 'string' &&
    value.enrichment.length >= 1 &&
    value.enrichment.length <= 64
      ? value.enrichment
      : null
  return {
    jobId,
    status: status as ParsedJob['status'],
    enrichment,
    ...costOf(value.cost),
    originalRetained: value.original_retained === true,
    failureReason: reasonOf(value.failure_reason),
  }
}

function policyVersionOf(json: unknown): string | null {
  if (!isObj(json) || !isObj(json.error) || !isObj(json.error.details)) return null
  const version = json.error.details.current_policy_version
  return typeof version === 'string' && version.length >= 1 && version.length <= 256
    ? version
    : null
}

/** 409 stale/policy_changed carries the version to adopt. Anything else is not a retry. */
function policyChanged(status: number, json: unknown): string | null {
  if (status !== 409 || !isObj(json) || !isObj(json.error)) return null
  if (json.error.code !== 'stale') return null
  const details = json.error.details
  if (!isObj(details) || details.reason !== 'policy_changed') return null
  return policyVersionOf(json)
}

function refusedOrFailed(
  status: number,
  json: unknown,
): { state: 'refused' | 'failed'; reason: string } {
  if (isObj(json) && isObj(json.error)) {
    const code = reasonOf(json.error.code) ?? 'internal'
    if (code === 'unauthorized' || code === 'unavailable' || code === 'internal') {
      return { state: 'failed', reason: code }
    }
    const details = isObj(json.error.details) ? json.error.details : null
    return { state: 'refused', reason: (details && reasonOf(details.reason)) ?? code }
  }
  if (status === 401) return { state: 'failed', reason: 'unauthorized' }
  if (status === 503) return { state: 'failed', reason: 'unavailable' }
  if (status >= 500) return { state: 'failed', reason: 'internal' }
  if (status === 404) return { state: 'refused', reason: 'not_found' }
  if (status === 403) return { state: 'refused', reason: 'forbidden' }
  if (status === 409) return { state: 'refused', reason: 'stale' }
  if (status === 400) return { state: 'refused', reason: 'invalid_request' }
  return { state: 'failed', reason: 'internal' }
}

class TooLarge extends Error {}

async function readBody(response: Response, signal: AbortSignal): Promise<unknown> {
  const reader = response.body?.getReader()
  if (!reader) throw new Error('no body')
  const chunks: Uint8Array[] = []
  let size = 0
  const onAbort = (): void => {
    void reader.cancel().catch(() => {})
  }
  signal.addEventListener('abort', onAbort)
  try {
    if (signal.aborted) throw new Error('aborted')
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      if (!value) continue
      size += value.byteLength
      if (size > MAX_RESPONSE_BYTES) {
        void reader.cancel().catch(() => {})
        throw new TooLarge()
      }
      chunks.push(value)
    }
  } finally {
    signal.removeEventListener('abort', onAbort)
  }
  return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(Buffer.concat(chunks)))
}

function needsPoll(upload: KnowledgeUpload): boolean {
  if (upload.jobId === null) return false
  if (upload.state === 'saved' || upload.state === 'processing') return true
  if (upload.state === 'indexed' && upload.enrichment === 'running') return true
  // A job that has stopped is read again until its cost is final (W5.10: final always follows).
  return (upload.state === 'indexed' || upload.state === 'cancelled' || upload.state === 'failed') && !upload.costFinal
}

/** needsPoll, with the wait for a stopped job's final cost bounded (it also stops on dispose, like every poll). */
/** The job has stopped and only its final cost is awaited (not while enrichment is still running). */
function waitingForCost(upload: KnowledgeUpload): boolean {
  return upload.state === 'cancelled' || upload.state === 'failed'
    || (upload.state === 'indexed' && upload.enrichment !== 'running')
}

function slotNeedsPoll(slot: Slot): boolean {
  if (!needsPoll(slot.upload)) return false
  return !waitingForCost(slot.upload) || (slot.costPolls ?? 0) < COST_WAIT_POLLS
}

function finished(upload: KnowledgeUpload): boolean {
  return upload.state !== 'sending' && !needsPoll(upload)
}

export function knowledgeUploads(deps: UploadDeps): {
  add(paths: string[]): Promise<KnowledgeUpload[]>
  list(): KnowledgeUpload[]
  cancel(uploadId: string): Promise<KnowledgeUpload | null>
  dispose(): void
} {
  const setTimer = deps.setTimer ?? ((fn: () => void, ms: number) => setTimeout(fn, ms))
  const clearTimer =
    deps.clearTimer ??
    ((timer: unknown) => {
      clearTimeout(timer as ReturnType<typeof setTimeout>)
    })
  const now = deps.now ?? (() => Date.now())
  // The server's policy version. A refused 409 writes nothing, so the same key is reused once.
  let policyVersion = 'unknown'
  const slots: Slot[] = []
  let timer: unknown = null
  let polling = false
  let disposed = false
  let tail: Promise<void> = Promise.resolve()

  function copy(upload: KnowledgeUpload): KnowledgeUpload {
    return { ...upload }
  }

  function fresh(
    fileName: string,
    state: KnowledgeUpload['state'],
    reason: string | null,
  ): KnowledgeUpload {
    return {
      uploadId: randomBytes(16).toString('hex'),
      fileName,
      state,
      reason,
      jobId: null,
      enrichment: null,
      costFinal: false,
      costZero: false,
      costUnknown: false,
      originalRetained: false,
      updatedAt: now(),
    }
  }

  function publish(): void {
    // Drop the oldest finished uploads until 50 remain. In-flight ones stay.
    while (slots.length > MAX_UPLOADS) {
      const index = slots.findIndex((slot) => finished(slot.upload))
      if (index < 0) break
      slots.splice(index, 1)
    }
    try {
      deps.onChange(slots.map((slot) => copy(slot.upload)))
    } catch {
      // A broken listener must not wedge the queue.
    }
    ensureTimer()
  }

  function ensureTimer(): void {
    if (disposed || timer !== null || polling) return
    if (!slots.some((slot) => slotNeedsPoll(slot))) return
    timer = setTimer(() => {
      timer = null
      if (!disposed) void pollOnce()
    }, POLL_MS)
  }

  function adopt(upload: KnowledgeUpload, job: ParsedJob): void {
    upload.state = job.status
    upload.jobId = job.jobId
    upload.enrichment = job.enrichment
    upload.costFinal = job.costFinal
    upload.costZero = job.costZero
    upload.costUnknown = job.costUnknown
    upload.originalRetained = job.originalRetained
    upload.updatedAt = now()
  }

  function applyOutcome(
    upload: KnowledgeUpload,
    outcome: { state: 'refused' | 'failed'; reason: string },
  ): void {
    upload.state = outcome.state
    upload.reason = outcome.reason
    upload.updatedAt = now()
  }

  // The request may have landed. Do not report it as failed.
  function markUnreachable(upload: KnowledgeUpload): void {
    upload.state = 'unknown'
    upload.reason = 'unreachable'
    upload.updatedAt = now()
  }

  async function post(
    provider: UploadProvider,
    credential: string,
    url: string,
    body: unknown,
    timeoutMs: number,
  ): Promise<Posted> {
    const timeout = new AbortController()
    const handle = setTimer(() => timeout.abort(), timeoutMs)
    try {
      let response: Response
      try {
        response = await provider.fetch(url, {
          method: 'POST',
          headers: { 'X-API-Key': credential, 'Content-Type': 'application/json' },
          body: JSON.stringify(body),
          redirect: 'error',
          cache: 'no-store',
          signal: timeout.signal,
        })
      } catch {
        return { kind: 'unreachable' }
      }
      try {
        const json = await readBody(response, timeout.signal)
        if (timeout.signal.aborted) return { kind: 'unreachable' }
        return { kind: 'http', status: response.status, json }
      } catch {
        if (timeout.signal.aborted) return { kind: 'unreachable' }
        return { kind: 'unreadable', status: response.status }
      }
    } finally {
      clearTimer(handle)
    }
  }

  function applyHttp(upload: KnowledgeUpload, requestId: string, posted: Posted): void {
    if (posted.kind === 'unreachable') {
      markUnreachable(upload)
      return
    }
    // A 2xx whose body cannot be used (cut off, too large, another request's, not a job): the upload may
    // have landed, so it is unknown, never failed.
    if (posted.status >= 200 && posted.status < 300) {
      const job = posted.kind !== 'unreadable' && isObj(posted.json) && posted.json.request_id === requestId
        ? parseJob(posted.json.job)
        : null
      if (!job) {
        markUnreachable(upload)
        return
      }
      adopt(upload, job)
      upload.reason = job.status === 'failed' ? job.failureReason : null
      return
    }
    if (posted.kind === 'unreadable') {
      applyOutcome(upload, refusedOrFailed(posted.status, null))
      return
    }
    applyOutcome(upload, refusedOrFailed(posted.status, posted.json))
  }

  async function deliver(
    upload: KnowledgeUpload,
    provider: UploadProvider,
    knowledgeBaseId: string,
    fileName: string,
    bytes: Buffer,
  ): Promise<void> {
    const idempotencyKey = randomBytes(32).toString('base64url')
    const contentSha256 = createHash('sha256').update(bytes).digest('hex')
    const contentBase64 = bytes.toString('base64')
    let version = policyVersion
    for (let attempt = 0; attempt < 2; attempt++) {
      const credential = provider.credential()
      if (!credential) {
        applyOutcome(upload, { state: 'refused', reason: 'not_connected' })
        return
      }
      const requestId = provider.newRequestId()
      const posted = await post(
        provider,
        credential,
        endpoint(provider.origin, 'ingest'),
        {
          request_id: requestId,
          idempotency_key: idempotencyKey,
          knowledge_base_id: knowledgeBaseId,
          file_name: fileName,
          content_base64: contentBase64,
          content_sha256: contentSha256,
          expected_policy_version: version,
        },
        INGEST_TIMEOUT_MS,
      )
      if (posted.kind === 'http') {
        const next = policyChanged(posted.status, posted.json)
        if (next !== null) {
          policyVersion = next
          version = next
          // A refused request writes nothing, so this key is still free. Resend once.
          if (attempt === 0) continue
        }
      }
      applyHttp(upload, requestId, posted)
      return
    }
  }

  async function processOne(filePath: string): Promise<KnowledgeUpload> {
    const fileName = path.basename(filePath)
    if (!ALLOWED.has(extensionOf(fileName))) return refuse(fileName, 'unsupported_format')
    let info: { size: number; isFile: boolean }
    try {
      info = await deps.stat(filePath)
    } catch {
      return refuse(fileName, 'not_a_file')
    }
    if (!info.isFile) return refuse(fileName, 'not_a_file')
    if (info.size > MAX_BYTES) return refuse(fileName, 'too_large')
    let provider: UploadProvider | null = null
    try {
      provider = deps.provider()
    } catch {
      provider = null
    }
    let knowledgeBaseId: string | null = null
    try {
      knowledgeBaseId = deps.knowledgeBaseId()
    } catch {
      knowledgeBaseId = null
    }
    const credential = provider?.credential() ?? null
    if (!provider || !credential || !knowledgeBaseId) return refuse(fileName, 'not_connected')
    let bytes: Buffer
    try {
      bytes = await deps.readFile(filePath)
    } catch {
      return refuse(fileName, 'not_a_file')
    }
    if (bytes.length > MAX_BYTES) return refuse(fileName, 'too_large')
    const slot: Slot = { upload: fresh(fileName, 'sending', null), epoch: 0 }
    slots.push(slot)
    publish()
    try {
      await deliver(slot.upload, provider, knowledgeBaseId, fileName, bytes)
    } catch {
      applyOutcome(slot.upload, { state: 'failed', reason: 'internal' })
    }
    publish()
    return slot.upload
  }

  function refuse(fileName: string, reason: string): KnowledgeUpload {
    const upload = fresh(fileName, 'refused', reason)
    slots.push({ upload, epoch: 0 })
    publish()
    return upload
  }

  async function pollOne(slot: Slot): Promise<void> {
    const jobId = slot.upload.jobId
    if (!jobId || !slotNeedsPoll(slot)) return
    // Only the wait for the final cost counts against its budget, never the polls of a running enrichment.
    if (waitingForCost(slot.upload)) slot.costPolls = (slot.costPolls ?? 0) + 1
    const epoch = slot.epoch
    const provider = deps.provider()
    const credential = provider?.credential() ?? null
    if (!provider || !credential) return
    const requestId = provider.newRequestId()
    const posted = await post(
      provider,
      credential,
      endpoint(provider.origin, 'jobs/get'),
      {
        request_id: requestId,
        job_id: jobId,
      },
      POLL_TIMEOUT_MS,
    )
    if (disposed || slot.epoch !== epoch || !slots.includes(slot)) return
    if (posted.kind === 'unreachable') return
    if (posted.status === 404) {
      slot.upload.state = 'unknown'
      slot.upload.reason = 'not_found'
      slot.upload.updatedAt = now()
      publish()
      return
    }
    if (posted.kind !== 'http' || posted.status !== 200) return
    if (!isObj(posted.json) || posted.json.request_id !== requestId) return
    const job = parseJob(posted.json.job)
    if (!job || job.jobId !== jobId) return
    adopt(slot.upload, job)
    slot.upload.reason = job.status === 'failed' ? job.failureReason : null
    publish()
  }

  async function pollOnce(): Promise<void> {
    if (disposed) return
    polling = true
    try {
      const due = slots.filter((slot) => slotNeedsPoll(slot))
      for (const slot of due) {
        if (disposed) return
        if (!slots.includes(slot) || !slotNeedsPoll(slot)) continue
        await pollOne(slot)
      }
    } finally {
      polling = false
      if (!disposed) ensureTimer()
    }
  }

  function add(paths: string[]): Promise<KnowledgeUpload[]> {
    const run = tail.then(async () => {
      const out: KnowledgeUpload[] = []
      for (const filePath of paths) out.push(copy(await processOne(filePath)))
      return out
    })
    tail = run.then(
      () => undefined,
      () => undefined,
    )
    return run
  }

  async function cancel(uploadId: string): Promise<KnowledgeUpload | null> {
    const slot = slots.find((item) => item.upload.uploadId === uploadId)
    if (!slot) return null
    const jobId = slot.upload.jobId
    if (!jobId) return copy(slot.upload)
    const provider = deps.provider()
    const credential = provider?.credential() ?? null
    if (!provider || !credential) return copy(slot.upload)
    const epoch = ++slot.epoch
    const requestId = provider.newRequestId()
    const posted = await post(
      provider,
      credential,
      endpoint(provider.origin, 'jobs/cancel'),
      {
        request_id: requestId,
        job_id: jobId,
      },
      POLL_TIMEOUT_MS,
    )
    if (slot.epoch !== epoch || !slots.includes(slot)) return copy(slot.upload)
    if (posted.kind === 'unreachable') return copy(slot.upload)
    if (posted.status === 404) {
      slot.upload.state = 'unknown'
      slot.upload.reason = 'not_found'
      slot.upload.updatedAt = now()
      publish()
      return copy(slot.upload)
    }
    if (posted.kind !== 'http' || posted.status !== 200 || !isObj(posted.json))
      return copy(slot.upload)
    if (posted.json.request_id !== requestId) return copy(slot.upload)
    const effect = posted.json.cancel_effect
    const job = parseJob(posted.json.job)
    if (!job || job.jobId !== jobId) return copy(slot.upload)
    if (effect !== 'cancelled' && effect !== 'enrichment_stopped' && effect !== 'none') {
      return copy(slot.upload)
    }
    adopt(slot.upload, job)
    slot.upload.reason = effect === 'cancelled' ? null : effect
    publish()
    return copy(slot.upload)
  }

  function dispose(): void {
    disposed = true
    if (timer !== null) {
      clearTimer(timer)
      timer = null
    }
  }

  return {
    add,
    list: () => slots.map((slot) => copy(slot.upload)),
    cancel,
    dispose,
  }
}
