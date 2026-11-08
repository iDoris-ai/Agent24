// KB-C2-04b-1 (#1624): one citation's single-chunk read (U7.2, READONLY R4).
// Nothing in this file writes a log: not the text, not C, not a URL query.

export const READ_TIMEOUT_MS = 10_000
/** #1662 L4: one citation across all its pages, so a slow server cannot hold a budget slot for 16 × 10 s. */
export const READ_TOTAL_MS = 30_000
export const READ_MAX_PAGES = 16
export const READ_MAX_TEXT_BYTES = 256 * 1024

const READ_MAX_BYTES = 65_536
const READ_DEADLINE_MS = 9_000
const READ_CODES = new Set(['unauthorized', 'invalid_request', 'forbidden', 'not_found', 'stale', 'timeout', 'unavailable', 'internal'])
const REASON = /^[a-z_]{1,64}$/
/** R6.1: the only codes whose envelope may carry a null request_id or generation. */
const NULL_ECHO = new Set(['invalid_request', 'unauthorized', 'internal'])
// U+0000–001F, U+007F–009F, and the bidi controls U7.2 names.
// eslint-disable-next-line no-control-regex -- U7.2 strips control characters on purpose
const STRIP = /[\u0000-\u001f\u007f-\u009f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/gu

export interface CitationRef {
  knowledge_base_id: string
  knowledge_id: string
  chunk_id: string
  source_revision: string
  index_build_id: string
  chunk_revision: number | null
  content_tag: string
}

export type ChunkRead =
  | { ok: true; text: string; contentTag: string; title: string | null }
  | { ok: false; code: string; reason?: string }

/** What the read needs of a fetch: Electron's `net.fetch` in the app (the UI client's, same proxy and CA), a fake in tests. */
export type ReadFetch = (url: string, init: RequestInit) => Promise<Response>

export interface ReadonlyDeps {
  origin: string
  credential: () => string | null
  fetch: ReadFetch
  newRequestId: () => string
}

/** U7.2: plain text. Strip control and bidi characters, collapse whitespace, cut to 120 code points. */
export function plainTitle(raw: unknown): string | null {
  if (typeof raw !== 'string') return null
  const collapsed = raw.replace(STRIP, '').replace(/\s+/g, ' ').trim()
  const cut = [...collapsed].slice(0, 120).join('')
  return cut === '' ? null : cut
}

function isObj(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}

function fail(code: string, reason?: string): ChunkRead {
  return reason ? { ok: false, code, reason } : { ok: false, code }
}

/** R6.1 envelope. Unknown code becomes internal (R6.2). Branch on code, not on HTTP status. */
function readError(body: unknown, requestId: string, generation: number): ChunkRead {
  if (!isObj(body) || !isObj(body.error) || !isObj(body.error.details)) return fail('internal')
  // R6.1 / R7.3: the envelope echoes this request. null only where R6.1 allows it (a body that
  // could not be read or echoed: invalid_request; failing before the body: unauthorized, internal),
  // and then paired by this exchange.
  const details = body.error.details
  const nullable = typeof body.error.code === 'string' && NULL_ECHO.has(body.error.code)
  if (details.request_id !== requestId && !(nullable && details.request_id === null)) return fail('internal')
  if (details.generation !== generation && !(nullable && details.generation === null)) return fail('internal')
  const raw = body.error.code
  const code = typeof raw === 'string' && READ_CODES.has(raw) ? raw : 'internal'
  const reason = body.error.details.reason
  if (typeof reason === 'string' && REASON.test(reason)) return fail(code, reason)
  return fail(code)
}

interface PageOk {
  text: string
  titleRaw: unknown
  truncated: boolean
  nextCursor: string | null
}

/** On 200 every one of these has to hold, or the read is internal. */
function readPage(body: unknown, requestId: string, generation: number, ref: CitationRef): PageOk | ChunkRead {
  if (!isObj(body)) return fail('internal')
  // READONLY §0.3: anything but v1 is not read as v1.
  if (body.protocol_version !== 1) return fail('internal')
  if (body.request_id !== requestId || body.generation !== generation) return fail('internal')
  if (body.knowledge_id !== ref.knowledge_id) return fail('internal')
  if (!Array.isArray(body.segments) || body.segments.length !== 1) return fail('internal')
  const seg = body.segments[0]
  if (!isObj(seg)) return fail('internal')
  if (seg.chunk_id !== ref.chunk_id || seg.chunk_type !== 'text' || seg.content_tag !== ref.content_tag) return fail('internal')
  if (typeof seg.text !== 'string') return fail('internal')
  const scope = body.effective_scope
  if (!isObj(scope) || !Array.isArray(scope.knowledge_base_ids) || !scope.knowledge_base_ids.includes(ref.knowledge_base_id)) return fail('internal')
  if (typeof body.truncated !== 'boolean') return fail('internal')
  if (body.truncated) {
    if (typeof body.next_cursor !== 'string' || body.next_cursor.length === 0) return fail('internal')
    return { text: seg.text, titleRaw: isObj(body.display) ? body.display.title : undefined, truncated: true, nextCursor: body.next_cursor }
  }
  return { text: seg.text, titleRaw: isObj(body.display) ? body.display.title : undefined, truncated: false, nextCursor: null }
}

function isPage(v: PageOk | ChunkRead): v is PageOk {
  return !('ok' in v)
}

/** One response body, at most this many bytes: max_bytes of text, each byte up to six once JSON-escaped (`\\u003c`), and the envelope. */
export const READ_MAX_RESPONSE_BYTES = 6 * READ_MAX_BYTES + 64 * 1024

/** Reads the body under the same timeout and abort as the request; past the cap the read stops. */
async function readBody(response: Response): Promise<unknown> {
  const reader = response.body?.getReader()
  if (!reader) throw new Error('no body')
  const chunks: Uint8Array[] = []
  let size = 0
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    size += value.byteLength
    if (size > READ_MAX_RESPONSE_BYTES) {
      void reader.cancel().catch(() => {})
      throw new TooLarge()
    }
    chunks.push(value)
  }
  return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(Buffer.concat(chunks)))
}

