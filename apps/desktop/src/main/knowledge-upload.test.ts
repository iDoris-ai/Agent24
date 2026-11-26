// L7-K2 A3: upload md / markdown / txt into the default knowledge base.

import { createHash } from 'node:crypto'
import { describe, expect, it } from 'vitest'
import { knowledgeUploads } from './knowledge-upload'
import type { KnowledgeUpload } from '../shared/ipc-types'

const ORIGIN = 'https://canary-upload.example'
const CREDENTIAL = 'CANARY_CREDENTIAL_ZZ'
const KB = 'kb-default'
const KEYS = [
  'costFinal',
  'costUnknown',
  'costZero',
  'enrichment',
  'fileName',
  'jobId',
  'originalRetained',
  'reason',
  'state',
  'updatedAt',
  'uploadId',
]

interface FileRec {
  bytes: Buffer
  size?: number
  isFile?: boolean
}
interface Call {
  url: string
  init: RequestInit
  json: Record<string, unknown>
}
type Timer = { fn: () => void; ms: number }
type Step =
  | { throw: true }
  | { hang: true }
  | { status: number; body?: unknown | ((req: Record<string, unknown>) => unknown) }

function job(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    job_id: 'job-1',
    op_class: 'ingest',
    status: 'saved',
    enrichment: 'none',
    knowledge_base_id: KB,
    knowledge_id: 'know-1',
    source_revision: '1',
    index_build_id: 'idx-1',
    attempt: 1,
    failure_reason: null,
    cancel_reason: null,
    cost: { final: false, items: [] },
    saved_at: '2026-11-24T00:00:00Z',
    processing_at: null,
    indexed_at: null,
    terminal_at: null,
    original_retained: true,
    partial_artifacts: 'retained_invisible',
    ...over,
  }
}

function item(measured: string, units: number | null): Record<string, unknown> {
  return { kind: 'embed', destination: 'index', units, unit_type: 'token', measured }
}

function saved(over: Record<string, unknown> = {}): (req: Record<string, unknown>) => unknown {
  return (req) => ({ request_id: req.request_id, job: job(over) })
}

function stale(version: string): unknown {
  return {
    error: {
      code: 'stale',
      message: 'policy changed',
      details: {
        reason: 'policy_changed',
        retryable: true,
        request_id: 'server',
        current_policy_version: version,
      },
    },
  }
}

function failure(code: string, reason: string | null): unknown {
  return {
    error: {
      code,
      message: 'nope',
      details: {
        ...(reason === null ? {} : { reason }),
        retryable: false,
        request_id: 'server',
        generation: 1,
      },
    },
  }
}

function header(init: RequestInit, name: string): string | null {
  const headers = init.headers
  if (!headers || headers instanceof Headers || Array.isArray(headers)) return null
  return headers[name] ?? null
}

function kindOf(url: string): 'ingest' | 'get' | 'cancel' | null {
  const pathname = new URL(url).pathname
  if (pathname.endsWith('/ingest')) return 'ingest'
  if (pathname.endsWith('/jobs/get')) return 'get'
  if (pathname.endsWith('/jobs/cancel')) return 'cancel'
  return null
}

async function settle(): Promise<void> {
  for (let i = 0; i < 8; i++) await new Promise((resolve) => setImmediate(resolve))
}

