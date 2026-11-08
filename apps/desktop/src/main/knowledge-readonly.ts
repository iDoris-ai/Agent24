// KB-C2-04b-1 (#1624): one citation's single-chunk read (U7.2, READONLY R4).
// Nothing in this file writes a log: not the text, not C, not a URL query.

export const READ_TIMEOUT_MS = 10_000
export const READ_MAX_PAGES = 16
export const READ_MAX_TEXT_BYTES = 256 * 1024

const READ_MAX_BYTES = 65_536
const READ_DEADLINE_MS = 9_000
const READ_CODES = new Set(['unauthorized', 'invalid_request', 'forbidden', 'not_found', 'stale', 'timeout', 'unavailable', 'internal'])
const REASON = /^[a-z_]{1,64}$/
// U+0000–001F, U+007F–009F, and the bidi controls U7.2 names.
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

export interface ReadonlyDeps {
  origin: string
  credential: () => string | null
  fetch: typeof fetch
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
function readError(body: unknown): ChunkRead {
  if (!isObj(body) || !isObj(body.error) || !isObj(body.error.details)) return fail('internal')
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

/** One response body, at most this many bytes: a page's text is capped by max_bytes, the rest is a small envelope. */
export const READ_MAX_RESPONSE_BYTES = 4 * READ_MAX_BYTES + 16 * 1024

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
async function post(deps: ReadonlyDeps, credential: string, body: unknown, signal: AbortSignal): Promise<{ status: number; json: unknown } | ChunkRead> {
  if (signal.aborted) return fail('cancelled')
  const timeout = new AbortController()
  const timer = setTimeout(() => timeout.abort(), READ_TIMEOUT_MS)
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

export async function readCitationChunk(deps: ReadonlyDeps, generation: number, ref: CitationRef, signal: AbortSignal): Promise<ChunkRead> {
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
  for (let page = 1; page <= READ_MAX_PAGES; page++) {
    if (signal.aborted) return fail('cancelled')
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
    const posted = await post(deps, credential, body, signal)
    if ('ok' in posted) return posted
    if (posted.status !== 200) return readError(posted.json)
    const parsed = readPage(posted.json, requestId, generation, ref)
    if (!isPage(parsed)) return parsed
    if (page === 1) title = plainTitle(parsed.titleRaw)
    text += parsed.text
    if (Buffer.byteLength(text, 'utf8') > READ_MAX_TEXT_BYTES) return fail('internal', 'too_large')
    if (!parsed.truncated) return { ok: true, text, contentTag: ref.content_tag, title }
    cursor = parsed.nextCursor
  }
  return fail('internal', 'too_large')
}
