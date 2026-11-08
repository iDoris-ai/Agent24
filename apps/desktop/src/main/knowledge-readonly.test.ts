import { readFileSync } from 'node:fs'
import path from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  READ_MAX_PAGES,
  READ_MAX_RESPONSE_BYTES,
  READ_MAX_TEXT_BYTES,
  READ_TIMEOUT_MS,
  plainTitle,
  readCitationChunk,
  type CitationRef,
  type ReadonlyDeps,
} from './knowledge-readonly'

const FIXTURES = path.join(__dirname, '../../../../protocol/knowledge/fixtures/v1')
const fixture = (name: string): { response: { status: number; body: unknown } } =>
  JSON.parse(readFileSync(path.join(FIXTURES, name), 'utf8'))

const ORIGIN = 'https://kb.example'
const REF: CitationRef = {
  knowledge_base_id: 'kb-c',
  knowledge_id: 'doc-7',
  chunk_id: 'chk-71',
  source_revision: 'src-8',
  index_build_id: 'ib-19',
  chunk_revision: 3,
  content_tag: 'ct-chk-71',
}

function ids(): () => string {
  let n = 0
  return () => `01J9ZQ3V6R8XKQ4M2N5P7T${String(n++).padStart(4, '0')}`.slice(-26)
}

interface Call { url: string; init: RequestInit; body: Record<string, any> }

/** An error envelope echoing this call (R6.1); fixtures carry their own request_id. */
function echo(body: any, call: Call): unknown {
  return { error: { ...body.error, details: { ...body.error.details, request_id: call.body.request_id, generation: call.body.generation } } }
}

function client(handler: (call: Call, index: number) => { status: number; json: () => Promise<unknown> } | 'throw' | 'hang'): { deps: ReadonlyDeps; calls: Call[] } {
  const calls: Call[] = []
  const fetchImpl: typeof fetch = (async (url: string, init?: RequestInit) => {
    const call: Call = { url: String(url), init: init ?? {}, body: JSON.parse(String(init?.body ?? '{}')) }
    calls.push(call)
    const result = handler(call, calls.length - 1)
    if (result === 'throw') throw new TypeError('network')
    if (result === 'hang') {
      return await new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener('abort', () => reject(new Error('aborted')))
      })
    }
    // A real Response: the client reads the body as a stream, under its timeout and cap.
    let text: string
    try { text = JSON.stringify(await result.json()) } catch { text = '{not json' }
    return new Response(text, { status: result.status })
  }) as typeof fetch
  return { deps: { origin: ORIGIN, credential: () => 'C-secret', fetch: fetchImpl, newRequestId: ids() }, calls }
}

function okPage(req: Record<string, any>, over: { text?: string; title?: unknown; truncated?: boolean; cursor?: string; contentTag?: string; segments?: unknown; chunkId?: string; chunkType?: string; knowledgeId?: string; scope?: unknown; requestId?: string; generation?: number; textValue?: unknown } = {}): { status: number; json: () => Promise<unknown> } {
  const truncated = over.truncated ?? false
  const body: Record<string, unknown> = {
    protocol_version: 1,
    protocol_version: 1,
    request_id: over.requestId ?? req.request_id,
    generation: over.generation ?? req.generation,
    effective_scope: over.scope ?? { knowledge_base_ids: [REF.knowledge_base_id] },
    knowledge_id: over.knowledgeId ?? req.knowledge_id,
    segments: over.segments ?? [{
      chunk_id: over.chunkId ?? req.chunk_id,
      chunk_type: over.chunkType ?? 'text',
      chunk_revision: 0,
      content_tag: over.contentTag ?? REF.content_tag,
      text: over.textValue !== undefined ? over.textValue : (over.text ?? 'hello'),
    }],
    truncated,
  }
  if (over.title !== undefined) body.display = { title: over.title }
  if (truncated) body.next_cursor = over.cursor ?? 'cur-71-1'
  return { status: 200, json: async () => body }
}

const read = (deps: ReadonlyDeps, ref: CitationRef = REF, signal = new AbortController().signal) =>
  readCitationChunk(deps, 15, ref, signal)

