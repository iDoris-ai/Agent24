import crypto from 'node:crypto'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { afterEach, describe, expect, it } from 'vitest'
import { MAX_PAGES, checkCapabilities, checkGet, checkImport, pages, waitForJob } from './checks'
import { type Sample, loadSample } from './samples'
import { type Call, answer, document, failed, fakeClient, id, job, refuse, upload } from './test/fake'

const S01 = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '..', 'docs', 'documenting', 'samples', 's01')
const NOTICE = loadSample(path.join(S01, 's01-07-mixed-building-notice'))
/** Polls a job without waiting in between. */
const QUICK = { sleep: async () => {} }
const sha = (bytes: Uint8Array) => `sha256:${crypto.createHash('sha256').update(bytes).digest('hex')}`
const UPLOAD = id('upl')
const JOB = id('job')
const DOC = id('doc')
const NOT_A = (schema: string) => expect.stringContaining(`the answer is not a ${schema}`)

const made: string[] = []
afterEach(() => {
  for (const dir of made.splice(0)) fs.rmSync(dir, { recursive: true, force: true })
})

describe('pages', () => {
  const read = async (cursors: unknown[]) => {
    const got: string[] = []
    const page = async (cursor: string) => ({ at: cursor, next_cursor: cursors.shift() as string | null })
    for await (const p of pages(page)) got.push(p.at)
    return got
  }

  it('hands on every page, from the first to the one with no cursor', async () => {
    expect(await read(['a', 'b', null])).toEqual(['', 'a', 'b'])
    expect(await read([null])).toEqual([''])
  })

  it('refuses a cursor that comes twice, or is not one, before it hands the page on', async () => {
    await expect(read(['a', 'b', 'a'])).rejects.toThrow('the pages do not end: cursor a came twice')
    await expect(read([''])).rejects.toThrow('the pages do not end: cursor  came twice')
    await expect(read([undefined])).rejects.toThrow('next_cursor is neither a cursor nor null')
    await expect(read([7])).rejects.toThrow('next_cursor is neither a cursor nor null')
    // Nothing of a page with a bad cursor is seen.
    const seen: string[] = []
    const bad = pages(async (cursor: string) => ({ at: cursor, next_cursor: 7 as never }))
    await expect((async () => { for await (const p of bad) seen.push(p.at) })()).rejects.toThrow()
    expect(seen).toEqual([])
  })

  it('stops a route whose every page names another', async () => {
    let n = 0
    const endless = pages(async () => ({ next_cursor: String((n += 1)) }))
    await expect((async () => { for await (const _ of endless) void _ })()).rejects.toThrow(`the pages do not end: more than ${MAX_PAGES}`)
    expect(n).toBe(MAX_PAGES + 1)
  })
})

