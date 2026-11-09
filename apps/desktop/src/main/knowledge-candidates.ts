// KB-SOP-01b (#1670, M3-1c host submit): the Provider side of `candidate.propose`
// (CONTRACT-OPS-v2 §8): submit a user-confirmed entry draft, read it back, and
// withdraw it while it is still `submitted`. Nothing here keeps state; the
// pending-write record (W1.5) is the caller's (`knowledge-candidate-store.ts`).
// Nothing in this file writes a log: not the text, not C, not a URL.

export const CANDIDATE_TIMEOUT_MS = 10_000
const MAX_RESPONSE_BYTES = 6 * 64 * 1024 + 64 * 1024

/** W1.9 codes a write may answer with; anything else reads as `internal` (READONLY R6.2). */
const WRITE_CODES = new Set(['unauthorized', 'invalid_request', 'forbidden', 'not_found', 'stale', 'timeout', 'unavailable', 'internal'])
const REASON = /^[a-z_]{1,64}$/
/** READONLY R6.1 (OPS I1): the only codes whose envelope may echo a null request_id or generation. */
const NULL_ECHO = new Set(['invalid_request', 'unauthorized', 'internal'])
const SHA256 = /^[0-9a-f]{64}$/
const TIMESTAMP = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?Z$/
const STATUSES = new Set(['submitted', 'approved', 'returned', 'withdrawn'])
const RETURN_REASONS = new Set(['reviewer', 'conflicted', 'target_withdrawn', 'target_gone', 'unsupported_format', 'too_large'])

export type CandidateFetch = (url: string, init: RequestInit) => Promise<Response>

export interface CandidateDeps {
  /** The binding's origin, `https://host[:port]` (as `settingsKnowledgeViewTarget`). */
  origin: string
  credential: () => string | null
  fetch: CandidateFetch
  newRequestId: () => string
}

/**
 * A result that did not come back as a success. `code` is the server's (or a
 * local one: `cancelled`, `not_ready`, and `unavailable/operation_disabled`
 * from W1.4). `unknown` is W1.5's `write_outcome: "unknown"`: the request may
 * have been carried out and the caller must not say it was not.
 */
export interface CandidateFailure {
  ok: false
  code: string
  reason?: string
  retryAfterMs?: number
  unknown: boolean
  /** Set when nothing reached the network (no credential, cancelled before sending): no answer from the server. */
  local?: true
}

export interface SubmitBody {
  idempotency_key: string
  target_knowledge_base_id: string
  mode: 'create'
  kind: string
  title: string
  body: string
  content_sha256: string
  provenance: {
    source: 'agent24_conversation'
    session_ref: string
    message_range: [number, number]
    skill_version: string
    redaction_rules_version: number
  }
}

export interface Submitted {
  ok: true
  candidateId: string
  contentSha256: string
  submittedAt: string
  /** W8.2: the same content was already `submitted`; this is that candidate. */
  existing: boolean
  /** W3.2: a retry of a request the server had finished. */
  replayed: boolean
}

export interface CandidateView {
  candidateId: string
  status: 'submitted' | 'approved' | 'returned' | 'withdrawn'
  contentSha256: string
  targetKnowledgeBaseId: string
  returnReason: string | null
  /** The reviewer's note (W8.6), plain text; gone once W8.9 deletes it. */
  note: string | null
  knowledgeId: string | null
  submittedAt: string
  updatedAt: string
}

function isObj(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}

function failure(code: string, unknown: boolean, reason?: string, retryAfterMs?: number): CandidateFailure {
  const out: CandidateFailure = { ok: false, code, unknown }
  if (reason !== undefined) out.reason = reason
  if (retryAfterMs !== undefined) out.retryAfterMs = retryAfterMs
  return out
}

/** A failure that never left this machine. */
function notSent(code: string): CandidateFailure {
  return { ...failure(code, false), local: true }
}

function isId(v: unknown): v is string {
  return typeof v === 'string' && v.length >= 1 && v.length <= 256
}

function isAuthzVersion(v: unknown): boolean {
  return typeof v === 'string' && v.length >= 1 && v.length <= 128
}

/**
 * READONLY R6.1 envelope, write reasons per OPS §6. Unknown code → internal;
 * the echo must be this request's, null only where R6.1 allows. A reply that
 * is not a well-formed envelope leaves the outcome unknown for a write.
 */
