// documentsCall runs `agent24 documents request` (documents-cli.ts): which
// operations are allowed, and how each maps to its route, is the CLI's,
// tested in rust/crates/agent24-documents-client (ops). The bridge itself is
// tested through importFile in documents-import.test.ts.

import { EventEmitter } from 'node:events'
import { PassThrough } from 'node:stream'
import type { ChildProcess } from 'node:child_process'
import { describe, expect, it } from 'vitest'
import { documentsCall, documentsEvent } from './documents'
import type { Spawner } from './documents-cli'

const DOC = 'doc_01K74Z3QJ8V5N2W9RTX6YB4MCD'
const JOB = 'job_01K75A0B1C2D3E4F5G6H7J8K9M'
const EP = { port: 4321, token: 'tok-secret' }

/** A CLI that answers one result line once its stdin is read. */
function fakeCli(result: object) {
  const calls: { bin: string; args: string[]; stdin: string }[] = []
  const spawn: Spawner = (bin, args) => {
    const call = { bin, args, stdin: '' }
    calls.push(call)
    const child = new EventEmitter() as ChildProcess & EventEmitter
    const stdin = new PassThrough()
    const stdout = new PassThrough()
    Object.assign(child, { stdin, stdout, stderr: new PassThrough(), kill: () => true })
    stdin.on('data', (c: Buffer) => (call.stdin += c.toString()))
    stdin.on('end', () => {
      stdout.end(`${JSON.stringify({ type: 'result', ...result })}\n`)
      stdout.on('end', () => setImmediate(() => child.emit('close', 0)))
    })
    return child
  }
  return { spawn, calls }
}

describe('documentsCall', () => {
  it('runs `agent24 documents request` with the endpoint, token and request on stdin', async () => {
    const data = { documents: [], next_cursor: null }
    const cli = fakeCli({ ok: true, status: 200, data })
    const req = { op: 'list', cursor: 'ab12', limit: 20 }
    expect(await documentsCall(req, () => EP, cli.spawn, '/app/agent24')).toEqual({ ok: true, status: 200, data })
    expect(cli.calls.map(({ bin, args }) => ({ bin, args }))).toEqual([
      { bin: '/app/agent24', args: ['documents', 'request', '--host-stdin', '--json'] },
    ])
    expect(JSON.parse(cli.calls[0]!.stdin)).toEqual({ base: 'http://127.0.0.1:4321', token: 'tok-secret', request: req })
  })

  it("gives the CLI's answer as it is, a refusal included", async () => {
    const error = { code: 'invalid_request', message: 'not an allowed documents operation' }
    const cli = fakeCli({ ok: false, status: 400, error })
    const res = await documentsCall({ op: 'get', documentId: '../../runs' }, () => EP, cli.spawn, 'agent24')
    expect(res).toEqual({ ok: false, status: 400, error })
    const job = { job_id: JOB, kind: 'import', status: 'cancelling' }
    const cancel = fakeCli({ ok: true, status: 202, data: job })
    expect(await documentsCall({ op: 'cancelJob', jobId: JOB }, () => EP, cancel.spawn, 'agent24')).toEqual({
      ok: true,
      status: 202,
      data: job,
    })
  })

  it('no daemon yet is backend_not_ready, and the CLI is not run', async () => {
    const cli = fakeCli({ ok: true, status: 200, data: {} })
    const res = await documentsCall({ op: 'get', documentId: DOC }, () => null, cli.spawn, 'agent24')
    expect(res).toEqual({ ok: false, status: 503, error: { code: 'backend_not_ready', message: 'Backend daemon has not announced readiness yet' } })
    expect(cli.calls).toEqual([])
  })

  it('a request that is not JSON is refused in main, and the CLI is not run', async () => {
    const cli = fakeCli({ ok: true, status: 200, data: {} })
    for (const req of [undefined, { op: 'list', limit: 1n }, () => 'list']) {
      const res = await documentsCall(req, () => EP, cli.spawn, 'agent24')
      expect(res).toEqual({ ok: false, status: 400, error: { code: 'invalid_request', message: 'not an allowed documents operation' } })
    }
    expect(cli.calls).toEqual([])
  })
})

describe('documentsEvent', () => {
  const ev = (module: string, kind: string, payload: Record<string, unknown>) => ({ module, kind, payload })

  it('passes the Documenting OS\'s known events on', () => {
    for (const [kind, payload] of [
      ['job.finished', { job_id: JOB, status: 'succeeded' }],
      ['job.progress', { job_id: JOB }],
      ['document.imported', { document_id: DOC, job_id: JOB, revision: 1 }],
      ['revision.committed', { document_id: DOC, revision: 2 }],
    ] as const) {
      expect(documentsEvent(ev('documents', kind, payload))).toEqual({ kind, payload })
    }
  })

  it('drops another module\'s event, an unknown kind, and one without a well-formed id', () => {
    expect(documentsEvent(ev('agentear', 'job.finished', { job_id: JOB }))).toBeNull()
    expect(documentsEvent(ev('documents', 'document.deleted', { document_id: DOC }))).toBeNull()
    expect(documentsEvent(ev('documents', 'job.finished', { job_id: 'job_x' }))).toBeNull()
    expect(documentsEvent(ev('documents', 'job.finished', { document_id: DOC }))).toBeNull()
    expect(documentsEvent(ev('documents', 'document.imported', { job_id: JOB }))).toBeNull()
  })
})