describe('readCitationChunk', () => {
  it('sends the brief body field by field and joins the fixture page', async () => {
    const sample = fixture('read.chunk.json')
    const { deps, calls } = client((call, i) => {
      if (i === 0) {
        const body = structuredClone(sample.response.body) as Record<string, any>
        body.request_id = call.body.request_id
        body.generation = call.body.generation
        body.display = { title: '  Cited \u202e title  ' }
        return { status: 200, json: async () => body }
      }
      return okPage(call.body, { text: ' tail', title: 'ignored', truncated: false })
    })
    const got = await read(deps)
    expect(got).toEqual({ ok: true, text: '<redacted> tail', contentTag: REF.content_tag, title: 'Cited title' })
    expect(calls[0].url).toBe(`${ORIGIN}/api/v1/agent24/knowledge/read`)
    expect(calls[0].init.method).toBe('POST')
    expect(calls[0].init.redirect).toBe('error')
    expect(calls[0].init.headers).toEqual({ 'X-API-Key': 'C-secret', 'Content-Type': 'application/json' })
    expect(calls[0].body.protocol_version).toBe(1)
    expect(calls[0].body.request_id).toMatch(/^[0-9A-HJKMNP-TV-Z]{26}$/)
    expect(calls[0].body.generation).toBe(15)
    expect(calls[0].body.scope).toEqual({ knowledge_base_ids: ['kb-c'] })
    expect(calls[0].body.knowledge_id).toBe('doc-7')
    expect(calls[0].body.chunk_id).toBe('chk-71')
    expect(calls[0].body.expected_revision).toEqual({ content_tag: 'ct-chk-71', source_revision: 'src-8', index_build_id: 'ib-19', chunk_revision: 3 })
    expect(calls[0].body.max_bytes).toBe(65536)
    expect(calls[0].body.deadline_ms).toBe(9000)
    expect(calls[0].body.page).toBeUndefined()
    expect(calls[1].body.request_id).not.toBe(calls[0].body.request_id)
    expect(calls[1].body.page).toEqual({ cursor: 'cur-71-1' })
    expect(calls[1].body.generation).toBe(15)
    expect(calls[1].body.expected_revision).toEqual(calls[0].body.expected_revision)
  })

  it('omits chunk_revision when it is null', async () => {
    const { deps, calls } = client((call) => okPage(call.body, { text: 'one' }))
    const got = await read(deps, { ...REF, chunk_revision: null })
    expect(got).toEqual({ ok: true, text: 'one', contentTag: REF.content_tag, title: null })
    expect(calls[0].body.expected_revision).toEqual({ content_tag: 'ct-chk-71', source_revision: 'src-8', index_build_id: 'ib-19' })
    expect(calls[0].body.expected_revision).not.toHaveProperty('chunk_revision')
  })

  it('a content_tag that changes on page 2 is internal', async () => {
    const { deps, calls } = client((call, i) => i === 0
      ? okPage(call.body, { text: 'p1', truncated: true, cursor: 'c2' })
      : okPage(call.body, { text: 'p2', contentTag: 'other' }))
    expect(await read(deps)).toEqual({ ok: false, code: 'internal' })
    expect(calls).toHaveLength(2)
  })

  it.each([
    ['a second segment', (call: Call) => okPage(call.body, { segments: [{ chunk_id: call.body.chunk_id, chunk_type: 'text', content_tag: REF.content_tag, text: 'a' }, { chunk_id: 'x', chunk_type: 'text', content_tag: REF.content_tag, text: 'b' }] })],
    ['a chunk_id mismatch', (call: Call) => okPage(call.body, { chunkId: 'other' })],
    ['a request_id mismatch', (call: Call) => okPage(call.body, { requestId: '01AAAAAAAAAAAAAAAAAAAAAAAAAA' })],
    ['a generation mismatch', (call: Call) => okPage(call.body, { generation: 14 })],
    ['a knowledge_id mismatch', (call: Call) => okPage(call.body, { knowledgeId: 'other' })],
    ['a scope that misses the base', (call: Call) => okPage(call.body, { scope: { knowledge_base_ids: ['kb-other'] } })],
    ['a non-text chunk', (call: Call) => okPage(call.body, { chunkType: 'image' })],
    ['a non-string text', (call: Call) => okPage(call.body, { textValue: 1 })],
    ['truncated without a cursor', (call: Call) => ({ status: 200, json: async () => ({ request_id: call.body.request_id, generation: 15, knowledge_id: call.body.knowledge_id, effective_scope: { knowledge_base_ids: ['kb-c'] }, segments: [{ chunk_id: call.body.chunk_id, chunk_type: 'text', content_tag: REF.content_tag, text: 'a' }], truncated: true }) })],
  ])('%s is internal', async (_name, respond) => {
    const { deps } = client((call) => respond(call))
    expect(await read(deps)).toEqual({ ok: false, code: 'internal' })
  })

  it('an error envelope is read by its code even on a 200 (R6.2)', async () => {
    const body = fixture('read.not-found.json').response.body
    const { deps } = client((call) => ({ status: 200, json: async () => echo(body, call) }))
    expect(await read(deps)).toEqual({ ok: false, code: 'not_found' })
  })

  it('a success body on another status is internal', async () => {
    const { deps } = client((call) => ({ ...okPage(call.body), status: 203 }))
    expect(await read(deps)).toEqual({ ok: false, code: 'internal' })
  })

  it('a 200 that is not protocol_version 1 is internal (§0.3)', async () => {
    for (const v of [2, undefined, '1']) {
      const { deps } = client((call) => ({ status: 200, json: async () => ({ ...((await okPage(call.body).json()) as object), protocol_version: v }) }))
      expect(await read(deps)).toEqual({ ok: false, code: 'internal' })
    }
  })

  it('an error envelope has to echo this request; null only where R6.1 allows it', async () => {
    const base = fixture('read.not-found.json').response.body as any
    const with_ = (request_id: unknown, generation: unknown) => (call: Call) => ({ status: 404, json: async () => ({ error: { ...base.error, details: { ...base.error.details, request_id: request_id === 'echo' ? call.body.request_id : request_id, generation: generation === 'echo' ? call.body.generation : generation } } }) })
    expect(await read(client(with_('01JB3R0A1B2C3D4E5F6G7H8J9K', 'echo')).deps)).toEqual({ ok: false, code: 'internal' })
    expect(await read(client(with_('echo', 99)).deps)).toEqual({ ok: false, code: 'internal' })
    expect(await read(client(with_(undefined, 'echo')).deps)).toEqual({ ok: false, code: 'internal' })
    expect(await read(client(with_('echo', undefined)).deps)).toEqual({ ok: false, code: 'internal' })
    expect(await read(client(with_(null, null)).deps)).toEqual({ ok: false, code: 'not_found' })
    expect(await read(client(with_('echo', 'echo')).deps)).toEqual({ ok: false, code: 'not_found' })
  })

  it('an unreadable body is internal', async () => {
    const { deps } = client(() => ({ status: 200, json: async () => { throw new Error('bad') } }))
    expect(await read(deps)).toEqual({ ok: false, code: 'internal' })
  })

  it('branches on the error code, not the HTTP status', async () => {
    const body = fixture('read.not-found.json').response.body
    const { deps } = client((call) => ({ status: 500, json: async () => echo(body, call) }))
    expect(await read(deps)).toEqual({ ok: false, code: 'not_found' })
  })

  it.each([
    ['read.not-found.json', { ok: false, code: 'not_found' }],
    ['read.operation-disabled.json', { ok: false, code: 'forbidden', reason: 'operation_disabled' }],
    ['read.stale-anchor-missing.json', { ok: false, code: 'stale', reason: 'anchor_missing' }],
    ['read.timeout.json', { ok: false, code: 'timeout' }],
    ['read.internal.json', { ok: false, code: 'internal' }],
  ])('error fixture %s', async (name, expected) => {
    const sample = fixture(name)
    const { deps } = client((call) => ({ status: sample.response.status, json: async () => echo(sample.response.body, call) }))
    expect(await read(deps)).toEqual(expected)
  })

  it('an unknown code becomes internal, and a bad reason is dropped', async () => {
    const { deps: unknown } = client(() => ({ status: 400, json: async () => ({ error: { code: 'nope', details: { retryable: false, request_id: null, generation: null, reason: 'kept_reason' } } }) }))
    expect(await read(unknown)).toEqual({ ok: false, code: 'internal', reason: 'kept_reason' })
    const { deps: digits } = client(() => ({ status: 400, json: async () => ({ error: { code: 'stale', details: { retryable: false, request_id: null, generation: null, reason: 'a1' } } }) }))
    expect(await read(digits)).toEqual({ ok: false, code: 'stale' })
    const { deps: long } = client(() => ({ status: 400, json: async () => ({ error: { code: 'stale', details: { retryable: false, request_id: null, generation: null, reason: 'a'.repeat(65) } } }) }))
    expect(await read(long)).toEqual({ ok: false, code: 'stale' })
    const { deps: kept } = client(() => ({ status: 400, json: async () => ({ error: { code: 'stale', details: { retryable: false, request_id: null, generation: null, reason: 'a'.repeat(64) } } }) }))
    expect(await read(kept)).toEqual({ ok: false, code: 'stale', reason: 'a'.repeat(64) })
    const { deps: bare } = client(() => ({ status: 400, json: async () => ({ error: { code: 'stale' } }) }))
    expect(await read(bare)).toEqual({ ok: false, code: 'internal' })
  })

  it('stops after READ_MAX_PAGES with internal/too_large', async () => {
    const { deps, calls } = client((call, i) => okPage(call.body, { text: 'x', truncated: true, cursor: `c${i}` }))
    expect(await read(deps)).toEqual({ ok: false, code: 'internal', reason: 'too_large' })
    expect(calls).toHaveLength(READ_MAX_PAGES)
  })

  it('stops when the joined text passes READ_MAX_TEXT_BYTES', async () => {
    const { deps, calls } = client((call) => okPage(call.body, { text: 'a'.repeat(READ_MAX_TEXT_BYTES + 1) }))
    expect(await read(deps)).toEqual({ ok: false, code: 'internal', reason: 'too_large' })
    expect(calls).toHaveLength(1)
  })

  it('a joined second page that crosses the byte cap is too_large', async () => {
    const half = READ_MAX_TEXT_BYTES - 10
    const { deps } = client((call, i) => i === 0
      ? okPage(call.body, { text: 'a'.repeat(half), truncated: true, cursor: 'more' })
      : okPage(call.body, { text: 'b'.repeat(20) }))
    expect(await read(deps)).toEqual({ ok: false, code: 'internal', reason: 'too_large' })
  })

  it('abort before the request is cancelled and does not fetch', async () => {
    const ac = new AbortController()
    ac.abort()
    const { deps, calls } = client(() => 'hang')
    expect(await read(deps, REF, ac.signal)).toEqual({ ok: false, code: 'cancelled' })
    expect(calls).toEqual([])
  })

  it('abort during the request is cancelled', async () => {
    const ac = new AbortController()
    const { deps } = client(() => 'hang')
    const pending = read(deps, REF, ac.signal)
    await Promise.resolve()
    ac.abort()
    expect(await pending).toEqual({ ok: false, code: 'cancelled' })
  })

  it('a request timeout is timeout', async () => {
    vi.useFakeTimers()
    try {
      const { deps } = client(() => 'hang')
      const pending = read(deps)
      await vi.advanceTimersByTimeAsync(READ_TIMEOUT_MS)
      expect(await pending).toEqual({ ok: false, code: 'timeout' })
    } finally {
      vi.useRealTimers()
    }
  })

  it('a network error is unavailable', async () => {
    const { deps } = client(() => 'throw')
    expect(await read(deps)).toEqual({ ok: false, code: 'unavailable' })
  })

  it('no C is unauthorized and does not fetch', async () => {
    let called = 0
    const deps: ReadonlyDeps = {
      origin: ORIGIN,
      credential: () => null,
      fetch: (() => { called++; throw new Error('sent') }) as typeof fetch,
      newRequestId: ids(),
    }
    expect(await read(deps)).toEqual({ ok: false, code: 'unauthorized' })
    expect(called).toBe(0)
  })
})