function readError(body: unknown, requestId: string, generation: number, write: boolean): CandidateFailure {
  if (!isObj(body) || !isObj(body.error) || !isObj(body.error.details)) return failure('internal', write)
  const details = body.error.details
  const rawCode = body.error.code
  const nullable = typeof rawCode === 'string' && NULL_ECHO.has(rawCode)
  if (details.request_id !== requestId && !(nullable && details.request_id === null)) return failure('internal', write)
  if (details.generation !== generation && !(nullable && details.generation === null)) return failure('internal', write)
  const code = typeof rawCode === 'string' && WRITE_CODES.has(rawCode) ? rawCode : 'internal'
  const reason = typeof details.reason === 'string' && REASON.test(details.reason) ? details.reason : undefined
  const retry = code === 'unavailable' && reason === 'request_in_progress'
    && typeof details.retry_after_ms === 'number' && Number.isInteger(details.retry_after_ms)
    && details.retry_after_ms >= 0 && details.retry_after_ms <= 60_000
    ? details.retry_after_ms
    : undefined
  // An answered refusal is a definite outcome (W1.9: refused requests write
  // nothing); a timeout, an internal error or a request still in progress is not.
  const unknown = write && (code === 'internal' || code === 'timeout' || code === 'unavailable')
  return failure(code, unknown, reason, retry)
}

class TooLarge extends Error {}

async function readBody(response: Response): Promise<unknown> {
  const reader = response.body?.getReader()
  if (!reader) throw new Error('no body')
  const chunks: Uint8Array[] = []
  let size = 0
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    size += value.byteLength
    if (size > MAX_RESPONSE_BYTES) {
      void reader.cancel().catch(() => {})
      throw new TooLarge()
    }
    chunks.push(value)
  }
  return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(Buffer.concat(chunks)))
}

