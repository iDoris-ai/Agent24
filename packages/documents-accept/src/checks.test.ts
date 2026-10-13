import crypto from 'node:crypto'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { afterEach, describe, expect, it } from 'vitest'
import { checkCapabilities, checkGet, checkImport, waitForJob } from './checks'
import { type Sample, loadSample } from './samples'
import { type Call, answer, fakeClient, refuse } from './test/fake'

const S01 = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '..', 'docs', 'documenting', 'samples', 's01')
const NOTICE = loadSample(path.join(S01, 's01-07-mixed-building-notice'))
/** Polls a job without waiting in between. */
const QUICK = { sleep: async () => {} }
const sha = (bytes: Uint8Array) => `sha256:${crypto.createHash('sha256').update(bytes).digest('hex')}`

const made: string[] = []
afterEach(() => {
  for (const dir of made.splice(0)) fs.rmSync(dir, { recursive: true, force: true })
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
    for (const broken of [{}, { storage: { state: 'ready' }, engines: 'none', operations: [] }, null]) {
      const result = await checkCapabilities(fakeClient(() => answer(broken)).client)
      expect(result.outcome, JSON.stringify(broken)).toBe('failed')
      expect(result.problems).toHaveLength(1)
    }
  })

  it('fails when the daemon does not answer at all', async () => {
    const result = await checkCapabilities(fakeClient(() => refuse(404, 'not_found')).client)
    expect(result).toMatchObject({ outcome: 'failed', problems: ['404 not_found: it is not_found'] })
    expect(result.capabilities).toBeUndefined()
  })
})

/** A daemon that takes an upload chunk by chunk and imports it, as the routes do. */
function importing(change: { upload?: object; job?: object } = {}) {
  let received = 0
  let polls = 0
  const total = (call: Call) => (call.json as { total_size: number }).total_size
  let size = 0
  return (call: Call): Response => {
    if (call.route === '/uploads') {
      size = total(call)
      return answer({ upload_id: 'upl_1', received: 0, status: 'receiving' }, 201)
    }
    if (call.route === '/uploads/upl_1/chunks') {
      received += call.bytes?.length ?? 0
      return answer({ upload_id: 'upl_1', received, status: received === size ? 'complete' : 'receiving', ...change.upload })
    }
    if (call.route === '/imports') return answer({ job_id: 'job_1', status: 'queued' }, 202)
    if (call.route === '/jobs/job_1') {
      polls += 1
      if (polls < 3) return answer({ job_id: 'job_1', status: polls === 1 ? 'queued' : 'running' })
      return answer({ job_id: 'job_1', status: 'succeeded', result: { document_id: 'doc_1', revision: 1 }, ...change.job })
    }
    return refuse(404, 'not_found')
  }
}