function harness(): {
  files: Map<string, FileRec>
  calls: Call[]
  seen: KnowledgeUpload[][]
  reads: string[]
  timers: Timer[]
  queue: { ingest: Step[]; get: Step[]; cancel: Step[] }
  connected: { value: boolean }
  binding: { value: string }
  local: { value: boolean }
  credential: { value: string | null }
  kb: { value: string | null }
  hold: { current: Promise<void> | null }
  uploads: ReturnType<typeof knowledgeUploads>
  firePoll: () => Promise<void>
} {
  const files = new Map<string, FileRec>()
  const calls: Call[] = []
  const seen: KnowledgeUpload[][] = []
  const reads: string[] = []
  const timers: Timer[] = []
  const queue = { ingest: [] as Step[], get: [] as Step[], cancel: [] as Step[] }
  const connected = { value: true }
  const binding = { value: 'default#1' }
  const local = { value: false }
  const credential = { value: CREDENTIAL as string | null }
  const kb = { value: KB as string | null }
  const hold = { current: null as Promise<void> | null }
  const fetch = async (url: string, init: RequestInit): Promise<Response> => {
    const json = JSON.parse(String(init.body)) as Record<string, unknown>
    calls.push({ url: String(url), init, json })
    const kind = kindOf(String(url))
    if (!kind) throw new Error(`unexpected ${String(url)}`)
    const step = queue[kind].shift()
    if (!step) throw new Error(`no scripted ${kind}`)
    if ('throw' in step) throw new Error('network')
    if ('hang' in step) {
      return await new Promise<Response>((_resolve, reject) => {
        const signal = init.signal
        if (!signal) {
          reject(new Error('no signal'))
          return
        }
        const fail = (): void => reject(new Error('aborted'))
        if (signal.aborted) fail()
        else signal.addEventListener('abort', fail, { once: true })
      })
    }
    if (hold.current && kind === 'ingest') {
      const waiting = hold.current
      hold.current = null
      await waiting
    }
    const payload = typeof step.body === 'function' ? step.body(json) : step.body
    return new Response(payload === undefined ? '' : JSON.stringify(payload), {
      status: step.status,
    })
  }
  const uploads = knowledgeUploads({
    provider: () =>
      connected.value
        ? {
            binding: binding.value,
            local: local.value,
            origin: ORIGIN,
            credential: () => credential.value,
            fetch,
            newRequestId: () => `req-${calls.length + 1}`,
          }
        : null,
    knowledgeBaseId: () => kb.value,
    readFile: async (filePath) => {
      const file = files.get(filePath)
      if (!file) throw new Error('missing')
      reads.push(filePath)
      return file.bytes
    },
    stat: async (filePath) => {
      const file = files.get(filePath)
      if (!file) throw new Error('missing')
      return { size: file.size ?? file.bytes.length, isFile: file.isFile !== false }
    },
    setTimer: (fn, ms) => {
      const timer = { fn, ms }
      timers.push(timer)
      return timer
    },
    clearTimer: (timer) => {
      const index = timers.indexOf(timer as Timer)
      if (index >= 0) timers.splice(index, 1)
    },
    onChange: (list) => {
      seen.push(list)
    },
  })
  return {
    files,
    calls,
    seen,
    reads,
    timers,
    queue,
    connected,
    binding,
    local,
    credential,
    kb,
    hold,
    uploads,
    firePoll: async () => {
      const index = timers.findIndex((timer) => timer.ms === 2_000)
      if (index < 0) throw new Error('no poll timer')
      const [timer] = timers.splice(index, 1)
      timer.fn()
      await settle()
    },
  }
}

function put(h: ReturnType<typeof harness>, filePath: string, text = 'hello'): void {
  h.files.set(filePath, { bytes: Buffer.from(text) })
}