class TooLarge extends Error {}

/** One request and its body; the timeout and the caller's abort cover both (R7.1: cancelling aborts the HTTP request). */
async function post(deps: ReadonlyDeps, credential: string, body: unknown, signal: AbortSignal, limitMs: number): Promise<{ status: number; json: unknown } | ChunkRead> {
  if (signal.aborted) return fail('cancelled')
  const timeout = new AbortController()
  const timer = setTimeout(() => timeout.abort(), limitMs)
  const onAbort = (): void => timeout.abort()
  signal.addEventListener('abort', onAbort)
  try {
    let response: Response
    try {
      response = await deps.fetch(`${deps.origin}/api/v1/agent24/knowledge/read`, {
        method: 'POST',
        headers: { 'X-API-Key': credential, 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
        redirect: 'error',
        signal: timeout.signal,
      })
    } catch {
      if (signal.aborted) return fail('cancelled')
      if (timeout.signal.aborted) return fail('timeout')
      return fail('unavailable')
    }
    let json: unknown
    try {
      json = await readBody(response)
    } catch (error) {
      if (signal.aborted) return fail('cancelled')
      if (timeout.signal.aborted) return fail('timeout')
      return error instanceof TooLarge ? fail('internal', 'too_large') : fail('internal')
    }
    if (signal.aborted) return fail('cancelled')
    return { status: response.status, json }
  } finally {
    clearTimeout(timer)
    signal.removeEventListener('abort', onAbort)
  }
}

/**
 * `visible` is asked again before every page (U7.2: no read outside the last visible set);
 * once it says no, the answer is `not_found`, the same as a missing document.
 */
export async function readCitationChunk(deps: ReadonlyDeps, generation: number, ref: CitationRef, signal: AbortSignal, visible: () => boolean = () => true): Promise<ChunkRead> {
  const credential = deps.credential()
  if (credential === null) return fail('unauthorized')
  const expected: Record<string, unknown> = {
    content_tag: ref.content_tag,
    source_revision: ref.source_revision,
    index_build_id: ref.index_build_id,
  }
  if (ref.chunk_revision !== null) expected.chunk_revision = ref.chunk_revision
  let text = ''
  let title: string | null = null
  let cursor: string | null = null
  const deadline = Date.now() + READ_TOTAL_MS
  for (let page = 1; page <= READ_MAX_PAGES; page++) {
    if (signal.aborted) return fail('cancelled')
    if (!visible()) return fail('not_found')
    const requestId = deps.newRequestId()
    const body: Record<string, unknown> = {
      protocol_version: 1,
      request_id: requestId,
      generation,
      scope: { knowledge_base_ids: [ref.knowledge_base_id] },
      knowledge_id: ref.knowledge_id,
      chunk_id: ref.chunk_id,
      expected_revision: expected,
      max_bytes: READ_MAX_BYTES,
      deadline_ms: READ_DEADLINE_MS,
    }
    if (cursor !== null) body.page = { cursor }
    // Each request gets at most 10 s, and never past the citation's own deadline (L4).
    const left = deadline - Date.now()
    if (left <= 0) return fail('timeout')
    const posted = await post(deps, credential, body, signal, Math.min(READ_TIMEOUT_MS, left))
    if ('ok' in posted) return posted
    // R6.2: an error envelope is read by its code, whatever the HTTP status. A success is a 200 whose
    // body passes every check; only that wins over an `error` field (§0.3: unknown fields are ignored).
    const envelope = isObj(posted.json) && isObj(posted.json.error)
    const parsed = posted.status === 200 ? readPage(posted.json, requestId, generation, ref) : fail('internal')
    if (!isPage(parsed)) return envelope ? readError(posted.json, requestId, generation) : parsed
    if (page === 1) title = plainTitle(parsed.titleRaw)
    text += parsed.text
    if (Buffer.byteLength(text, 'utf8') > READ_MAX_TEXT_BYTES) return fail('internal', 'too_large')
    if (!parsed.truncated) return { ok: true, text, contentTag: ref.content_tag, title }
    cursor = parsed.nextCursor
  }
  return fail('internal', 'too_large')
}
