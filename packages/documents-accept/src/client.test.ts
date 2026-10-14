import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { ApiError, connect } from './client'
import { ContractError } from './contract'
import { answer, document, fakeClient, job, refuse } from './test/fake'

const made: string[] = []
afterEach(() => {
  for (const dir of made.splice(0)) fs.rmSync(dir, { recursive: true, force: true })
})
function stateFile(state: unknown): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'accept-'))
  made.push(dir)
  const file = path.join(dir, 'daemon.json')
  fs.writeFileSync(file, JSON.stringify(state))
  return file
}

describe('connect', () => {
  it('takes the port and the bearer from the state file the daemon wrote', () => {
    expect(connect(stateFile({ port: 51234, token: 'abc', pid: 7 }))).toEqual({ base: 'http://127.0.0.1:51234/api/v1/documents', token: 'abc' })
  })

  it('refuses a state file without a port or a bearer', () => {
    expect(() => connect(stateFile({ port: 51234 }))).toThrow(/no port and bearer token/)
    expect(() => connect(stateFile({ port: 51234, token: '' }))).toThrow(/no port and bearer token/)
    expect(() => connect(stateFile({ port: '51234', token: 'abc' }))).toThrow(/no port and bearer token/)
    expect(() => connect(stateFile(null))).toThrow(/no port and bearer token/)
  })

  it('says nothing of what a state file it cannot read holds', () => {
    const file = stateFile({})
    fs.writeFileSync(file, '{"port": 51234, "token": "s3cr3t-bearer"')
    const error = (() => {
      try {
        connect(file)
      } catch (e) {
        return String(e)
      }
    })()
    expect(error).toContain('not a daemon state file that can be read')
    expect(error).not.toContain('s3cr3t')
  })
})