describe('ACC-IMPORT', () => {
  it('uploads the source in one chunk when it fits, imports it, and waits for the job', async () => {
    const { client, calls } = fakeClient(importing())
    const bytes = fs.readFileSync(NOTICE.source)
    expect(await checkImport(client, NOTICE, QUICK)).toMatchObject({ id: 'ACC-IMPORT', sample: NOTICE.id, outcome: 'passed', problems: [], documentId: 'doc_1' })
    expect(calls.map((c) => `${c.method} ${c.route}`)).toEqual(['POST /uploads', 'POST /uploads/upl_1/chunks', 'POST /imports', 'GET /jobs/job_1', 'GET /jobs/job_1', 'GET /jobs/job_1'])
    expect(calls[0]?.json).toEqual({ total_size: bytes.length, sha256: sha(bytes), filename: 'source.pdf' })
    expect(calls[1]?.headers).toMatchObject({ 'upload-offset': '0', 'chunk-sha256': sha(bytes) })
    expect(calls[2]?.json).toEqual({ upload_id: 'upl_1', title: NOTICE.id })
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

  it('fails with what went wrong: an upload left incomplete, a job that did not succeed, no document', async () => {
    const failed = async (change: Parameters<typeof importing>[0]) => (await checkImport(fakeClient(importing(change)).client, NOTICE, QUICK)).problems
    expect(await failed({ upload: { status: 'receiving', received: 10 } })).toEqual([expect.stringMatching(/the upload is receiving after its last chunk, with 10 of \d+ bytes/)])
    expect(await failed({ upload: { status: 'complete', received: 10 } })).toEqual([expect.stringMatching(/the upload is complete after its last chunk, with 10 of \d+ bytes/)])
    expect(await failed({ job: { status: 'failed', error: { code: 'upload_checksum_mismatch', message: 'x' } } })).toEqual(['the import job is failed: upload_checksum_mismatch'])
    expect(await failed({ job: { status: 'interrupted' } })).toEqual(['the import job is interrupted: no error code'])
    expect(await failed({ job: { result: null } })).toEqual(['the import job succeeded without a document_id'])
    const refused = await checkImport(fakeClient(() => refuse(503, 'storage_unavailable')).client, NOTICE, QUICK)
    expect(refused).toMatchObject({ outcome: 'failed', problems: ['503 storage_unavailable: it is storage_unavailable'] })
    expect(refused.documentId).toBeUndefined()
  })
})

describe('waitForJob', () => {
  it('goes on asking while the job is queued, running or cancelling, and no longer than it may', async () => {
    const states = ['queued', 'running', 'cancelling', 'cancelled']
    const { client, calls } = fakeClient((_c, all) => answer({ job_id: 'j', status: states[all.length - 1] }))
    expect((await waitForJob(client, 'j', { sleep: async () => {} })).status).toBe('cancelled')
    expect(calls).toHaveLength(4)
    const stuck = fakeClient(() => answer({ job_id: 'j', status: 'running' }))
    await expect(waitForJob(stuck.client, 'j', { sleep: async () => {}, timeoutMs: 0 })).rejects.toThrow(/job j is still running/)
  })
})

describe('ACC-GET', () => {
  const stored = `sha256:${NOTICE.meta.sha256_stored}`
  const daemon = (document: object, pages: string[][]) => (call: Call) => {
    if (call.route === '/documents/doc_1') return answer({ document_id: 'doc_1', head_revision: 1, head: { revision: 1, content_sha256: stored }, ...document })
    const at = call.route === '/documents' ? 0 : Number(new URLSearchParams(call.route.split('?')[1]).get('cursor'))
    return answer({ documents: pages[at]!.map((document_id) => ({ document_id })), next_cursor: at + 1 < pages.length ? String(at + 1) : null })
  }
  const run = async (document: object = {}, pages = [['doc_0', 'doc_1']]) => checkGet(fakeClient(daemon(document, pages)).client, NOTICE, 'doc_1')

  it('passes when the document is there at revision 1, holds the source byte for byte, and is listed', async () => {
    expect(await run()).toMatchObject({ id: 'ACC-GET', sample: NOTICE.id, outcome: 'passed', problems: [] })
  })

  it('looks through every page of the list', async () => {
    const { client, calls } = fakeClient(daemon({}, [['doc_8'], ['doc_9'], ['doc_1'], ['doc_2']]))
    expect((await checkGet(client, NOTICE, 'doc_1')).outcome).toBe('passed')
    expect(calls.map((c) => c.route)).toEqual(['/documents/doc_1', '/documents', '/documents?cursor=1', '/documents?cursor=2'])
    expect((await run({}, [['doc_8'], ['doc_9']])).problems).toEqual(['the document is not in the list'])
  })

  it('fails, and does not go on for ever, on a list whose pages do not end', async () => {
    const turning = (next: unknown) => (call: Call) => (call.route === '/documents/doc_1' ? answer({ head_revision: 1, head: { content_sha256: stored } }) : answer({ documents: [], next_cursor: next }))
    const failed = async (next: unknown) => (await checkGet(fakeClient(turning(next)).client, NOTICE, 'doc_1')).problems
    expect(await failed('again')).toEqual(['the pages do not end: cursor again came twice'])
    expect(await failed(undefined)).toEqual(['next_cursor is neither a cursor nor null'])
    expect(await failed(7)).toEqual(['next_cursor is neither a cursor nor null'])
  })

  it('fails on another revision, or on content that is not the sample', async () => {
    expect((await run({ head_revision: 2 })).problems).toEqual(['head_revision is 2'])
    expect((await run({ head: { content_sha256: 'sha256:00' } })).problems).toEqual(["the stored content is sha256:00, not the sample's sha256_stored"])
    expect((await run({ head: undefined, head_revision: 3 }, [[]])).problems).toHaveLength(3)
  })
})