describe('plainTitle', () => {
  it('strips control characters', () => {
    expect(plainTitle('a\u0000b\u001fc')).toBe('abc')
    expect(plainTitle('a\u007fb\u009fc')).toBe('abc')
    expect(plainTitle('a\tb')).toBe('ab')
  })

  it('strips bidi characters', () => {
    expect(plainTitle('a\u061cb')).toBe('ab')
    expect(plainTitle('a\u200eb\u200fc')).toBe('abc')
    expect(plainTitle('a\u202ab\u202ec')).toBe('abc')
    expect(plainTitle('a\u2066b\u2069c')).toBe('abc')
  })

  it('collapses whitespace', () => {
    expect(plainTitle('  hello   world  ')).toBe('hello world')
    expect(plainTitle('a \u202e  b')).toBe('a b')
  })

  it('cuts to 120 code points', () => {
    expect(plainTitle('a'.repeat(121))).toBe('a'.repeat(120))
    expect([...(plainTitle('😀'.repeat(121)) ?? '')]).toHaveLength(120)
  })

  it('is null when empty or not a string', () => {
    expect(plainTitle('')).toBeNull()
    expect(plainTitle('   ')).toBeNull()
    expect(plainTitle('\u200e')).toBeNull()
    expect(plainTitle(null)).toBeNull()
    expect(plainTitle(1)).toBeNull()
    expect(plainTitle({})).toBeNull()
  })
})

