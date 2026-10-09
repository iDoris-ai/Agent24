// Importing a file into the Documenting OS (ADR-DOC-01 D5, ADR-DOC-02 §5.6):
// main hashes the file, starts an upload, sends it as raw chunks of at most
// 768 KiB and starts the import. The renderer never sees the file's path,
// and the public API never gets it either (ADR-001): only its base name.

import { createHash } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { open, stat, type FileHandle } from 'node:fs/promises'
import { basename } from 'node:path'
import type { components } from '@agent24/api-client'
import type { DocumentsResponse } from '../shared/ipc-types'
import { toResponse, type Route, type SendOptions, type Sender } from './documents'

type Upload = components['schemas']['DocumentsUpload']

const BASE = '/api/v1/documents'
/** The OS's chunk limit (ADR-DOC-02 §5.6). */
export const CHUNK_BYTES = 786_432
const UPLOAD_ID = /^upl_[0-7][0-9A-HJKMNP-TV-Z]{25}$/
/** Replies to send again with the same key (ADR-DOC-02 §6): main's own
 * transport failures, the kernel's "not dispatched" codes and its "dispatched,
 * outcome unknown" ones. */
const RETRY = new Set([
  'backend_not_ready',
  'backend_unreachable',
  'backend_timeout',
  'module_overloaded',
  'module_stopping',
  'module_not_ready',
  'module_draining',
  'request_abandoned',
  'upstream_timeout',
  'upstream_connection_closed',
  'upstream_unavailable',
  'upstream_response_too_large',
  'duplicate_request_id',
  'module_panicked',
  'module_killed',
  'stop_failed',
])
/** Sends per step. Every step is idempotent (the key, a chunk replay, the
 * upload id), so a retry never doubles anything. */
const ATTEMPTS = 3
/** 409 resumes allowed in one run, so a confused server cannot loop us. */
const MAX_RESUMES = 8
/** Uploads tried in one run: the first, then one more for each that the OS
 * says has expired. Expiry is 24 h apart, so this is never reached in use;
 * it only keeps one run bounded. */
const MAX_GENERATIONS = 64

/** Per file (its first key), the last upload in its chain that `/imports`
 * answered 404 for (`after`) and the chain's first upload (`root`): the next
 * run starts its walk just past `after`. `/imports` answers an import that
 * started with its job before it looks at expiry, so a 404 means that upload
 * has no job and never will; the uploads before it were all answered so too.
 * Only a shortcut: without it a run walks from the start, as after a restart;
 * the check on `root` before any import catches an OS that lost its data. */
const chainTail = new Map<string, { root: string; after: string }>()

/** For tests: each fake OS starts with no uploads. */
export function forgetChains(): void {
  chainTail.clear()
}

export interface ImportProgress {
  sent: number
  total: number
}

type Answer = DocumentsResponse<'job'>

function fail(status: number, code: string, message: string): DocumentsResponse<never> {
  return { ok: false, status, error: { code, message } }
}

const retryable = (r: DocumentsResponse): boolean => !r.ok && RETRY.has(r.error.code)

/** Sends one step, again while the reply says that is safe. */
async function step(send: Sender, route: Route, options: SendOptions): Promise<DocumentsResponse> {
  let res = fail(503, 'backend_unreachable', 'not sent') as DocumentsResponse
  for (let i = 0; i < ATTEMPTS; i++) {
    res = toResponse(await send(route, options))
    if (!retryable(res)) return res
  }
  return res
}

async function sha256Of(path: string): Promise<string> {
  const hash = createHash('sha256')
  for await (const chunk of createReadStream(path)) hash.update(chunk as Buffer)
  return `sha256:${hash.digest('hex')}`
}

const json = (body: unknown, headers: Record<string, string> = {}): SendOptions => ({
  headers: { 'Content-Type': 'application/json', ...headers },
  body: Buffer.from(JSON.stringify(body)),
})

/** The base name when the contract allows it (1–255 characters, no `/`, `\`
 * or NUL); otherwise none: the OS then titles the document itself. */
function filenameOf(path: string): string | undefined {
  const name = basename(path)
  const chars = [...name].length
  return chars >= 1 && chars <= 255 && !/[/\\\0]/.test(name) ? name : undefined
}

/** The OS's answer that the upload is gone: expired before its import (or
 * unknown). Only the OS decides this, never main's clock: an upload whose
 * import already started still answers `/imports` with that job. */
const gone = (r: DocumentsResponse): boolean => !r.ok && r.status === 404 && r.error.code === 'not_found'

const isOffset = (v: unknown, max: number): v is number =>
  typeof v === 'number' && Number.isSafeInteger(v) && v >= 0 && v <= max

/** The upload's Idempotency-Key: derived from what it creates (the file's
 * hash and size, and its name), so a retry, a second import of the same file
 * or two at once all reach the same upload, and through it the same import
 * job. No state in main, nothing to forget. `after` is the expired upload a
 * new key replaces. */
function keyFor(sha256: string, total: number, filename: string | undefined, after?: string): string {
  const basis = JSON.stringify([sha256, total, filename ?? null, after ?? null])
  return `desktop-${createHash('sha256').update(basis).digest('hex')}`
}