describe('ACC-CAP', () => {
  const capabilities = (change: (c: Record<string, any>) => void = () => {}) => {
    const c = {
      os_version: '0.1.0',
      storage: { state: 'ready' },
      engines: [
        { id: 'apple-pdfkit', kind: 'parse', formats: [], state: 'ready', version: '15.2.0' },
        { id: 'apple-pdfkit', kind: 'render', formats: [], state: 'ready', version: '15.2.0' },
      ],
      knowledge: { setting: 'on', state: 'unavailable' },
      operations: [{ op: 'find', available: true, reason: null }, { op: 'extract', available: true, reason: null }],
    }
    change(c)
    return c
  }
  const run = async (change?: (c: Record<string, any>) => void) => checkCapabilities(fakeClient(() => answer(capabilities(change))).client)

  it('passes when storage, both engines and extraction are there, and hands the answer on', async () => {
    const result = await run()
    expect(result).toMatchObject({ id: 'ACC-CAP', outcome: 'passed', problems: [] })
    expect(result.capabilities?.engines).toHaveLength(2)
  })

  it('is unmet, not failed, for each thing the environment lacks', async () => {
    expect(await run((c) => void (c.storage.state = 'unavailable'))).toMatchObject({ outcome: 'unmet', problems: ['storage is unavailable'] })
    expect(await run((c) => void (c.engines[1].state = 'absent'))).toMatchObject({ outcome: 'unmet', problems: ['no render engine is ready'] })
    expect(await run((c) => void c.engines.shift())).toMatchObject({ outcome: 'unmet', problems: ['no parse engine is ready'] })
    expect(await run((c) => void Object.assign(c.operations[1], { available: false, reason: 'engine_unavailable' }))).toMatchObject({
      outcome: 'unmet',
      problems: ['extract is not available (engine_unavailable)'],
    })
    expect(await run((c) => void c.operations.pop())).toMatchObject({ outcome: 'unmet', problems: ['extract is not available (not listed)'] })
  })

  it('fails, and is not unmet, when the answer is not what the contract says', async () => {
    const broken: ((c: Record<string, any>) => void)[] = [
      (c) => void (c.storage.state = 'banana'),
      (c) => void (c.storage = {}),
      (c) => void (c.engines[0].state = 'banana'),
      (c) => void (c.operations[1].available = 'true'),
      (c) => void (c.engines = 'none'),
      (c) => void delete c.operations,
    ]
    for (const change of broken) {
      const result = await run(change)
      expect(result, JSON.stringify(capabilities(change))).toMatchObject({ outcome: 'failed', problems: [NOT_A('DocumentsCapabilities')] })
      expect(result.capabilities).toBeUndefined()
    }
    expect((await checkCapabilities(fakeClient(() => answer(null)).client)).outcome).toBe('failed')
  })

  it('fails when the daemon does not answer at all', async () => {
    expect(await checkCapabilities(fakeClient(() => refuse(404, 'not_found')).client)).toMatchObject({ outcome: 'failed', problems: ['404 not_found: it is not_found'] })
  })
})

/** A daemon that takes an upload chunk by chunk and imports it, as the routes do. */
function importing(change: { created?: object; createdStatus?: number; upload?: object; chunkStatus?: number; started?: object; startedStatus?: number; startedRaw?: () => Response; job?: object } = {}) {
  let received = 0
  let polls = 0
  let size = 0
  let hash = ''
  return (call: Call): Response => {
    if (call.route === '/uploads') {
      const asked = call.json as { total_size: number; sha256: string }
      size = asked.total_size
      hash = asked.sha256
      return answer(upload({ total_size: size, sha256: hash, ...change.created }), change.createdStatus ?? 201)
    }
    if (call.route === `/uploads/${UPLOAD}/chunks`) {
      received += call.bytes?.length ?? 0
      return answer(upload({ total_size: size, sha256: hash, received, status: received === size ? 'complete' : 'receiving', ...change.upload }), change.chunkStatus ?? 200)
    }
    if (call.route === '/imports') return change.startedRaw?.() ?? answer(change.started ?? job({ kind: 'import' }), change.startedStatus ?? 202)
    if (call.route === `/jobs/${JOB}`) {
      polls += 1
      if (polls < 3) return answer(job({ kind: 'import', status: polls === 1 ? 'queued' : 'running' }))
      return answer(job({ kind: 'import', status: 'succeeded', result: { document_id: DOC, revision: 1 }, ...change.job }))
    }
    return refuse(404, 'not_found')
  }
}