afterEach(() => {
  vi.useRealTimers()
})

describe('readCitationChunk: the body is read under the same timeout, abort and cap', () => {
  const streaming = (chunks: Uint8Array[], hangAfter: boolean): typeof fetch => (async (_url: string, init?: RequestInit) => {
    let i = 0
    const body = new ReadableStream<Uint8Array>({
      pull(controller) {
        if (i < chunks.length) { controller.enqueue(chunks[i++]); return }
        if (!hangAfter) { controller.close(); return }
        return new Promise<void>((_resolve, reject) => init?.signal?.addEventListener('abort', () => reject(new Error('aborted'))))
      },
    })
    return new Response(body, { status: 200 })
  }) as typeof fetch

  it('an abort while the body is still coming is cancelled', async () => {
    const abort = new AbortController()
    const deps: ReadonlyDeps = { origin: ORIGIN, credential: () => 'C', fetch: streaming([new TextEncoder().encode('{"a":')], true), newRequestId: ids() }
    const pending = readCitationChunk(deps, 15, REF, abort.signal)
    await new Promise((r) => setTimeout(r, 5))
    abort.abort()
    expect(await pending).toEqual({ ok: false, code: 'cancelled' })
  })

  it('a body that stops coming times out', async () => {
    vi.useFakeTimers()
    try {
      const deps: ReadonlyDeps = { origin: ORIGIN, credential: () => 'C', fetch: streaming([new TextEncoder().encode('{"a":')], true), newRequestId: ids() }
      const pending = readCitationChunk(deps, 15, REF, new AbortController().signal)
      await vi.advanceTimersByTimeAsync(READ_TIMEOUT_MS)
      expect(await pending).toEqual({ ok: false, code: 'timeout' })
    } finally {
      vi.useRealTimers()
    }
  })

  it('a body past READ_MAX_RESPONSE_BYTES is internal/too_large, read no further', async () => {
    const piece = new Uint8Array(64 * 1024).fill(0x20)
    const pieces = Array.from({ length: Math.ceil(READ_MAX_RESPONSE_BYTES / piece.byteLength) + 1 }, () => piece)
    const deps: ReadonlyDeps = { origin: ORIGIN, credential: () => 'C', fetch: streaming(pieces, false), newRequestId: ids() }
    expect(await readCitationChunk(deps, 15, REF, new AbortController().signal)).toEqual({ ok: false, code: 'internal', reason: 'too_large' })
  })

  it('a body that is not UTF-8 is internal', async () => {
    const deps: ReadonlyDeps = { origin: ORIGIN, credential: () => 'C', fetch: streaming([new Uint8Array([0x7b, 0xff, 0x7d])], false), newRequestId: ids() }
    expect(await readCitationChunk(deps, 15, REF, new AbortController().signal)).toEqual({ ok: false, code: 'internal' })
  })

  it('a page escaped up to six times its text bytes still fits (\\u003c)', async () => {
    const text = '<'.repeat(65_536)
    const fetchImpl: typeof fetch = (async (_url: string, init?: RequestInit) => {
      const req = JSON.parse(String(init?.body))
      const body = { protocol_version: 1, request_id: req.request_id, generation: req.generation, effective_scope: { knowledge_base_ids: [REF.knowledge_base_id] }, knowledge_id: REF.knowledge_id, revision: { source_revision: 's', index_build_id: 'i' }, segments: [{ chunk_id: REF.chunk_id, chunk_type: 'text', chunk_revision: 1, content_tag: REF.content_tag, text }], truncated: false }
      // Go's encoder writes < as \u003c: six bytes for one.
      return new Response(JSON.stringify(body).split('<').join('\\u003c'), { status: 200 })
    }) as typeof fetch
    const got = await readCitationChunk({ origin: ORIGIN, credential: () => 'C', fetch: fetchImpl, newRequestId: ids() }, 15, REF, new AbortController().signal)
    expect(got).toMatchObject({ ok: true, text })
  })
})

describe('readCitationChunk: the visible set is asked before every page (U7.2)', () => {
  it('a set that drops the knowledge base before a page stops the read with not_found, sending nothing more', async () => {
    let visible = true
    const { deps, calls } = client((call) => {
      visible = false
      return okPage(call.body, { truncated: true })
    })
    expect(await readCitationChunk(deps, 15, REF, new AbortController().signal, () => visible)).toEqual({ ok: false, code: 'not_found' })
    expect(calls).toHaveLength(1)
  })

  it('not visible from the start sends no request', async () => {
    const { deps, calls } = client((call) => okPage(call.body))
    expect(await readCitationChunk(deps, 15, REF, new AbortController().signal, () => false)).toEqual({ ok: false, code: 'not_found' })
    expect(calls).toHaveLength(0)
  })
})