/** Imports the file at `path`; on success the answer is the import job. */
export async function importFile(
  path: string,
  send: Sender,
  onProgress: (p: ImportProgress) => void = () => {},
): Promise<Answer> {
  try {
    const total = (await stat(path)).size
    const sha256 = await sha256Of(path)
    return await upload(path, total, sha256, send, onProgress)
  } catch {
    // Never the error's own text: it often names the absolute path.
    return fail(400, 'file_unreadable', 'the file could not be read')
  }
}

async function upload(
  path: string,
  total: number,
  sha256: string,
  send: Sender,
  onProgress: (p: ImportProgress) => void,
): Promise<Answer> {
  const filename = filenameOf(path)
  const file = await open(path, 'r')
  try {
    // Each expired upload is followed by one under a key derived from it,
    // just as stable, so every run walks the same chain to the live one.
    const first = keyFor(sha256, total, filename)
    const create = async (after: string | undefined): Promise<{ up: Upload } | { answer: Answer }> => {
      const created = await step(
        send,
        { method: 'POST', path: `${BASE}/uploads` },
        json(
          { total_size: total, sha256, ...(filename ? { filename } : {}) },
          { 'Idempotency-Key': keyFor(sha256, total, filename, after) },
        ),
      )
      if (!created.ok) return { answer: created }
      const up = created.data as unknown as Upload
      if (!UPLOAD_ID.test(up.upload_id) || up.total_size !== total || up.sha256 !== sha256 || !isOffset(up.received, total)) {
        return { answer: fail(502, 'invalid_response', 'the upload does not match the file') }
      }
      return { up }
    }
    // The OS keeps every upload key and never reuses an upload id, so a key
    // that no longer gives the upload it gave before means the OS lost its
    // data (and its 404s meant that, not expiry): walk from the start, as
    // every other run will. An upload made by such a check gets no chunks
    // and expires.
    const hint = chainTail.get(first)
    let root = hint?.root
    let after = hint?.after
    const restart = (): void => {
      chainTail.delete(first)
      root = after = undefined
    }
    for (let generation = 0; generation < MAX_GENERATIONS; generation++) {
      const made = await create(after)
      if ('answer' in made) return made.answer
      const up = made.up
      if (after === undefined) root = up.upload_id
      if (up.status !== 'expired') {
        const sent = await sendChunks(file, up, total, send, onProgress)
        if (sent && !gone(sent)) return sent as Answer
        if (!sent) onProgress({ sent: total, total })
      }
      // An import only ever starts on the chain the OS holds now: its first
      // key must still give the first upload. Lost after this check, the
      // upload is gone too, and `/imports` answers 404.
      if (up.upload_id !== root) {
        const top = await create(undefined)
        if ('answer' in top) return top.answer
        if (top.up.upload_id !== root) {
          restart()
          continue
        }
      }
      // Expired or not, an import that started on it answers with its job.
      const started = await step(send, { method: 'POST', path: `${BASE}/imports` }, json({ upload_id: up.upload_id }))
      if (!gone(started)) return started as Answer
      chainTail.delete(first)
      // Bounded: the oldest file's walk just starts from the beginning.
      if (chainTail.size >= 1024) chainTail.delete(chainTail.keys().next().value!)
      after = up.upload_id
      chainTail.set(first, { root: root!, after })
    }
    return fail(404, 'not_found', 'the upload keeps expiring')
  } finally {
    await file.close()
  }
}

/** Sends the rest of the file; `null` once the OS holds all of it. */
async function sendChunks(
  file: FileHandle,
  up: Upload,
  total: number,
  send: Sender,
  onProgress: (p: ImportProgress) => void,
): Promise<DocumentsResponse | null> {
  let offset = up.received
  let resumes = 0
  while (offset < total) {
    onProgress({ sent: offset, total })
    const want = Math.min(CHUNK_BYTES, total - offset)
    const bytes = Buffer.alloc(want)
    const { bytesRead } = await file.read(bytes, 0, want, offset)
    if (bytesRead !== want) return fail(409, 'file_changed', 'the file changed while it was being sent')
    const reply = await step(send, { method: 'POST', path: `${BASE}/uploads/${up.upload_id}/chunks` }, {
      headers: {
        'Content-Type': 'application/octet-stream',
        'Upload-Offset': String(offset),
        'Chunk-Sha256': `sha256:${createHash('sha256').update(bytes).digest('hex')}`,
      },
      body: bytes,
    })
    if (reply.ok) {
      const received = (reply.data as unknown as Upload).received
      if (!isOffset(received, total) || received <= offset) {
        return fail(502, 'invalid_response', 'the upload did not move forward')
      }
      offset = received
      continue
    }
    // The OS holds a different length (a chunk that landed before a lost
    // reply, say): carry on from there, a bounded number of times.
    const at = reply.error.code === 'upload_offset_mismatch' ? reply.error.details?.['received_offset'] : undefined
    if (!isOffset(at, total) || at === offset || resumes >= MAX_RESUMES) return reply
    resumes++
    offset = at
  }
  return null
}