describe('ACC-IMPORT', () => {
  it('uploads the source in one chunk when it fits, imports it, and waits for the job', async () => {
    const { client, calls } = fakeClient(importing())
    const bytes = fs.readFileSync(NOTICE.source)
    expect(await checkImport(client, NOTICE, QUICK)).toMatchObject({ id: 'ACC-IMPORT', sample: NOTICE.id, outcome: 'passed', problems: [], documentId: DOC })
    const poll = `GET /jobs/${JOB}`
    expect(calls.map((c) => `${c.method} ${c.route}`)).toEqual(['POST /uploads', `POST /uploads/${UPLOAD}/chunks`, 'POST /imports', poll, poll, poll])
    expect(calls[0]?.json).toEqual({ total_size: bytes.length, sha256: sha(bytes), filename: 'source.pdf' })
    expect(calls[1]?.headers).toMatchObject({ 'upload-offset': '0', 'chunk-sha256': sha(bytes) })
    expect(calls[2]?.json).toEqual({ upload_id: UPLOAD, title: NOTICE.id })
  })

  it('cuts a larger source into chunks of 768 KiB, each with its offset and its own hash', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'accept-'))
    made.push(dir)
    const bytes = crypto.randomBytes(2 * 768 * 1024 + 5)
    const big: Sample = { ...NOTICE, source: path.join(dir, 'source.pdf') }
    fs.writeFileSync(big.source, bytes)
    const { client, calls } = fakeClient(importing())
    expect((await checkImport(client, big, QUICK)).outcome).toBe('passed')
    const chunks = calls.filter((c) => c.route.endsWith('/chunks'))
    expect(chunks.map((c) => [c.headers['upload-offset'], c.bytes?.length])).toEqual([['0', 786432], ['786432', 786432], ['1572864', 5]])
    expect(chunks.map((c) => c.headers['chunk-sha256'])).toEqual(chunks.map((c) => sha(c.bytes!)))
    expect(Buffer.concat(chunks.map((c) => c.bytes!)).equals(bytes)).toBe(true)
    // Before its last chunk the upload is still receiving.
    const early = await checkImport(fakeClient(importing({ upload: { status: 'complete' } })).client, big, QUICK)
    expect(early.problems).toEqual([`the upload is complete with 786432 bytes after 786432 of ${bytes.length} were sent`])
  })

  it('uploads under a new key every time, so that every run gets a new document', async () => {
    const keys: string[] = []
    for (let run = 0; run < 2; run += 1) {
      const { client, calls } = fakeClient(importing())
      await checkImport(client, NOTICE, QUICK)
      keys.push(calls[0]!.headers['idempotency-key']!)
    }
    expect(keys[0]).toMatch(/^[0-9a-f-]{36}$/)
    expect(keys[0]).not.toBe(keys[1])
  })

  it('fails with what went wrong: an upload left incomplete, or a job that did not succeed', async () => {
    const problems = async (change: Parameters<typeof importing>[0]) => (await checkImport(fakeClient(importing(change)).client, NOTICE, QUICK)).problems
    expect(await problems({ upload: { status: 'receiving', received: 10 } })).toEqual([expect.stringMatching(/^the upload is receiving with 10 bytes after (\d+) of \1 were sent$/)])
    expect(await problems({ upload: { status: 'complete', received: 10 } })).toEqual([expect.stringMatching(/^the upload is complete with 10 bytes after (\d+) of \1 were sent$/)])
    expect(await problems({ job: failed('upload_checksum_mismatch', { kind: 'import' }) })).toEqual(['the import job is failed: upload_checksum_mismatch'])
    expect(await problems({ job: { status: 'interrupted', result: null } })).toEqual(['the import job is interrupted: no error code'])
    const refused = await checkImport(fakeClient(() => refuse(503, 'storage_unavailable')).client, NOTICE, QUICK)
    expect(refused).toMatchObject({ outcome: 'failed', problems: ['503 storage_unavailable: it is storage_unavailable'] })
    expect(refused.documentId).toBeUndefined()
  })

  it('fails when a route answers with a status it does not give: a new upload is 201, a chunk 200, a new import 202', async () => {
    const problems = async (change: Parameters<typeof importing>[0]) => {
      const imported = await checkImport(fakeClient(importing(change)).client, NOTICE, QUICK)
      if (imported.problems.length > 0) expect(imported.documentId).toBeUndefined()
      return imported.problems
    }
    expect(await problems({ createdStatus: 200 })).toEqual([expect.stringContaining(`POST /uploads answered 200, not 201: `)])
    expect(await problems({ createdStatus: 202 })).toEqual([expect.stringContaining(`POST /uploads answered 202, not 201: `)])
    expect(await problems({ chunkStatus: 201 })).toEqual([expect.stringContaining(`POST /uploads/${UPLOAD}/chunks answered 201, not 200: `)])
    expect(await problems({ startedStatus: 200 })).toEqual(['a new import was answered 200 with a job of kind import that is queued'])
    expect(await problems({ startedStatus: 201 })).toEqual(['a new import was answered 201 with a job of kind import that is queued'])
    // A new import is a job still to run, and an import.
    expect(await problems({ started: job({ kind: 'import', status: 'succeeded', result: { document_id: DOC, revision: 1 } }) })).toEqual(['a new import was answered 202 with a job of kind import that is succeeded'])
    expect(await problems({ started: job({ kind: 'import', status: 'cancelled' }), startedStatus: 200 })).toEqual(['a new import was answered 200 with a job of kind import that is cancelled'])
    expect(await problems({ started: job({ kind: 'extract' }) })).toEqual(['a new import was answered 202 with a job of kind extract that is queued'])
    expect(await problems({ started: job({ kind: 'import', status: 'running' }) })).toEqual([])
    expect(await problems({ started: { status: 'queued' } })).toEqual([NOT_A('DocumentsJob')])
    expect(await problems({ startedRaw: () => new Response(null, { status: 204 }) })).toEqual(['POST /imports answered 204 with no JSON'])
  })

  it('fails when the upload is not of the bytes that were announced, a chunk is answered with another upload, or the job that ran is no import', async () => {
    const problems = async (change: Parameters<typeof importing>[0]) => (await checkImport(fakeClient(importing(change)).client, NOTICE, QUICK)).problems
    const size = fs.statSync(NOTICE.source).size
    expect(await problems({ created: { total_size: size + 1 } })).toEqual([`the upload is of ${size + 1} bytes, sha256:${NOTICE.meta.sha256_stored}: not what was announced`])
    expect(await problems({ created: { sha256: `sha256:${'c'.repeat(64)}` } })).toEqual([`the upload is of ${size} bytes, sha256:${'c'.repeat(64)}: not what was announced`])
    expect(await problems({ upload: { upload_id: id('upl', 2) } })).toEqual([`a chunk of ${UPLOAD} was answered with ${id('upl', 2)}`])
    expect(await problems({ upload: { total_size: 1 } })).toEqual([`a chunk of ${UPLOAD} was answered with an upload of 1 bytes, sha256:${NOTICE.meta.sha256_stored}`])
    expect(await problems({ upload: { sha256: `sha256:${'c'.repeat(64)}` } })).toEqual([`a chunk of ${UPLOAD} was answered with an upload of ${size} bytes, sha256:${'c'.repeat(64)}`])
    // A new upload holds nothing yet; after each chunk it holds what was sent, and is complete only at the end.
    expect(await problems({ created: { received: 7 } })).toEqual(['a new upload is receiving with 7 bytes'])
    expect(await problems({ created: { status: 'complete' } })).toEqual(['a new upload is complete with 0 bytes'])
    expect(await problems({ upload: { received: size - 1 } })).toEqual([`the upload is complete with ${size - 1} bytes after ${size} of ${size} were sent`])
    expect(await problems({ upload: { status: 'receiving' } })).toEqual([`the upload is receiving with ${size} bytes after ${size} of ${size} were sent`])
    expect(await problems({ job: { kind: 'extract', result: { extraction_id: id('ext') } } })).toEqual(['the import job is of kind extract'])
  })

  it('fails, and names no document, when the job it waited for is not what the contract says', async () => {
    for (const result of [{ document_id: 5, revision: 1 }, { document_id: { x: 1 }, revision: 1 }, { document_id: 'doc_1', revision: 1 }, { revision: 1 }, null]) {
      const imported = await checkImport(fakeClient(importing({ job: { result } })).client, NOTICE, QUICK)
      expect(imported, JSON.stringify(result)).toMatchObject({ outcome: 'failed', problems: [NOT_A('DocumentsJob')] })
      expect(imported.documentId).toBeUndefined()
    }
    const odd = async (change: Parameters<typeof importing>[0]) => (await checkImport(fakeClient(importing(change)).client, NOTICE, QUICK)).problems
    expect(await odd({ job: { status: 'done' } })).toEqual([NOT_A('DocumentsJob')])
    expect(await odd({ job: { error: { code: 'internal', message: 'x' } } })).toEqual([NOT_A('DocumentsJob')])
    expect(await odd({ upload: { status: 'done' } })).toEqual([NOT_A('DocumentsUpload')])
  })
})