/** One POST and its body under one timeout and the caller's abort. `write` marks a request whose effect may already have happened. */
async function post(
  deps: CandidateDeps,
  credential: string,
  path: string,
  body: unknown,
  signal: AbortSignal,
  write: boolean,
): Promise<{ status: number; json: unknown } | CandidateFailure> {
  if (signal.aborted) return notSent('cancelled')
  const timeout = new AbortController()
  const timer = setTimeout(() => timeout.abort(), CANDIDATE_TIMEOUT_MS)
  const onAbort = (): void => timeout.abort()
  signal.addEventListener('abort', onAbort)
  let sent = false
  try {
    let response: Response
    try {
      const request = deps.fetch(`${deps.origin}/api/v1/agent24/knowledge/${path}`, {
        method: 'POST',
        headers: { 'X-API-Key': credential, 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
        redirect: 'error',
        cache: 'no-store',
        signal: timeout.signal,
      })
      sent = true
      response = await request
    } catch {
      // Once handed to the network a write may have landed (W1.5).
      if (!sent) return notSent(signal.aborted ? 'cancelled' : 'unavailable')
      const maybe = write
      if (signal.aborted) return failure('cancelled', maybe)
      if (timeout.signal.aborted) return failure('timeout', maybe)
      return failure('unavailable', maybe)
    }
    let json: unknown
    try {
      json = await readBody(response)
    } catch (error) {
      if (signal.aborted) return failure('cancelled', write)
      if (timeout.signal.aborted) return failure('timeout', write)
      return failure('internal', write, error instanceof TooLarge ? 'too_large' : undefined)
    }
    if (signal.aborted) return failure('cancelled', write)
    return { status: response.status, json }
  } finally {
    clearTimeout(timer)
    signal.removeEventListener('abort', onAbort)
  }
}

/** R6.2: an envelope is read by its code whatever the status; only a 200 that passes every check is a success. */
function settle<T>(
  posted: { status: number; json: unknown },
  requestId: string,
  generation: number,
  write: boolean,
  parse: (body: Record<string, unknown>) => T | null,
): T | CandidateFailure {
  const json = posted.json
  if (posted.status === 200 && isObj(json)
    && json.request_id === requestId && json.generation === generation && isAuthzVersion(json.authz_version)) {
    const parsed = parse(json)
    if (parsed !== null) return parsed
  }
  if (isObj(json) && isObj(json.error)) return readError(json, requestId, generation, write)
  return failure('internal', write)
}

/**
 * W1.4 client admission for `candidate.*`: the latest ready answer's
 * `operations.candidate`. `disabled` → local `unavailable/operation_disabled`
 * (not retryable); anything but `ready`, or no answer → local `not_ready`.
 * Neither sends the write.
 */
export async function candidateAdmission(deps: CandidateDeps, generation: number, signal: AbortSignal): Promise<{ ok: true } | CandidateFailure> {
  const credential = deps.credential()
  if (credential === null) return notSent('unauthorized')
  const requestId = deps.newRequestId()
  const posted = await post(deps, credential, 'ready', { protocol_version: 1, request_id: requestId, generation }, signal, false)
  if ('ok' in posted) return posted.code === 'cancelled' ? posted : failure('not_ready', false)
  const json = posted.json
  // R7.2/R7.3: the ready answer echoes both, like every other.
  if (posted.status !== 200 || !isObj(json) || json.request_id !== requestId || json.generation !== generation || json.live !== true) {
    if (isObj(json) && isObj(json.error)) {
      const err = readError(json, requestId, generation, false)
      if (err.code === 'unauthorized') return err
    }
    return failure('not_ready', false)
  }
  const operations = json.operations
  const state = isObj(operations) && isObj(operations.candidate) ? operations.candidate.state : undefined
  if (state === 'ready') return { ok: true }
  if (state === 'disabled') return failure('unavailable', false, 'operation_disabled')
  return failure('not_ready', false)
}

/** W8.2. `body` is sent exactly as given: the pending-write record holds these bytes (W1.5). */
export async function submitCandidate(deps: CandidateDeps, generation: number, body: SubmitBody, signal: AbortSignal): Promise<Submitted | CandidateFailure> {
  const credential = deps.credential()
  if (credential === null) return notSent('unauthorized')
  const requestId = deps.newRequestId()
  const posted = await post(deps, credential, 'candidates', { request_id: requestId, generation, ...body }, signal, true)
  if ('ok' in posted) return posted
  return settle(posted, requestId, generation, true, (json): Submitted | null => {
    if (!isId(json.candidate_id) || json.status !== 'submitted') return null
    // The server hashes what it got (W8.13); a different hash is not our entry.
    if (json.content_sha256 !== body.content_sha256) return null
    if (typeof json.submitted_at !== 'string' || !TIMESTAMP.test(json.submitted_at)) return null
    const details = json.details
    if (details !== undefined && !isObj(details)) return null
    return {
      ok: true,
      candidateId: json.candidate_id,
      contentSha256: json.content_sha256,
      submittedAt: json.submitted_at,
      existing: isObj(details) && details.outcome === 'existing',
      replayed: isObj(details) && details.replayed === true,
    }
  })
}

function parseView(v: unknown, candidateId: string): CandidateView | null {
  if (!isObj(v)) return null
  if (v.candidate_id !== candidateId) return null
  if (typeof v.status !== 'string' || !STATUSES.has(v.status)) return null
  if (typeof v.content_sha256 !== 'string' || !SHA256.test(v.content_sha256)) return null
  if (!isId(v.target_knowledge_base_id)) return null
  if (typeof v.submitted_at !== 'string' || !TIMESTAMP.test(v.submitted_at)) return null
  if (typeof v.updated_at !== 'string' || !TIMESTAMP.test(v.updated_at)) return null
  // OPS §6: a reason this build does not know is still a return; shown as "other".
  const returnReason = v.status === 'returned' && typeof v.return_reason === 'string' && REASON.test(v.return_reason)
    ? (RETURN_REASONS.has(v.return_reason) ? v.return_reason : 'other')
    : null
  const note = typeof v.note === 'string' && v.note.length <= 1024 ? v.note : null
  return {
    candidateId,
    status: v.status as CandidateView['status'],
    contentSha256: v.content_sha256,
    targetKnowledgeBaseId: v.target_knowledge_base_id,
    returnReason,
    note,
    knowledgeId: v.status === 'approved' && isId(v.knowledge_id) ? v.knowledge_id : null,
    submittedAt: v.submitted_at,
    updatedAt: v.updated_at,
  }
}

/** W8.3: the author reads their own candidate. Not a write; nothing is unknown. */
export async function getCandidate(deps: CandidateDeps, generation: number, candidateId: string, signal: AbortSignal): Promise<{ ok: true; view: CandidateView } | CandidateFailure> {
  const credential = deps.credential()
  if (credential === null) return notSent('unauthorized')
  const requestId = deps.newRequestId()
  const posted = await post(deps, credential, 'candidates/get', { request_id: requestId, generation, candidate_id: candidateId }, signal, false)
  if ('ok' in posted) return posted
  return settle(posted, requestId, generation, false, (json) => {
    const view = parseView(json.candidate, candidateId)
    return view === null ? null : { ok: true as const, view }
  })
}

/** W8.8: the author withdraws a `submitted` candidate. Naturally idempotent (W3.1): a repeat answers `already`. */
export async function withdrawCandidate(deps: CandidateDeps, generation: number, candidateId: string, signal: AbortSignal): Promise<{ ok: true; already: boolean } | CandidateFailure> {
  const credential = deps.credential()
  if (credential === null) return notSent('unauthorized')
  const requestId = deps.newRequestId()
  const posted = await post(deps, credential, 'candidates/withdraw', { request_id: requestId, generation, candidate_id: candidateId }, signal, true)
  if ('ok' in posted) return posted
  return settle(posted, requestId, generation, true, (json) => {
    if (json.candidate_id !== candidateId || json.status !== 'withdrawn') return null
    const details = json.details
    if (details !== undefined && !isObj(details)) return null
    return { ok: true as const, already: isObj(details) && details.already === true }
  })
}