describe('Client', () => {
  it('sends the bearer, and JSON or bytes as what they are', async () => {
    const { client, calls } = fakeClient(() => answer(job()))
    expect(await client.json('GET', '/jobs/j', 'DocumentsJob')).toEqual(job())
    await client.json('POST', '/imports', 'DocumentsJob', { json: { upload_id: 'u' }, headers: { 'idempotency-key': 'k' } })
    await client.send('POST', '/uploads/u/chunks', { bytes: new Uint8Array([1, 2, 3]) })
    expect(calls.map((c) => [c.method, c.route, c.headers['content-type']])).toEqual([
      ['GET', '/jobs/j', undefined],
      ['POST', '/imports', 'application/json'],
      ['POST', '/uploads/u/chunks', 'application/octet-stream'],
    ])
    expect(calls.every((c) => c.headers.authorization === 'Bearer secret')).toBe(true)
    expect(calls[1]).toMatchObject({ json: { upload_id: 'u' }, headers: { 'idempotency-key': 'k' } })
    expect(calls[2]?.bytes).toEqual(new Uint8Array([1, 2, 3]))
    // No request may wait for ever.
    expect(calls.every((c) => c.timed)).toBe(true)
  })

  it('reads no answer that is not what the contract says it is', async () => {
    const got = async (body: unknown, schema: string) => fakeClient(() => answer(body)).client.json('GET', '/x', schema).catch((e: unknown) => e)
    expect(await got(document(), 'DocumentDetail')).toEqual(document())
    for (const broken of [{}, null, 'a job', job({ status: 'done' }), job({ job_id: 5 }), job({ status: 'succeeded' }), job({ status: 'succeeded', kind: 'import', result: { document_id: 5, revision: 1 } })]) {
      const error = await got(broken, 'DocumentsJob')
      expect(error, JSON.stringify(broken)).toBeInstanceOf(ContractError)
      expect(String(error)).toContain('the answer is not a DocumentsJob')
    }
    // A job that succeeded carries no error, and one that failed says why.
    expect(await got(job({ status: 'succeeded', result: { extraction_id: 'ext_00000000000000000000000001' }, error: { code: 'internal', message: 'x' } }), 'DocumentsJob')).toBeInstanceOf(ContractError)
    expect(await got(job({ status: 'failed' }), 'DocumentsJob')).toBeInstanceOf(ContractError)
    await expect(got(job(), 'NoSuchSchema')).resolves.toMatchObject({ message: 'the contract has no schema NoSuchSchema' })
  })

  it('reads no answer that has not the status the route gives for it: 200, unless another is said', async () => {
    const got = async (http: number, status?: number) => fakeClient(() => answer(job(), http)).client.json('GET', '/jobs/x?cursor=secret', 'DocumentsJob', {}, status).catch((e: unknown) => e)
    expect(await got(200)).toEqual(job())
    expect(await got(202, 202)).toEqual(job())
    for (const [http, status] of [[201, undefined], [202, undefined], [206, undefined], [200, 202], [201, 202]] as const) {
      const error = await got(http, status)
      expect(error, `${http} for ${status}`).toBeInstanceOf(ContractError)
      // Said without the query: a cursor is the daemon's, and may be long.
      expect(String(error)).toContain(`GET /jobs/x answered ${http}, not ${status ?? 200}`)
    }
  })

  it('throws what a route refused with: its status, its code, and whether to ask again', async () => {
    const { client } = fakeClient(() => refuse(404, 'not_found'))
    const error = await client.send('GET', '/jobs/j').catch((e: unknown) => e)
    expect(error).toBeInstanceOf(ApiError)
    expect(error).toMatchObject({ status: 404, code: 'not_found', retryable: false, message: '404 not_found: it is not_found' })
  })

  it('makes do with an answer that is not the error envelope', async () => {
    const { client } = fakeClient(() => new Response('upstream went away', { status: 502 }))
    expect(await client.send('GET', '/x').catch((e: unknown) => e)).toMatchObject({ status: 502, code: 'unknown', retryable: false, message: '502 unknown: upstream went away' })
    const odd = fakeClient(() => answer({ error: 'no' }, 500))
    expect(await odd.client.send('GET', '/x').catch((e: unknown) => e)).toMatchObject({ status: 500, code: 'unknown', retryable: false })
  })

  it('asks a retryable 503 again, the same request, until it is answered', async () => {
    const waits: number[] = []
    const { client, calls } = fakeClient((_call, all) => (all.length < 3 ? refuse(503, 'engine_unavailable', true) : answer(job())), {
      sleep: async (ms) => void waits.push(ms),
      retryMs: 250,
    })
    expect(await client.json('POST', '/documents/d/extractions', 'DocumentsJob', { json: { revision: 1 } })).toEqual(job())
    expect(calls).toHaveLength(3)
    expect(new Set(calls.map((c) => JSON.stringify(c)))).toHaveLength(1)
    expect(waits).toEqual([250, 250])
  })

  it('asks again a 503 of the kernel for a request it never passed on, though it does not say retryable', async () => {
    for (const code of ['module_overloaded', 'module_stopping', 'module_not_ready', 'module_draining', 'duplicate_request_id']) {
      const { client, calls } = fakeClient((_c, all) => (all.length < 2 ? answer({ error: { code, message: 'not yet' } }, 503) : answer({})))
      await client.send('POST', '/imports', { json: { upload_id: 'u' } })
      expect(calls, code).toHaveLength(2)
    }
  })

  it('does not ask again what is not a retryable 503', async () => {
    const once = async (response: () => Response) => {
      const { client, calls } = fakeClient(response)
      await client.send('GET', '/x').catch(() => {})
      return calls.length
    }
    expect(await once(() => refuse(503, 'storage_unavailable', false))).toBe(1)
    expect(await once(() => refuse(409, 'revision_conflict', true))).toBe(1)
    expect(await once(() => new Response('', { status: 503 }))).toBe(1)
    // The request may have reached the module: what became of it is not known.
    expect(await once(() => answer({ error: { code: 'request_abandoned', message: 'gone' } }, 503))).toBe(1)
    expect(await once(() => answer({ error: { code: 'module_not_ready', message: 'not yet' } }, 404))).toBe(1)
  })

  it('gives up after the retries it was allowed', async () => {
    const { client, calls } = fakeClient(() => refuse(503, 'engine_unavailable', true), { retries: 2 })
    expect(await client.send('GET', '/x').catch((e: unknown) => e)).toMatchObject({ status: 503, code: 'engine_unavailable' })
    expect(calls).toHaveLength(3)
  })
})