describe('the status of an answer', () => {
  it('fails every check whose route answers 2xx, but not the one the route gives', async () => {
    const off = (route: string, status: number, d: (call: Call) => Response) => (call: Call) => {
      const real = d(call)
      return call.route.split('?')[0] === route ? new Response(real.body, { status, headers: real.headers }) : real
    }
    const caps = await checkCapabilities(fakeClient(() => answer({ os_version: '0.1.0', storage: { state: 'ready' }, engines: [], knowledge: { setting: 'on', state: 'ready' }, operations: [] }, 203)).client)
    expect(caps).toMatchObject({ outcome: 'failed', problems: [expect.stringContaining(`GET /capabilities answered 203, not 200: `)] })
    expect((await checkImport(fakeClient(off(`/jobs/${JOB}`, 202, importing())).client, NOTICE, QUICK)).problems).toEqual([expect.stringContaining(`GET /jobs/${JOB} answered 202, not 200: `)])
    const stored = (call: Call) => (call.route.startsWith('/documents?') || call.route === '/documents' ? answer({ documents: [document({ document_id: DOC })], next_cursor: null }) : answer(document({ document_id: DOC, head: { revision: 1, content_sha256: `sha256:${NOTICE.meta.sha256_stored}`, size: 1, media_type: 'application/pdf' } })))
    expect((await checkGet(fakeClient(stored).client, NOTICE, DOC)).problems).toEqual([])
    expect((await checkGet(fakeClient(off(`/documents/${DOC}`, 203, stored)).client, NOTICE, DOC)).problems).toEqual([expect.stringContaining(`GET /documents/${DOC} answered 203, not 200: `)])
    expect((await checkGet(fakeClient(off('/documents', 206, stored)).client, NOTICE, DOC)).problems).toEqual([expect.stringContaining(`GET /documents answered 206, not 200: `)])
  })
})

