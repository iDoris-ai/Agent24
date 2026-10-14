// A stand-in for the daemon in unit tests: a `Client` whose requests go to a
// function, which sees each one as the routes would. The builders below give
// answers the contract accepts, so a test changes only what it is about.

import { Client, type ClientOptions } from '../client'

export interface Call {
  method: string
  /** The path under /api/v1/documents, with its query. */
  route: string
  headers: Record<string, string>
  json?: unknown
  bytes?: Uint8Array
  /** Whether the request carried a deadline. */
  timed: boolean
}

export const answer = (body: unknown, status = 200, headers: Record<string, string> = {}): Response =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json', ...headers } })

export const refuse = (status: number, code: string, retryable = false): Response =>
  answer({ error: { code, message: `it is ${code}`, details: { retryable } } }, status)

/** An id as the contract shapes it: a kind, then a ULID. */
export const id = (kind: 'job' | 'doc' | 'upl' | 'ext', n = 1): string => `${kind}_0${String(n).padStart(25, '0')}`
export const sha = (digit = '0'): string => `sha256:${digit.repeat(64)}`
const AT = '2026-10-14T00:00:00.000Z'

export const job = (more: object = {}) => ({ job_id: id('job'), kind: 'extract', status: 'queued', attempt: 1, progress: null, error: null, result: null, created_at: AT, updated_at: AT, ...more })
export const failed = (code = 'engine_unavailable', more: object = {}) => job({ status: 'failed', error: { code, message: code }, ...more })
export const upload = (more: object = {}) => ({ upload_id: id('upl'), total_size: 1, sha256: sha(), received: 0, status: 'receiving', expires_at: AT, ...more })
export const document = (more: object = {}) => ({
  document_id: id('doc'),
  title: 'a sample',
  media_type: 'application/pdf',
  head_revision: 1,
  created_at: AT,
  updated_at: AT,
  head: { revision: 1, content_sha256: sha(), size: 1, media_type: 'application/pdf' },
  ...more,
})

const GEOMETRY = { box: 'CropBox', unit: 'pt', origin: 'top-left-rotated', rects: [[0, 0, 10, 10]] }
/** A block of a text layer, on `page`. */
export const block = (block_id: string, page = 1) => ({ block_id, page, text: 'x', text_sha256: sha('b'), geometry: GEOMETRY })
/** One page of a reading of the text. */
export const textPage = (blocks: object[], next_cursor: string | null, more: object = {}) => ({
  document_id: id('doc'),
  revision: 1,
  content_sha256: sha(),
  media_type: 'application/pdf',
  text_layer_sha256: sha('a'),
  engine: { id: 'apple-pdfkit', version: '15.2.0' },
  parse_status: 'complete',
  blocks,
  next_cursor,
  ...more,
})
/** An anchor: bytes `start` to `end` of a block on `page`. */
export const anchor = (page: number, block_id: string, start = 0, end = 4, more: object = {}) => ({
  document_id: id('doc'),
  revision: 1,
  content_sha256: sha(),
  media_type: 'application/pdf',
  text_layer_sha256: sha('a'),
  engine: { id: 'apple-pdfkit', version: '15.2.0' },
  block_id,
  page,
  block_text_sha256: sha('b'),
  text_range: { unit: 'utf8', start, end },
  geometry: GEOMETRY,
  quote: 'x',
  ...more,
})
/** One page of the matches of a `find`. */
export const found = (matches: object[], next_cursor: string | null = null) => ({ parse_status: 'complete', matches, next_cursor })

const BASE = 'http://127.0.0.1:1/api/v1/documents'

/** A client answered by `daemon`, and every call it made, in order. */
export function fakeClient(daemon: (call: Call, calls: Call[]) => Response, options: ClientOptions = {}): { client: Client; calls: Call[] } {
  const calls: Call[] = []
  const fetch = (async (url: string, init: RequestInit = {}) => {
    const body = init.body
    const call: Call = {
      method: init.method ?? 'GET',
      route: String(url).slice(BASE.length),
      headers: init.headers as Record<string, string>,
      timed: init.signal instanceof AbortSignal,
      ...(typeof body === 'string' ? { json: JSON.parse(body) as unknown } : {}),
      ...(body instanceof Uint8Array ? { bytes: body } : {}),
    }
    calls.push(call)
    return daemon(call, calls)
  }) as unknown as typeof globalThis.fetch
  const client = new Client({ base: BASE, token: 'secret' }, { fetch, sleep: async () => {}, ...options })
  return { client, calls }
}