describe('knowledge upload (L7-K2 A3)', () => {
  it('refuses pdf, docx and extensionless names before any request', async () => {
    const h = harness()
    for (const name of ['notes.pdf', 'notes.docx', 'README', 'notes.PDF']) {
      const [upload] = await h.uploads.add([`/tmp/${name}`])
      expect(upload).toMatchObject({
        state: 'refused',
        reason: 'unsupported_format',
        fileName: name,
        originalRetained: false,
        jobId: null,
      })
    }
    expect(h.calls).toEqual([])
    expect(h.reads).toEqual([])
  })

  it('Loop 9 3c: the local Lite also takes csv and json, and says plainly it does not take pdf or Word', async () => {
    const h = harness()
    h.local.value = true
    for (const name of ['notes.pdf', 'notes.docx', 'notes.doc']) {
      const [upload] = await h.uploads.add([`/tmp/${name}`])
      expect(upload).toMatchObject({ state: 'refused', reason: 'unsupported_format_local', fileName: name })
    }
    h.kb.value = null
    for (const name of ['a.csv', 'b.JSON', 'c.md', 'd.txt']) {
      put(h, `/tmp/${name}`)
      const [upload] = await h.uploads.add([`/tmp/${name}`])
      expect(upload).toMatchObject({ state: 'refused', reason: 'not_connected', fileName: name })
    }
    // The remote server keeps its own list.
    h.local.value = false
    const [remote] = await h.uploads.add(['/tmp/a.csv'])
    expect(remote).toMatchObject({ state: 'refused', reason: 'unsupported_format' })
    expect(h.calls).toEqual([])
  })

  it('FU-235: after a switch of binding, an upload is no longer asked about with the new key, and says so', async () => {
    const h = harness()
    put(h, '/tmp/a.md')
    put(h, '/tmp/b.md')
    h.queue.ingest.push({ status: 200, body: saved({ status: 'saved', enrichment: 'none' }) })
    h.queue.ingest.push({ status: 200, body: saved({ status: 'saved', enrichment: 'none', job_id: 'job-2' }) })
    await h.uploads.add(['/tmp/a.md'])
    await h.uploads.add(['/tmp/b.md'])
    const [first] = h.uploads.list()
    // Switched (say, remote to the local Lite): another origin and key behind the same slot name.
    h.binding.value = 'default#2'
    h.credential.value = 'OTHER_KEY'
    await h.firePoll()
    expect(h.uploads.list().map((u) => [u.state, u.reason])).toEqual([['unknown', 'rebound'], ['unknown', 'rebound']])
    expect(h.calls.filter((c) => !c.url.endsWith('/ingest'))).toEqual([])
    expect(h.timers.some((timer) => timer.ms === 2_000)).toBe(false)
    // Cancelling it does not reach the new server either.
    expect(await h.uploads.cancel(first.uploadId)).toMatchObject({ state: 'unknown', reason: 'rebound' })
    expect(h.calls.filter((c) => !c.url.endsWith('/ingest'))).toEqual([])
  })

  it('accepts md, markdown and txt regardless of case', async () => {
    const h = harness()
    h.connected.value = false
    for (const name of ['A.MD', 'B.MARKDOWN', 'C.Txt', 'notes.bak.md']) {
      const filePath = `/tmp/${name}`
      put(h, filePath)
      const [upload] = await h.uploads.add([filePath])
      expect(upload).toMatchObject({ state: 'refused', reason: 'not_connected', fileName: name })
    }
    expect(h.calls).toEqual([])
    expect(h.reads).toEqual([])
  })

  it('refuses a non-file and a missing path', async () => {
    const h = harness()
    h.files.set('/tmp/dir.md', { bytes: Buffer.alloc(0), isFile: false })
    expect((await h.uploads.add(['/tmp/dir.md']))[0]).toMatchObject({
      state: 'refused',
      reason: 'not_a_file',
    })
    expect((await h.uploads.add(['/tmp/missing.md']))[0]).toMatchObject({
      state: 'refused',
      reason: 'not_a_file',
    })
    expect(h.calls).toEqual([])
    expect(h.reads).toEqual([])
  })

  it('refuses anything over 10 MiB before reading it, and allows exactly 10 MiB', async () => {
    const h = harness()
    for (const size of [10_485_761, 11 * 1024 * 1024]) {
      const filePath = `/tmp/big-${size}.md`
      h.files.set(filePath, { bytes: Buffer.alloc(0), size })
      const [upload] = await h.uploads.add([filePath])
      expect(upload).toMatchObject({
        state: 'refused',
        reason: 'too_large',
        fileName: `big-${size}.md`,
      })
    }
    expect(h.reads).toEqual([])
    expect(h.calls).toEqual([])
    const exact = '/tmp/exact.md'
    h.files.set(exact, { bytes: Buffer.from('x'), size: 10_485_760 })
    h.queue.ingest.push({ status: 200, body: saved() })
    expect((await h.uploads.add([exact]))[0].state).toBe('saved')
    expect(h.reads).toEqual([exact])
  })

  it('refuses when the binding, the credential or the knowledge base is missing', async () => {
    const h = harness()
    put(h, '/tmp/a.md')
    h.connected.value = false
    expect((await h.uploads.add(['/tmp/a.md']))[0]).toMatchObject({
      state: 'refused',
      reason: 'not_connected',
    })
    h.connected.value = true
    h.credential.value = null
    expect((await h.uploads.add(['/tmp/a.md']))[0]).toMatchObject({
      state: 'refused',
      reason: 'not_connected',
    })
    h.credential.value = CREDENTIAL
    h.kb.value = null
    expect((await h.uploads.add(['/tmp/a.md']))[0]).toMatchObject({
      state: 'refused',
      reason: 'not_connected',
    })
    h.kb.value = ''
    expect((await h.uploads.add(['/tmp/a.md']))[0]).toMatchObject({
      state: 'refused',
      reason: 'not_connected',
    })
    expect(h.calls).toEqual([])
    expect(h.reads).toEqual([])
  })

  it('uploads markdown, then polls until indexed and stops', async () => {
    const h = harness()
    const filePath = '/tmp/docs/notes.md'
    const text = 'hello knowledge'
    put(h, filePath, text)
    h.queue.ingest.push({ status: 200, body: saved({ status: 'saved', enrichment: 'none' }) })
    h.queue.get.push({ status: 200, body: saved({ status: 'processing', enrichment: 'none' }) })
    h.queue.get.push({ status: 200, body: saved({ status: 'indexed', enrichment: 'none', cost: { final: true, items: [item('exact', 0)] } }) })
    const [upload] = await h.uploads.add([filePath])
    const body = h.calls[0].json
    expect(h.calls[0].url).toBe(`${ORIGIN}/api/v1/agent24/knowledge/ingest`)
    expect(header(h.calls[0].init, 'X-API-Key')).toBe(CREDENTIAL)
    expect(h.calls[0].init.redirect).toBe('error')
    expect(h.calls[0].init.cache).toBe('no-store')
    expect(body.file_name).toBe('notes.md')
    expect(body.knowledge_base_id).toBe(KB)
    expect(body.content_sha256).toBe(createHash('sha256').update(Buffer.from(text)).digest('hex'))
    expect(body.content_base64).toBe(Buffer.from(text).toString('base64'))
    expect(body.expected_policy_version).toBe('unknown')
    expect(body.idempotency_key).toMatch(/^[A-Za-z0-9_-]{22,128}$/)
    expect(Object.keys(body).sort()).toEqual([
      'content_base64',
      'content_sha256',
      'expected_policy_version',
      'file_name',
      'idempotency_key',
      'knowledge_base_id',
      'request_id',
    ])
    expect(upload).toMatchObject({
      state: 'saved',
      fileName: 'notes.md',
      jobId: 'job-1',
      originalRetained: true,
    })
    await h.firePoll()
    expect(h.uploads.list()[0]).toMatchObject({ state: 'processing', enrichment: 'none' })
    expect(h.calls[1].url).toBe(`${ORIGIN}/api/v1/agent24/knowledge/jobs/get`)
    expect(h.calls[1].json).toMatchObject({ job_id: 'job-1' })
    expect(header(h.calls[1].init, 'X-API-Key')).toBe(CREDENTIAL)
    await h.firePoll()
    expect(h.uploads.list()[0]).toMatchObject({
      state: 'indexed',
      enrichment: 'none',
      originalRetained: true,
    })
    expect(h.calls.filter((call) => call.url.endsWith('/jobs/get'))).toHaveLength(2)
    expect(h.timers.some((timer) => timer.ms === 2_000)).toBe(false)
  })

  it('keeps polling an indexed upload while enrichment is running', async () => {
    const h = harness()
    put(h, '/tmp/docs/notes.md')
    h.queue.ingest.push({ status: 200, body: saved({ status: 'indexed', enrichment: 'running' }) })
    h.queue.get.push({ status: 200, body: saved({ status: 'indexed', enrichment: 'none', cost: { final: true, items: [item('exact', 0)] } }) })
    expect((await h.uploads.add(['/tmp/docs/notes.md']))[0].enrichment).toBe('running')
    expect(h.timers.some((timer) => timer.ms === 2_000)).toBe(true)
    await h.firePoll()
    expect(h.uploads.list()[0]).toMatchObject({ state: 'indexed', enrichment: 'none' })
    expect(h.timers.some((timer) => timer.ms === 2_000)).toBe(false)
  })

  it('adopts a changed policy and resends once with the same idempotency key', async () => {
    const h = harness()
    put(h, '/tmp/docs/notes.md')
    h.queue.ingest.push({ status: 409, body: stale('pol-v2') })
    h.queue.ingest.push({ status: 200, body: saved() })
    const [upload] = await h.uploads.add(['/tmp/docs/notes.md'])
    expect(upload.state).toBe('saved')
    expect(h.calls).toHaveLength(2)
    expect(h.calls[0].json.idempotency_key).toBe(h.calls[1].json.idempotency_key)
    expect(h.calls[0].json.idempotency_key).toMatch(/^[A-Za-z0-9_-]{22,128}$/)
    expect(h.calls[0].json.expected_policy_version).toBe('unknown')
    expect(h.calls[1].json.expected_policy_version).toBe('pol-v2')
    expect(h.calls[0].json.request_id).not.toBe(h.calls[1].json.request_id)
  })

  it('does not resend a second policy change, and the next file uses the new version', async () => {
    const h = harness()
    put(h, '/tmp/docs/a.md', 'a')
    put(h, '/tmp/docs/b.md', 'b')
    h.queue.ingest.push({ status: 409, body: stale('pol-v2') })
    h.queue.ingest.push({ status: 409, body: stale('pol-v3') })
    const [first] = await h.uploads.add(['/tmp/docs/a.md'])
    expect(first).toMatchObject({ state: 'refused', reason: 'policy_changed' })
    expect(h.calls).toHaveLength(2)
    expect(h.calls[0].json.idempotency_key).toBe(h.calls[1].json.idempotency_key)
    expect(h.calls[1].json.expected_policy_version).toBe('pol-v2')
    h.queue.ingest.push({ status: 200, body: saved() })
    expect((await h.uploads.add(['/tmp/docs/b.md']))[0].state).toBe('saved')
    expect(h.calls[2].json.expected_policy_version).toBe('pol-v3')
    expect(h.calls[2].json.idempotency_key).not.toBe(h.calls[0].json.idempotency_key)
  })

  it('reports a network error as unknown, because the upload may have landed', async () => {
    const h = harness()
    put(h, '/tmp/docs/notes.md')
    h.queue.ingest.push({ throw: true })
    const [upload] = await h.uploads.add(['/tmp/docs/notes.md'])
    expect(upload).toMatchObject({ state: 'unknown', reason: 'unreachable', jobId: null })
    expect(upload.state).not.toBe('failed')
    expect(h.calls).toHaveLength(1)
  })

  it('a 200 whose body cannot be used is unknown, never failed: the upload may have landed', async () => {
    const h = harness()
    put(h, '/tmp/docs/a.md')
    put(h, '/tmp/docs/b.md')
    h.queue.ingest.push({ status: 200 }) // empty body: cut off after the status line
    h.queue.ingest.push({ status: 200, body: { request_id: 'someone-else', job: job() } })
    const [a, b] = await h.uploads.add(['/tmp/docs/a.md', '/tmp/docs/b.md'])
    expect(a).toMatchObject({ state: 'unknown', reason: 'unreachable' })
    expect(b).toMatchObject({ state: 'unknown', reason: 'unreachable' })
  })

  it('any 2xx is read the same way: 202 with a job is saved, 204 with no body is unknown', async () => {
    const h = harness()
    put(h, '/tmp/docs/a.md')
    put(h, '/tmp/docs/b.md')
    h.queue.ingest.push({ status: 202, body: saved() })
    h.queue.ingest.push({ status: 204 })
    const [a, b] = await h.uploads.add(['/tmp/docs/a.md', '/tmp/docs/b.md'])
    expect(a).toMatchObject({ state: 'saved' })
    expect(b).toMatchObject({ state: 'unknown', reason: 'unreachable' })
  })

  it('reports a timeout as unknown', async () => {
    const h = harness()
    put(h, '/tmp/docs/notes.md')
    h.queue.ingest.push({ hang: true })
    const pending = h.uploads.add(['/tmp/docs/notes.md'])
    await settle()
    const timeout = h.timers.find((timer) => timer.ms === 60_000)
    if (!timeout) throw new Error('ingest did not arm a timeout')
    timeout.fn()
    await settle()
    const [upload] = await pending
    expect(upload).toMatchObject({ state: 'unknown', reason: 'unreachable', jobId: null })
  })

  it('maps server refusals to refused and auth or outage to failed', async () => {
    const h = harness()
    const cases: Array<{
      name: string
      status: number
      body: unknown
      state: string
      reason: string
    }> = [
      {
        name: 'fmt.md',
        status: 400,
        body: failure('invalid_request', 'unsupported_format'),
        state: 'refused',
        reason: 'unsupported_format',
      },
      {
        name: 'big.md',
        status: 400,
        body: failure('invalid_request', 'too_large'),
        state: 'refused',
        reason: 'too_large',
      },
      {
        name: 'hash.md',
        status: 400,
        body: failure('invalid_request', 'content_hash_mismatch'),
        state: 'refused',
        reason: 'content_hash_mismatch',
      },
      {
        name: 'key.md',
        status: 400,
        body: failure('invalid_request', 'idempotency_key_reused'),
        state: 'refused',
        reason: 'idempotency_key_reused',
      },
      {
        name: 'scope.md',
        status: 403,
        body: failure('forbidden', 'empty_scope'),
        state: 'refused',
        reason: 'empty_scope',
      },
      {
        name: 'op.md',
        status: 403,
        body: failure('forbidden', 'operation_not_allowed'),
        state: 'refused',
        reason: 'operation_not_allowed',
      },
      {
        name: 'auth.md',
        status: 401,
        body: failure('unauthorized', null),
        state: 'failed',
        reason: 'unauthorized',
      },
      {
        name: 'down.md',
        status: 503,
        body: failure('unavailable', 'request_in_progress'),
        state: 'failed',
        reason: 'unavailable',
      },
      {
        name: 'boom.md',
        status: 500,
        body: failure('internal', null),
        state: 'failed',
        reason: 'internal',
      },
    ]
    for (const entry of cases) {
      const filePath = `/tmp/${entry.name}`
      put(h, filePath)
      h.queue.ingest.push({ status: entry.status, body: entry.body })
      const [upload] = await h.uploads.add([filePath])
      expect(upload).toMatchObject({ state: entry.state, reason: entry.reason, jobId: null })
    }
  })

  it('leaves the state alone when a poll fails, and a 404 becomes not_found', async () => {
    const h = harness()
    put(h, '/tmp/docs/notes.md')
    h.queue.ingest.push({ status: 200, body: saved() })
    h.queue.get.push({ throw: true })
    h.queue.get.push({ status: 404, body: failure('not_found', 'not_found') })
    expect((await h.uploads.add(['/tmp/docs/notes.md']))[0].state).toBe('saved')
    await h.firePoll()
    expect(h.uploads.list()[0].state).toBe('saved')
    expect(h.timers.some((timer) => timer.ms === 2_000)).toBe(true)
    await h.firePoll()
    expect(h.uploads.list()[0]).toMatchObject({
      state: 'unknown',
      reason: 'not_found',
      jobId: 'job-1',
    })
    expect(h.timers.some((timer) => timer.ms === 2_000)).toBe(false)
  })

  it('keeps polling a cancelled job until its cost is final (W5.10), then stops', async () => {
    const h = harness()
    put(h, '/tmp/docs/a.md', 'a')
    h.queue.ingest.push({ status: 200, body: saved({ job_id: 'job-a' }) })
    const [added] = await h.uploads.add(['/tmp/docs/a.md'])
    h.queue.cancel.push({
      status: 200,
      body: (req) => ({
        request_id: req.request_id, cancel_effect: 'cancelled',
        job: job({ job_id: 'job-a', status: 'cancelled', cost: { final: false, items: [item('unknown', null)] } }),
      }),
    })
    expect(await h.uploads.cancel(added.uploadId)).toMatchObject({ state: 'cancelled', costFinal: false })
    h.queue.get.push({
      status: 200,
      body: saved({ job_id: 'job-a', status: 'cancelled', cost: { final: true, items: [item('exact', 120)] } }),
    })
    await h.firePoll()
    expect(h.uploads.list()[0]).toMatchObject({ state: 'cancelled', costFinal: true, costZero: false })
    expect(h.timers.some((timer) => timer.ms === 2_000)).toBe(false)
  })

  it('stops waiting for a final cost after 300 polls, and on dispose', async () => {
    const h = harness()
    put(h, '/tmp/docs/a.md', 'a')
    h.queue.ingest.push({ status: 200, body: saved({ job_id: 'job-a', status: 'cancelled', cost: { final: false, items: [] } }) })
    await h.uploads.add(['/tmp/docs/a.md'])
    for (let i = 0; i < 300; i++) {
      h.queue.get.push({ status: 200, body: saved({ job_id: 'job-a', status: 'cancelled', cost: { final: false, items: [] } }) })
      await h.firePoll()
    }
    expect(h.timers.some((timer) => timer.ms === 2_000)).toBe(false)
    expect(h.uploads.list()[0]).toMatchObject({ state: 'cancelled', costFinal: false })

    const d = harness()
    put(d, '/tmp/docs/b.md', 'b')
    d.queue.ingest.push({ status: 200, body: saved({ job_id: 'job-b', status: 'cancelled', cost: { final: false, items: [] } }) })
    await d.uploads.add(['/tmp/docs/b.md'])
    expect(d.timers.some((timer) => timer.ms === 2_000)).toBe(true)
    d.uploads.dispose()
    expect(d.timers.some((timer) => timer.ms === 2_000)).toBe(false)
  })

  it('polls spent while enrichment runs do not use up the wait for the final cost', async () => {
    const h = harness()
    put(h, '/tmp/docs/a.md', 'a')
    h.queue.ingest.push({ status: 200, body: saved({ job_id: 'job-a', status: 'indexed', enrichment: 'running' }) })
    await h.uploads.add(['/tmp/docs/a.md'])
    for (let i = 0; i < 300; i++) {
      h.queue.get.push({ status: 200, body: saved({ job_id: 'job-a', status: 'indexed', enrichment: 'running' }) })
      await h.firePoll()
    }
    h.queue.get.push({ status: 200, body: saved({ job_id: 'job-a', status: 'indexed', enrichment: 'none' }) })
    await h.firePoll()
    expect(h.timers.some((timer) => timer.ms === 2_000)).toBe(true)
    h.queue.get.push({
      status: 200,
      body: saved({ job_id: 'job-a', status: 'indexed', enrichment: 'none', cost: { final: true, items: [item('exact', 0)] } }),
    })
    await h.firePoll()
    expect(h.uploads.list()[0]).toMatchObject({ state: 'indexed', costFinal: true, costZero: true })
  })

  it('cancel takes the job and the cost from the answer', async () => {
    const h = harness()
    put(h, '/tmp/docs/a.md', 'a')
    put(h, '/tmp/docs/b.md', 'b')
    put(h, '/tmp/docs/c.md', 'c')
    h.queue.ingest.push({
      status: 200,
      body: saved({ job_id: 'job-a', cost: { final: false, items: [item('exact', 5)] } }),
    })
    h.queue.ingest.push({
      status: 200,
      body: saved({ job_id: 'job-b', status: 'processing', enrichment: 'running' }),
    })
    h.queue.ingest.push({
      status: 200,
      body: saved({ job_id: 'job-c', status: 'indexed', enrichment: 'running' }),
    })
    const added = await h.uploads.add(['/tmp/docs/a.md', '/tmp/docs/b.md', '/tmp/docs/c.md'])
    h.queue.cancel.push({
      status: 200,
      body: (req) => ({
        request_id: req.request_id,
        cancel_effect: 'cancelled',
        job: job({
          job_id: 'job-a',
          status: 'cancelled',
          cost: { final: true, items: [item('exact', 0)] },
        }),
      }),
    })
    const cancelled = await h.uploads.cancel(added[0].uploadId)
    expect(cancelled).toMatchObject({
      state: 'cancelled',
      reason: null,
      jobId: 'job-a',
      costFinal: true,
      costZero: true,
      costUnknown: false,
      originalRetained: true,
    })
    const cancelCall = h.calls.find((call) => call.url.endsWith('/jobs/cancel'))
    expect(cancelCall?.url).toBe(`${ORIGIN}/api/v1/agent24/knowledge/jobs/cancel`)
    expect(cancelCall?.json.job_id).toBe('job-a')
    expect(header(cancelCall?.init ?? {}, 'X-API-Key')).toBe(CREDENTIAL)
    h.queue.cancel.push({
      status: 200,
      body: (req) => ({
        request_id: req.request_id,
        cancel_effect: 'none',
        job: job({
          job_id: 'job-b',
          status: 'processing',
          enrichment: 'running',
          cost: { final: true, items: [item('unknown', null)] },
        }),
      }),
    })
    expect(await h.uploads.cancel(added[1].uploadId)).toMatchObject({
      state: 'processing',
      reason: 'none',
      costZero: false,
      costUnknown: true,
      costFinal: true,
    })
    h.queue.cancel.push({
      status: 200,
      body: (req) => ({
        request_id: req.request_id,
        cancel_effect: 'enrichment_stopped',
        job: job({ job_id: 'job-c', status: 'indexed', enrichment: 'stopped' }),
      }),
    })
    expect(await h.uploads.cancel(added[2].uploadId)).toMatchObject({
      state: 'indexed',
      enrichment: 'stopped',
      reason: 'enrichment_stopped',
    })
    const [refused] = await h.uploads.add(['/tmp/notes.pdf'])
    expect(await h.uploads.cancel(refused.uploadId)).toMatchObject({
      state: 'refused',
      jobId: null,
    })
    expect(await h.uploads.cancel('missing')).toBeNull()
    expect(h.calls.filter((call) => call.url.endsWith('/jobs/cancel'))).toHaveLength(3)
  })

  it('sets costZero only for a final cost whose items are all exact zeroes', async () => {
    const h = harness()
    const cases = [
      {
        name: 'zero.md',
        cost: { final: true, items: [item('exact', 0)] },
        costFinal: true,
        costZero: true,
        costUnknown: false,
      },
      {
        name: 'empty.md',
        cost: { final: true, items: [] },
        costFinal: true,
        costZero: false,
        costUnknown: false,
      },
      {
        name: 'open.md',
        cost: { final: false, items: [item('exact', 0)] },
        costFinal: false,
        costZero: false,
        costUnknown: false,
      },
      {
        name: 'mixed.md',
        cost: { final: true, items: [item('exact', 0), item('exact', 3)] },
        costFinal: true,
        costZero: false,
        costUnknown: false,
      },
      {
        name: 'est.md',
        cost: { final: true, items: [item('estimated', 0)] },
        costFinal: true,
        costZero: false,
        costUnknown: false,
      },
      {
        name: 'null.md',
        cost: { final: true, items: [item('exact', null)] },
        costFinal: true,
        costZero: false,
        costUnknown: false,
      },
      {
        name: 'unk.md',
        cost: { final: true, items: [item('unknown', null)] },
        costFinal: true,
        costZero: false,
        costUnknown: true,
      },
      {
        name: 'both.md',
        cost: { final: true, items: [item('exact', 0), item('unknown', null)] },
        costFinal: true,
        costZero: false,
        costUnknown: true,
      },
    ]
    for (const entry of cases) {
      const filePath = `/tmp/${entry.name}`
      put(h, filePath)
      h.queue.ingest.push({ status: 200, body: saved({ cost: entry.cost }) })
      const [upload] = await h.uploads.add([filePath])
      expect(upload).toMatchObject({
        costFinal: entry.costFinal,
        costZero: entry.costZero,
        costUnknown: entry.costUnknown,
      })
    }
  })

  it('uploads one file at a time', async () => {
    const h = harness()
    put(h, '/tmp/docs/a.md', 'a')
    put(h, '/tmp/docs/b.md', 'b')
    let release: (() => void) | null = null
    h.hold.current = new Promise((resolve) => {
      release = resolve
    })
    h.queue.ingest.push({ status: 200, body: saved({ job_id: 'job-a' }) })
    h.queue.ingest.push({ status: 200, body: saved({ job_id: 'job-b' }) })
    const pending = h.uploads.add(['/tmp/docs/a.md', '/tmp/docs/b.md'])
    await settle()
    expect(h.calls.filter((call) => call.url.endsWith('/ingest'))).toHaveLength(1)
    if (!release) throw new Error('hold was not armed')
    release()
    const added = await pending
    expect(h.calls.filter((call) => call.url.endsWith('/ingest'))).toHaveLength(2)
    expect(added.map((upload) => upload.fileName)).toEqual(['a.md', 'b.md'])
    expect(added.map((upload) => upload.jobId)).toEqual(['job-a', 'job-b'])
  })

  it('keeps at most 50 uploads, dropping the oldest finished ones', async () => {
    const h = harness()
    const paths = Array.from({ length: 51 }, (_, index) => `/tmp/batch/f${index}.pdf`)
    await h.uploads.add(paths)
    const list = h.uploads.list()
    expect(list).toHaveLength(50)
    expect(list[0].fileName).toBe('f1.pdf')
    expect(list[49].fileName).toBe('f50.pdf')
    expect(h.calls).toEqual([])
  })

  it('dispose stops the poll timer', async () => {
    const h = harness()
    put(h, '/tmp/docs/notes.md')
    h.queue.ingest.push({ status: 200, body: saved() })
    await h.uploads.add(['/tmp/docs/notes.md'])
    expect(h.timers.some((timer) => timer.ms === 2_000)).toBe(true)
    h.uploads.dispose()
    expect(h.timers.some((timer) => timer.ms === 2_000)).toBe(false)
  })

  it('never puts the path, the credential, the URL or the bytes on the upload list', async () => {
    const h = harness()
    const content = 'CANARY_CONTENT_ZZ'
    const filePath = '/data/CANARY_PATH_ZZ/notes.md'
    put(h, filePath, content)
    h.queue.ingest.push({ status: 200, body: saved() })
    const [upload] = await h.uploads.add([filePath])
    expect(h.calls[0].url).toContain('canary-upload.example')
    expect(header(h.calls[0].init, 'X-API-Key')).toBe(CREDENTIAL)
    expect(h.calls[0].json.content_base64).toBe(Buffer.from(content).toString('base64'))
    expect(JSON.stringify(h.calls[0].json)).not.toContain('CANARY_PATH_ZZ')
    expect(Buffer.from(content).toString('base64')).not.toContain(content)
    const published = JSON.stringify({ seen: h.seen, list: h.uploads.list(), upload })
    for (const secret of [
      CREDENTIAL,
      'CANARY_PATH_ZZ',
      'canary-upload.example',
      content,
      filePath,
    ]) {
      expect(published).not.toContain(secret)
    }
    for (const batch of h.seen) {
      for (const entry of batch) expect(Object.keys(entry).sort()).toEqual([...KEYS].sort())
    }
    expect(h.uploads.list()).toEqual(h.seen.at(-1))
  })
})