describe('waitForJob', () => {
  it('goes on asking while the job is queued, running or cancelling, and no longer than it may', async () => {
    const states = ['queued', 'running', 'cancelling', 'cancelled']
    const { client, calls } = fakeClient((_c, all) => answer(job({ status: states[all.length - 1] })))
    expect((await waitForJob(client, JOB, QUICK)).status).toBe('cancelled')
    expect(calls).toHaveLength(4)
    const stuck = fakeClient(() => answer(job({ status: 'running' })))
    await expect(waitForJob(stuck.client, JOB, { ...QUICK, timeoutMs: 0 })).rejects.toThrow(`job ${JOB} is still running`)
  })

  it('refuses the answer for another job', async () => {
    const other = fakeClient(() => answer(job({ job_id: id('job', 2), status: 'succeeded', result: { extraction_id: id('ext') } })))
    await expect(waitForJob(other.client, JOB, QUICK)).rejects.toThrow(`asked for job ${JOB}, answered with ${id('job', 2)}`)
  })
})

describe('ACC-GET', () => {
  const stored = `sha256:${NOTICE.meta.sha256_stored}`
  const mine = (more: Record<string, unknown> = {}) => document({ document_id: DOC, ...more, head: { revision: 1, content_sha256: stored, size: 1, media_type: 'application/pdf', ...(more.head as object) } })
  const daemon = (detail: object, listed: string[][], cursor = (at: number) => String(at)) => (call: Call) => {
    if (call.route === `/documents/${DOC}`) return answer(detail)
    const at = call.route === '/documents' ? 0 : Number(new URLSearchParams(call.route.split('?')[1]).get('cursor'))
    return answer({ documents: listed[at]!.map((document_id) => document({ document_id })), next_cursor: at + 1 < listed.length ? cursor(at + 1) : null })
  }
  const run = async (detail: object = mine(), listed = [[id('doc', 9), DOC]]) => checkGet(fakeClient(daemon(detail, listed)).client, NOTICE, DOC)

  it('passes when the document is there at revision 1, holds the source byte for byte, and is listed', async () => {
    expect(await run()).toMatchObject({ id: 'ACC-GET', sample: NOTICE.id, outcome: 'passed', problems: [] })
  })

  it('looks through every page of the list', async () => {
    const { client, calls } = fakeClient(daemon(mine(), [[id('doc', 8)], [id('doc', 9)], [DOC], [id('doc', 2)]]))
    expect((await checkGet(client, NOTICE, DOC)).outcome).toBe('passed')
    expect(calls.map((c) => c.route)).toEqual([`/documents/${DOC}`, '/documents', '/documents?cursor=1', '/documents?cursor=2'])
    expect((await run(mine(), [[id('doc', 8)], [id('doc', 9)]])).problems).toEqual(['the document is not in the list'])
  })

  it('fails on another document, another revision, or content that is not the sample', async () => {
    expect((await run(mine({ document_id: id('doc', 4) }))).problems).toEqual([`asked for ${DOC}, answered with ${id('doc', 4)}`])
    expect((await run(mine({ head_revision: 2 }))).problems).toEqual(['head_revision is 2'])
    expect((await run(mine({ head: { content_sha256: `sha256:${'0'.repeat(64)}` } }))).problems).toEqual([`the stored content is sha256:${'0'.repeat(64)}, not the sample's sha256_stored`])
  })

  it('fails on a document or a list that is not what the contract says', async () => {
    expect((await run({ ...mine(), head: undefined })).problems).toEqual([NOT_A('DocumentDetail')])
    expect((await run(mine({ head_revision: '1' }))).problems).toEqual([NOT_A('DocumentDetail')])
    const broken = (call: Call) => (call.route === `/documents/${DOC}` ? answer(mine()) : answer({ documents: 'none', next_cursor: null }))
    expect((await checkGet(fakeClient(broken).client, NOTICE, DOC)).problems).toEqual([NOT_A('DocumentList')])
  })

  it('fails, and does not go on for ever, on a list whose pages do not end', async () => {
    const turning = (next: unknown, documents: object[] = []) => (call: Call) => (call.route === `/documents/${DOC}` ? answer(mine()) : answer({ documents, next_cursor: next }))
    const problems = async (next: unknown, documents?: object[]) => (await checkGet(fakeClient(turning(next, documents)).client, NOTICE, DOC)).problems
    // The first page is asked for without a cursor, so the cursor 'again' is given twice.
    expect(await problems('again')).toEqual(['the pages do not end: cursor again came twice'])
    expect(await problems(undefined)).toEqual([NOT_A('DocumentList')])
    expect(await problems(7)).toEqual([NOT_A('DocumentList')])
    // A page with a cursor that is none is not read, though the document is on it.
    expect(await problems(7, [document({ document_id: DOC })])).toEqual([NOT_A('DocumentList')])
    let n = 0
    const endless = fakeClient((call) => (call.route === `/documents/${DOC}` ? answer(mine()) : answer({ documents: [], next_cursor: String((n += 1)) })))
    expect((await checkGet(endless.client, NOTICE, DOC)).problems).toEqual([`the pages do not end: more than ${MAX_PAGES}`])
  })
})
