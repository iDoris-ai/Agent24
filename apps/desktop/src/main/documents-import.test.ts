import { createHash } from 'node:crypto'
import { mkdtemp, rm, truncate, unlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { CHUNK_BYTES, forgetChains, importFile, type ImportProgress } from './documents-import'
import type { Reply, Route, SendOptions } from './documents'

const sha = (b: Buffer): string => `sha256:${createHash('sha256').update(b).digest('hex')}`
const reply = (status: number, body: unknown): Reply => ({ status, body: Buffer.from(JSON.stringify(body)) })
const kernelTimeout = (): Reply => reply(504, { error: { code: 'upstream_timeout', message: 'module took too long' } })
const localTimeout = (): Reply => ({ status: 504, error: { code: 'backend_timeout', message: 'outcome unknown' } })

interface FakeUpload {
  id: string
  request: string
  total: number
  sha256: string
  bytes: Buffer
  chunks: Map<number, string>
  status: string
  expiresAt: string
}

const LATER = '2999-01-01T00:00:00.000Z'
const view = (up: FakeUpload) => ({
  upload_id: up.id,
  total_size: up.total,
  sha256: up.sha256,
  received: up.bytes.length,
  status: up.status,
  expires_at: up.expiresAt,
})

/** An OS that follows the contract: uploads by Idempotency-Key, appends at
 * `received`, replays a received chunk with the same hash as 200, checks the
 * whole file at import, and imports each upload once. `drop(kind)` loses the
 * reply of a step that did happen (a lost reply, not a lost request). */
function fakeOs(drop: (kind: string, n: number) => Reply | null = () => null, era = 'M') {
  const byKey = new Map<string, FakeUpload>()
  const uploads = new Map<string, FakeUpload>()
  const jobs = new Map<string, { job_id: string; kind: string; status: string }>()
  const calls: Array<{ kind: string; route: Route; options: SendOptions }> = []
  const count: Record<string, number> = {}
  const send = async (route: Route, options: SendOptions = {}): Promise<Reply> => {
    const kind = route.path.endsWith('/uploads') ? 'create' : route.path.endsWith('/chunks') ? 'chunk' : 'import'
    calls.push({ kind, route, options })
    const n = (count[kind] = (count[kind] ?? 0) + 1)
    let answer: Reply
    if (kind === 'create') {
      const request = options.body!.toString()
      const body = JSON.parse(request)
      if (body.total_size < 1) return reply(400, { error: { code: 'invalid_request', message: 'total_size' } })
      const key = options.headers!['Idempotency-Key']!
      let up = byKey.get(key)
      // The real OS hashes the request: the same key with another one is 422.
      if (up && up.request !== request) return reply(422, { error: { code: 'idempotency_key_reused', message: 'x' } })
      if (!up) {
        up = {
          id: `upl_01K74Z3QJ8V5N2W9RTX6YB4${era}${String(byKey.size).padStart(2, '0')}`,
          request,
          total: body.total_size,
          sha256: body.sha256,
          bytes: Buffer.alloc(0),
          chunks: new Map(),
          status: 'receiving',
          expiresAt: LATER,
        }
        byKey.set(key, up)
        uploads.set(up.id, up)
        answer = reply(201, view(up))
      } else answer = reply(200, view(up))
    } else if (kind === 'chunk') {
      const up = uploads.get(route.path.split('/')[5]!)
      if (!up) return reply(404, { error: { code: 'not_found', message: 'no such upload' } })
      // Past its deadline: the chunk request marks it expired and is 404.
      if (up.status !== 'imported' && Date.parse(up.expiresAt) <= Date.now()) {
        up.status = 'expired'
        return reply(404, { error: { code: 'not_found', message: 'the upload has expired' } })
      }
      const at = Number(options.headers!['Upload-Offset'])
      const claimed = options.headers!['Chunk-Sha256']!
      const state = () => reply(200, view(up))
      if (at < up.bytes.length && up.chunks.get(at) === claimed) answer = state()
      else if (at !== up.bytes.length) {
        answer = reply(409, { error: { code: 'upload_offset_mismatch', message: 'x', details: { retryable: false, received_offset: up.bytes.length } } })
      } else {
        up.chunks.set(at, claimed)
        up.bytes = Buffer.concat([up.bytes, options.body!])
        answer = state()
      }
    } else {
      const upId = JSON.parse(options.body!.toString()).upload_id
      const up = uploads.get(upId)
      // A key hit first: an import already started answers with its job,
      // whatever the upload's deadline (the real OS checks the key first).
      const started = jobs.get(upId)
      if (started) return drop(kind, n) ?? reply(202, started)
      if (!up) return reply(404, { error: { code: 'not_found', message: 'no such upload' } })
      if (Date.parse(up.expiresAt) <= Date.now()) return reply(404, { error: { code: 'not_found', message: 'the upload has expired' } })
      if (sha(up.bytes) !== up.sha256) {
        return reply(422, { error: { code: 'upload_checksum_mismatch', message: 'x' } })
      }
      // Stays complete until its job commits; that is not modelled here.
      up.status = 'complete'
      let job = jobs.get(upId)
      if (!job) {
        job = { job_id: `job_01K75A0B1C2D3E4F5G6H7J8K${String(jobs.size).padStart(2, '0')}`, kind: 'import', status: 'queued' }
        jobs.set(upId, job)
      }
      answer = reply(202, job)
    }
    return drop(kind, n) ?? answer
  }
  return { send, calls, uploads, jobs, chunks: () => calls.filter((c) => c.kind === 'chunk') }
}

let dir = ''
beforeEach(async () => {
  forgetChains()
  dir = await mkdtemp(join(tmpdir(), 'a24-import-'))
})
afterEach(async () => {
  await rm(dir, { recursive: true, force: true })
})

let fileNo = 0
async function file(size: number, name = '通告 2026.pdf'): Promise<{ path: string; bytes: Buffer }> {
  // Distinct content per file, so the in-progress memory never links tests.
  fileNo++
  const bytes = Buffer.alloc(size)
  for (let i = 0; i < size; i++) bytes[i] = (i * 31 + fileNo * 7) % 251
  const path = join(dir, name)
  await writeFile(path, bytes)
  return { path, bytes }
}

describe('importFile', () => {
  it('declares the file, sends it in chunks of at most 768 KiB, then starts the import', async () => {
    const { path, bytes } = await file(2 * CHUNK_BYTES + 10)
    const os = fakeOs()
    const progress: ImportProgress[] = []
    const res = await importFile(path, os.send, (p) => progress.push(p))
    expect(res).toMatchObject({ ok: true, status: 202, data: { kind: 'import' } })
    const up = [...os.uploads.values()][0]!
    expect(up.bytes.equals(bytes)).toBe(true)
    const create = os.calls[0]!
    expect(JSON.parse(create.options.body!.toString())).toEqual({ total_size: bytes.length, sha256: sha(bytes), filename: '通告 2026.pdf' })
    expect(create.options.headers!['Idempotency-Key']).toMatch(/^desktop-[0-9a-f]{64}$/)
    const chunks = os.chunks()
    expect(chunks.map((c) => c.options.body!.length)).toEqual([CHUNK_BYTES, CHUNK_BYTES, 10])
    expect(chunks.map((c) => c.options.headers!['Upload-Offset'])).toEqual(['0', String(CHUNK_BYTES), String(2 * CHUNK_BYTES)])
    for (const c of chunks) expect(c.options.headers!['Chunk-Sha256']).toBe(sha(c.options.body!))
    expect(progress.at(-1)).toEqual({ sent: bytes.length, total: bytes.length })
  })

  it('a chunk whose reply was lost is replayed as 200, and a kernel timeout is retried', async () => {
    const { path, bytes } = await file(CHUNK_BYTES + 5)
    const os = fakeOs((kind, n) => (kind === 'chunk' && n === 1 ? localTimeout() : kind === 'chunk' && n === 3 ? kernelTimeout() : null))
    expect((await importFile(path, os.send)).ok).toBe(true)
    expect([...os.uploads.values()][0]!.bytes.equals(bytes)).toBe(true)
    expect(os.jobs.size).toBe(1)
  })

  it('an import whose replies were all lost is continued, not repeated, by importing the file again', async () => {
    const { path } = await file(10)
    let lose = true
    const os = fakeOs((kind) => (kind === 'import' && lose ? localTimeout() : null))
    const first = await importFile(path, os.send)
    expect(first).toMatchObject({ ok: false, error: { code: 'backend_timeout' } })
    lose = false
    const second = await importFile(path, os.send)
    expect(second.ok).toBe(true)
    // One upload, one job: the second run reused the first run's key.
    expect([os.uploads.size, os.jobs.size]).toEqual([1, 1])
  })

  it('an upload cut off midway resumes from what the OS already holds', async () => {
    const { path } = await file(2 * CHUNK_BYTES + 1)
    let cut = true
    const os = fakeOs((kind, n) => (kind === 'chunk' && n >= 2 && cut ? localTimeout() : null))
    expect((await importFile(path, os.send)).ok).toBe(false)
    cut = false
    const before = os.chunks().length
    expect((await importFile(path, os.send)).ok).toBe(true)
    // The second run starts where the first stopped, not at 0.
    expect(os.chunks()[before]!.options.headers!['Upload-Offset']).not.toBe('0')
    expect(os.uploads.size).toBe(1)
  })

  it('a file that shrinks while it is sent stops with file_changed, never loops', async () => {
    const { path } = await file(2 * CHUNK_BYTES)
    const os = fakeOs()
    // Shrink the file right after its first chunk was sent.
    const res = await importFile(path, async (route, options) => {
      const r = await os.send(route, options)
      if (route.path.endsWith('/chunks')) await truncate(path, 10)
      return r
    })
    expect(res).toMatchObject({ ok: false, error: { code: 'file_changed' } })
    expect(os.chunks().length).toBeLessThanOrEqual(2)
  })

  it('refuses replies that do not fit the file, and stops resuming after a bound', async () => {
    const { path } = await file(10)
    const answer = (create: unknown, chunk: () => Reply) => async (route: Route): Promise<Reply> =>
      route.path.endsWith('/uploads') ? reply(201, create) : chunk()
    const good = { upload_id: 'upl_01K74Z3QJ8V5N2W9RTX6YB4MCD', total_size: 10, received: 0, status: 'receiving', expires_at: LATER }
    const cases: Array<[string, unknown, () => Reply]> = [
      ['an id that is not one', { ...good, upload_id: '../x' }, () => reply(200, {})],
      ['received past the size', { ...good, received: 11 }, () => reply(200, {})],
      ['another file', { ...good, total_size: 9 }, () => reply(200, {})],
      ['received not an integer', { ...good, received: 1.5 }, () => reply(200, {})],
    ]
    for (const [name, create, chunk] of cases) {
      const sent: Route[] = []
      const res = await importFile(path, async (route, o) => {
        sent.push(route)
        return answer({ ...(create as object), sha256: JSON.parse(o!.body!.toString()).sha256 ?? '' }, chunk)(route)
      })
      expect(res, name).toMatchObject({ ok: false, error: { code: 'invalid_response' } })
      expect(sent, name).toHaveLength(1)
    }
    // An upload of other bytes under the same size.
    // The rest would succeed, so only the hash check can refuse it.
    const other = await importFile(path, answer({ ...good, sha256: sha(Buffer.from('other')) }, () => reply(200, { received: 10 })))
    expect(other).toMatchObject({ ok: false, error: { code: 'invalid_response' } })
    // A chunk accepted without moving forward, and offsets that go round.
    const stuck = await importFile(path, async (route, o) =>
      route.path.endsWith('/uploads')
        ? reply(201, { ...good, sha256: JSON.parse(o!.body!.toString()).sha256 })
        : reply(200, { received: 0 }),
    )
    expect(stuck).toMatchObject({ ok: false, error: { code: 'invalid_response' } })
    let flip = 0
    const cycling = await importFile(path, async (route, o) =>
      route.path.endsWith('/uploads')
        ? reply(201, { ...good, sha256: JSON.parse(o!.body!.toString()).sha256 })
        : reply(409, { error: { code: 'upload_offset_mismatch', message: 'x', details: { received_offset: ((flip++ + 1) % 2) * 5 } } }),
    )
    expect(cycling).toMatchObject({ ok: false, status: 409 })
    expect(flip).toBeLessThanOrEqual(10)
  })

  it('a file it cannot read is file_unreadable, without its path', async () => {
    const { path } = await file(10, 'secret-name.pdf')
    await unlink(path)
    const res = await importFile(path, fakeOs().send)
    expect(res).toMatchObject({ ok: false, error: { code: 'file_unreadable' } })
    expect(JSON.stringify(res)).not.toContain('secret-name')
  })

  it('a name the contract forbids is not sent; an OS error is returned as it is', async () => {
    const { path } = await file(10, 'report\\draft.pdf')
    const os = fakeOs()
    expect((await importFile(path, os.send)).ok).toBe(true)
    expect(JSON.parse(os.calls[0]!.options.body!.toString())).not.toHaveProperty('filename')
    const empty = await file(0, 'empty.pdf')
    const res = await importFile(empty.path, fakeOs().send)
    expect(res).toMatchObject({ ok: false, status: 400, error: { code: 'invalid_request' } })
  })
})

describe('importFile, one file imported more than once', () => {
  it('a retry, a second import or two at once reach one upload and one job', async () => {
    const { path } = await file(CHUNK_BYTES + 3)
    let lose = true
    // The kernel loses /imports replies (upstream_unavailable: maybe sent).
    const os = fakeOs((kind) => (kind === 'import' && lose ? reply(502, { error: { code: 'upstream_unavailable', message: 'x' } }) : null))
    expect(await importFile(path, os.send)).toMatchObject({ ok: false, error: { code: 'upstream_unavailable' } })
    lose = false
    const [a, b] = await Promise.all([importFile(path, os.send), importFile(path, os.send)])
    expect([a.ok, b.ok]).toEqual([true, true])
    expect([os.uploads.size, os.jobs.size]).toEqual([1, 1])
    expect(a.ok && b.ok && a.data.job_id === b.data.job_id).toBe(true)
  })

  it('a renamed copy is a new import, never a reused key with another request', async () => {
    const first = await file(10, 'a.pdf')
    const os = fakeOs()
    expect((await importFile(first.path, os.send)).ok).toBe(true)
    const copy = join(dir, 'b.pdf')
    await writeFile(copy, first.bytes)
    expect((await importFile(copy, os.send)).ok).toBe(true)
    expect([os.uploads.size, os.jobs.size]).toEqual([2, 2])
  })

  it('an upload that expired before its import is replaced by a new one', async () => {
    const { path } = await file(10)
    let cut = true
    const os = fakeOs((kind) => (kind === 'chunk' && cut ? localTimeout() : null))
    expect((await importFile(path, os.send)).ok).toBe(false)
    // 24 h later the OS has let it lapse.
    const old = [...os.uploads.values()][0]!
    old.expiresAt = '2000-01-01T00:00:00.000Z'
    cut = false
    const res = await importFile(path, os.send)
    expect(res.ok).toBe(true)
    expect(os.uploads.size).toBe(2)
    const keys = os.calls.filter((c) => c.kind === 'create').map((c) => c.options.headers!['Idempotency-Key'])
    expect(new Set(keys).size).toBe(2)
  })
})

it('a reply whose outcome is unknown, or that was never dispatched, is sent again (ADR-DOC-02 §6)', async () => {
  for (const code of ['upstream_unavailable', 'module_panicked', 'module_killed', 'stop_failed', 'duplicate_request_id']) {
    const { path } = await file(10)
    const os = fakeOs((kind, n) => (kind === 'import' && n === 1 ? reply(502, { error: { code, message: 'x' } }) : null))
    expect((await importFile(path, os.send)).ok, code).toBe(true)
    expect(os.calls.filter((c) => c.kind === 'import'), code).toHaveLength(2)
    expect(os.jobs.size, code).toBe(1)
  }
})

it('an import that started before its upload expired is found again, never repeated', async () => {
  const { path } = await file(10)
  let lose = true
  const os = fakeOs((kind) => (kind === 'import' && lose ? localTimeout() : null))
  expect((await importFile(path, os.send)).ok).toBe(false)
  // The job exists; then the upload's deadline passes (the reply only says
  // `complete`, and main's clock must not decide that it is gone).
  ;[...os.uploads.values()][0]!.expiresAt = '2000-01-01T00:00:00.000Z'
  lose = false
  const res = await importFile(path, os.send)
  expect(res.ok).toBe(true)
  expect([os.uploads.size, os.jobs.size]).toEqual([1, 1])
})

it('more expired uploads than any small bound still lead to an import', async () => {
  const { path } = await file(10)
  let cut = true
  const os = fakeOs((kind) => (kind === 'chunk' && cut ? localTimeout() : null))
  for (let i = 0; i < 6; i++) {
    expect((await importFile(path, os.send)).ok).toBe(false)
    for (const up of os.uploads.values()) up.expiresAt = '2000-01-01T00:00:00.000Z'
  }
  cut = false
  expect((await importFile(path, os.send)).ok).toBe(true)
  expect(os.jobs.size).toBe(1)
  expect(os.uploads.size).toBeGreaterThan(5)
})

it('an upload that expires midway is replaced in the same run, from its chunk request 404', async () => {
  const { path, bytes } = await file(CHUNK_BYTES + 7)
  let cut = true
  const os = fakeOs()
  // The first chunk lands; the second never reaches the OS.
  const send = (route: Route, options?: SendOptions): Promise<Reply> =>
    cut && route.path.endsWith('/chunks') && options?.headers?.['Upload-Offset'] !== '0'
      ? Promise.resolve(localTimeout())
      : os.send(route, options)
  expect((await importFile(path, send)).ok).toBe(false)
  ;[...os.uploads.values()][0]!.expiresAt = '2000-01-01T00:00:00.000Z'
  cut = false
  const res = await importFile(path, send)
  expect(res.ok).toBe(true)
  const [old, fresh] = [...os.uploads.values()]
  expect([old!.status, fresh!.bytes.equals(bytes)]).toEqual(['expired', true])
})

it('past 64 expired uploads a run still reaches a live one, starting where the last run stopped', async () => {
  const { path } = await file(10)
  let cut = true
  const os = fakeOs((kind) => (kind === 'chunk' && cut ? localTimeout() : null))
  for (let i = 0; i < 70; i++) {
    expect((await importFile(path, os.send)).ok).toBe(false)
    for (const up of os.uploads.values()) up.expiresAt = '2000-01-01T00:00:00.000Z'
  }
  cut = false
  const creates = os.calls.filter((c) => c.kind === 'create').length
  expect((await importFile(path, os.send)).ok).toBe(true)
  expect([os.uploads.size > 64, os.jobs.size]).toEqual([true, 1])
  // It did not walk the whole chain again (two creates per upload passed over).
  expect(os.calls.filter((c) => c.kind === 'create').length - creates).toBeLessThan(8)
})

it('an expired upload whose import started answers with that job, not a new upload', async () => {
  const { path } = await file(10)
  const os = fakeOs()
  const first = await importFile(path, os.send)
  const up = [...os.uploads.values()][0]!
  Object.assign(up, { status: 'expired', expiresAt: '2000-01-01T00:00:00.000Z' })
  const again = await importFile(path, os.send)
  expect(again.ok && first.ok && again.data).toEqual(first.ok && first.data)
  expect([os.uploads.size, os.jobs.size]).toEqual([1, 1])
})

it('after the OS loses its data, the remembered place is dropped and every run takes the same upload', async () => {
  const { path } = await file(10)
  const old = fakeOs((kind) => (kind === 'chunk' ? localTimeout() : null))
  for (let i = 0; i < 3; i++) {
    expect((await importFile(path, old.send)).ok).toBe(false)
    for (const up of old.uploads.values()) up.expiresAt = '2000-01-01T00:00:00.000Z'
  }
  const os = fakeOs(undefined, 'N')
  const first = await importFile(path, os.send)
  forgetChains() // a restart
  const again = await importFile(path, os.send)
  expect(again.ok && first.ok && again.data).toEqual(first.ok && first.data)
  expect(os.jobs.size).toBe(1)
})

it('the OS losing its data between creating an upload and its chunks does not fork the chain', async () => {
  const { path } = await file(10)
  let os = fakeOs()
  let reset = false
  const send = async (route: Route, options?: SendOptions): Promise<Reply> => {
    if (!reset && route.path.endsWith('/chunks')) {
      reset = true
      os = fakeOs(undefined, 'N') // created in the old data, chunked into the new
    }
    return os.send(route, options)
  }
  const first = await importFile(path, send)
  const again = await importFile(path, send)
  expect(again.ok && first.ok && again.data).toEqual(first.ok && first.data)
  expect(os.jobs.size).toBe(1)
})

it('the OS losing its data just before the next upload in the chain does not fork it', async () => {
  const { path } = await file(10)
  let cut = true
  let os = fakeOs((kind) => (kind === 'chunk' && cut ? localTimeout() : null))
  expect((await importFile(path, os.send)).ok).toBe(false)
  for (const up of os.uploads.values()) up.expiresAt = '2000-01-01T00:00:00.000Z'
  cut = false
  const key0 = os.calls[0]!.options.headers!['Idempotency-Key']
  let reset = false
  const send = async (route: Route, options?: SendOptions): Promise<Reply> => {
    // Just before the next upload in the chain is made.
    if (!reset && route.path.endsWith('/uploads') && options?.headers?.['Idempotency-Key'] !== key0) {
      reset = true
      os = fakeOs(undefined, 'N')
    }
    return os.send(route, options)
  }
  const first = await importFile(path, send)
  expect(reset).toBe(true)
  forgetChains() // a restart
  const again = await importFile(path, send)
  expect(again.ok && first.ok && again.data).toEqual(first.ok && first.data)
  expect(os.jobs.size).toBe(1)
})
