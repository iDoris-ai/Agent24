import { describe, expect, it } from 'vitest'
import {
  BRIDGE_BACKOFF_GRACE_MS,
  BRIDGE_BACKOFF_MS,
  BRIDGE_BACKOFF_STRIKES,
  BRIDGE_BURST,
  BRIDGE_DEDUPE_LIMIT,
  BRIDGE_SOFT_LIMIT,
  INTENT_ACTIVATION_MS,
  INTENT_ADMISSION_POLL_MS,
  INTENT_CONFIRM_MS,
  INTENT_COOLDOWN_MS,
  KnowledgeBridge,
  newUlid,
  type BridgeArrival,
} from './knowledge-bridge'

const ORIGIN = 'https://kb.example.org'
const REF = { knowledge_base_id: 'kb-1', knowledge_id: 'doc-1', chunk_id: 'c-1', source_revision: 'r1', index_build_id: 'b1', chunk_revision: 3, content_tag: 'tag-1' }

let seq = 0
const ulid = (): string => {
  const n = (seq++).toString(32).toUpperCase().replace(/I/g, 'X').replace(/L/g, 'Y').replace(/O/g, 'Z').replace(/U/g, 'W')
  return ('01J9ZQ3V6R8XKQ4M2N5P7T' + n.padStart(4, '0')).slice(-26)
}

function setup(opts: {
  open?: boolean
  generation?: number
  citation?: NonNullable<ConstructorParameters<typeof KnowledgeBridge>[0]['citation']>
  intent?: NonNullable<ConstructorParameters<typeof KnowledgeBridge>[0]['intent']>
} = {}) {
  let now = 1_000
  const timers: Array<{ at: number; fn: () => void; handle: number }> = []
  let nextHandle = 0
  const sent: Array<{ msg: Record<string, any>; document: number }> = []
  const logs: string[] = []
  const admission = { open: opts.open ?? true, generation: opts.generation ?? 5 }
  let closes = 0
  const bridge = new KnowledgeBridge({
    origin: ORIGIN,
    admission: () => ({ ...admission }),
    send: (text, document) => sent.push({ msg: JSON.parse(text), document }),
    onClose: () => { closes++ },
    now: () => now,
    setTimer: (fn, ms) => { const handle = ++nextHandle; timers.push({ at: now + ms, fn, handle }); return handle },
    clearTimer: (h) => { const i = timers.findIndex((t) => t.handle === h); if (i >= 0) timers.splice(i, 1) },
    log: (m) => logs.push(m),
    viewInstanceId: 'V'.repeat(22),
    ...(opts.citation ? { citation: opts.citation } : {}),
    ...(opts.intent ? { intent: opts.intent } : {}),
  })
  bridge.newDocument(1)
  const arrive = (body: unknown, over: Partial<BridgeArrival> = {}): void => {
    const bytes = typeof body === 'string' ? Buffer.from(body) : body instanceof Uint8Array ? body : Buffer.from(JSON.stringify(body))
    bridge.receive({ fromView: true, mainFrame: true, documentOrigin: ORIGIN, document: 1, bytes, senderKey: 's', ...over })
  }
  const msg = (type: string, payload: unknown, extra: Record<string, unknown> = {}) =>
    ({ v: 3, direction: 'page_to_host', type, id: ulid(), generation: admission.generation, document_id: bridge.currentDocumentId(), payload, ...extra })
  const replies = () => sent.filter((s) => s.msg.type === 'reply').map((s) => s.msg)
  const last = () => replies().at(-1)
  const advance = (ms: number): void => {
    now += ms
    for (const t of [...timers].sort((a, b) => a.at - b.at)) {
      if (t.at <= now) { timers.splice(timers.indexOf(t), 1); t.fn() }
    }
  }
  return { bridge, arrive, msg, sent, replies, last, logs, admission, closes: () => closes, advance, setNow: (n: number) => { now = n }, time: () => now }
}

describe('newUlid', () => {
  it('is a ULID led by the time', () => {
    const a = newUlid(1_700_000_000_000)
    expect(a).toMatch(/^[0-9A-HJKMNP-TV-Z]{26}$/)
    expect(newUlid(1_700_000_000_001).slice(0, 10) > a.slice(0, 10)).toBe(true)
  })
})

describe('hello (U6.3, U7.3)', () => {
  it('greets every new document, with the newest state after it', () => {
    const t = setup()
    expect(t.sent.map((s) => s.msg.type)).toEqual(['hello'])
    expect(t.sent[0].msg).toMatchObject({ v: 3, direction: 'host_to_page', generation: 5, document_id: t.bridge.currentDocumentId(), payload: { supported: [3], view_instance_id: 'V'.repeat(22) } })
    expect(t.bridge.currentDocumentId()).toMatch(/^[A-Za-z0-9_-]{22}$/)
    t.bridge.setState({ profile_state: 'ready', theme: 'dark', locale: 'en' })
    t.bridge.newDocument(2)
    expect(t.sent.slice(1).map((s) => [s.msg.type, s.document])).toEqual([['state', 1], ['hello', 2], ['state', 2]])
  })

  it('answers up to three hello requests per document, then treats them as stale', () => {
    const t = setup({ open: false })
    for (let i = 0; i < 3; i++) t.arrive(t.msg('status', { state: 'loading' }, { generation: 0 }))
    t.advance(1000)
    expect(t.sent.filter((s) => s.msg.type === 'hello')).toHaveLength(4)
    expect(t.replies()).toEqual([])
    t.arrive(t.msg('status', { state: 'loading' }, { generation: 0 }))
    expect(t.last()?.payload).toEqual({ result: 'rejected', code: 'invalid_request', reason: 'stale_generation' })
    t.bridge.newDocument(2)
    t.arrive(t.msg('status', { state: 'loading' }, { generation: 0 }), { document: 2 })
    expect(t.replies()).toHaveLength(1)
  })

  it('keeps hello and state within 5 a second, holding only the newest state', () => {
    const t = setup()
    for (let i = 0; i < 6; i++) t.bridge.setState({ profile_state: 'ready', theme: i % 2 ? 'light' : 'dark', locale: `l${i}` })
    // hello + 4 states fit the first second; the rest wait, newest only.
    expect(t.sent).toHaveLength(5)
    t.advance(1000)
    expect(t.sent.map((s) => s.msg.payload.locale ?? 'hello')).toEqual(['hello', 'l0', 'l1', 'l2', 'l3', 'l5'])
  })
})

describe('U6.4 ①–⑤: silent, some of it a hard violation', () => {
  it('drops what is not from the view, has no frame or belongs to another document', () => {
    const t = setup()
    t.arrive(t.msg('status', { state: 'ready' }), { fromView: false })
    t.arrive(t.msg('status', { state: 'ready' }), { mainFrame: null })
    t.arrive(t.msg('status', { state: 'ready' }), { document: 0 })
    t.arrive(t.msg('status', { state: 'ready' }), { document: null })
    t.arrive(t.msg('status', { state: 'ready' }), { documentOrigin: null })
    expect(t.replies()).toEqual([])
    expect(t.closes()).toBe(0)
  })

  it('a sub-frame of the view closes the bridge silently (②)', () => {
    const t = setup()
    t.arrive(t.msg('status', { state: 'ready' }), { mainFrame: false })
    expect(t.replies()).toEqual([])
    expect(t.closes()).toBe(1)
    expect(t.bridge.isClosed()).toBe(true)
  })

  it('a main frame on another origin closes the bridge silently (③)', () => {
    const t = setup()
    t.arrive(t.msg('status', { state: 'ready' }), { documentOrigin: 'https://evil.example' })
    expect(t.replies()).toEqual([])
    expect(t.closes()).toBe(1)
  })

  it('after closing nothing is taken or sent', () => {
    const t = setup()
    t.arrive(t.msg('status', { state: 'ready' }), { mainFrame: false })
    const before = t.sent.length
    t.arrive(t.msg('status', { state: 'ready' }))
    t.bridge.setState({ profile_state: 'ready', theme: 'dark', locale: 'en' })
    t.bridge.newDocument(2)
    expect(t.sent).toHaveLength(before)
  })

  it('a document resumed after a cancelled navigation keeps its tables and gets the newest state', () => {
    const t = setup()
    t.bridge.setState({ profile_state: 'ready', theme: 'dark', locale: 'en' })
    const m = t.msg('status', { state: 'ready' })
    t.arrive(m)
    t.bridge.leaveDocument()
    t.bridge.resumeDocument(1)
    expect(t.sent.at(-1)?.msg.type).toBe('state')
    t.arrive(m)
    expect(t.last()?.payload.reason).toBe('duplicate_id')
  })

  it('every document gets a new document_id; every host message carries the current one (decision #37)', () => {
    const t = setup()
    const first = t.bridge.currentDocumentId()
    t.bridge.newDocument(2)
    const second = t.bridge.currentDocumentId()
    expect(second).not.toBe(first)
    expect(t.sent.map((s) => s.msg.document_id)).toEqual([first, second])
    t.bridge.leaveDocument()
    expect(t.bridge.currentDocumentId()).toBeNull()
    t.bridge.resumeDocument(2)
    expect(t.bridge.currentDocumentId()).toBe(second)
  })

  it('a message with another document_id goes silently, uncounted, before any table (decision #37)', () => {
    const t = setup()
    const old = t.bridge.currentDocumentId()
    const late = t.msg('status', { state: 'ready' })
    t.bridge.newDocument(2)
    // Late from the replaced document, though the metadata says document 2.
    for (let i = 0; i < 6; i++) t.arrive({ ...late, id: ulid(), document_id: old }, { document: 2 })
    // Not even a forbidden key or a hello request of the old document counts.
    t.arrive({ ...t.msg('status', { state: 'ready', url: 'x' }), document_id: old }, { document: 2 })
    t.arrive({ ...t.msg('status', { state: 'ready' }, { generation: 0 }), document_id: old }, { document: 2 })
    t.arrive({ ...t.msg('status', { state: 'ready' }), document_id: 'not even an id' }, { document: 2 })
    expect(t.replies()).toEqual([])
    expect(t.closes()).toBe(0)
    expect(t.sent.filter((s) => s.msg.type === 'hello')).toHaveLength(2)
    // The same id from this document is new to it.
    t.arrive({ ...late, document_id: t.bridge.currentDocumentId() }, { document: 2 })
    expect(t.last()).toMatchObject({ in_reply_to: late.id, payload: { result: 'done' } })
  })

  it('a message without document_id is a schema violation, except a hello request', () => {
    const t = setup()
    t.arrive(t.msg('status', { state: 'ready' }, { document_id: undefined }))
    expect(t.last()?.payload.reason).toBe('schema_violation')
    t.arrive(t.msg('status', { state: 'loading' }, { generation: 0, document_id: undefined }))
    expect(t.sent.filter((s) => s.msg.type === 'hello')).toHaveLength(2)
  })

  it('between documents nothing is taken', () => {
    const t = setup()
    t.bridge.leaveDocument()
    t.arrive(t.msg('status', { state: 'ready' }))
    expect(t.replies()).toEqual([])
  })

  it('counts drops per sender and logs a sample, never the body', () => {
    const t = setup()
    for (let i = 0; i < 100; i++) t.arrive(t.msg('status', { state: 'ready' }), { fromView: false, senderKey: 'x' })
    const lines = t.logs.filter((l) => l.includes('step=①'))
    expect(lines).toHaveLength(2)
    expect(lines.join('\n')).not.toContain('ready')
  })
})

describe('⑥ rate (U6.6, U6.7)', () => {
  it('takes a burst of 20, then one overloaded per window and silence', () => {
    const t = setup()
    for (let i = 0; i < BRIDGE_BURST; i++) t.arrive(t.msg('status', { state: 'ready' }))
    expect(t.replies().every((r) => r.payload.result === 'done')).toBe(true)
    t.arrive(t.msg('status', { state: 'ready' }))
    t.arrive(t.msg('status', { state: 'ready' }))
    const over = t.replies().slice(BRIDGE_BURST)
    expect(over).toHaveLength(1)
    expect(over[0]).not.toHaveProperty('in_reply_to')
    expect(over[0].payload).toEqual({ result: 'rejected', code: 'unavailable', reason: 'overloaded', retry_after_ms: BRIDGE_BACKOFF_MS })
    t.advance(BRIDGE_BACKOFF_MS)
    t.arrive(t.msg('status', { state: 'ready' }))
    expect(t.last()?.payload.result).toBe('done')
    expect(t.closes()).toBe(0)
  })

  it('three more within the window after the grace make one soft violation; inside the grace none', () => {
    const t = setup()
    for (let i = 0; i < BRIDGE_BURST + 1; i++) t.arrive(t.msg('status', { state: 'ready' }))
    for (let i = 0; i < 5; i++) t.arrive(t.msg('status', { state: 'ready' }))
    expect(t.logs.some((l) => l.includes('closed'))).toBe(false)
    // Windows with three late arrivals each: the first window's early ones did
    // not count, so four windows are four soft violations and the fifth closes.
    const window = (): void => {
      t.advance(500)
      for (let i = 0; i < 3; i++) t.arrive(t.msg('status', { state: 'ready' }))
      t.advance(500)
      for (let i = 0; i < BRIDGE_BURST + 1; i++) t.arrive(t.msg('status', { state: 'ready' }))
    }
    for (let w = 0; w < 4; w++) window()
    expect(t.closes()).toBe(0)
    window()
    expect(t.closes()).toBe(1)
  })
})

describe('⑦–⑫: rejected with a reply', () => {
  it('⑦ too large, or not one inline body', () => {
    const t = setup()
    t.arrive(Buffer.alloc(8 * 1024 + 1, 0x20))
    expect(t.last()).toEqual(expect.objectContaining({ payload: { result: 'rejected', code: 'invalid_request', reason: 'too_large' } }))
    expect(t.last()).not.toHaveProperty('in_reply_to')
    t.arrive('{}', { bytes: null })
    expect(t.last()?.payload.reason).toBe('too_large')
  })

  it('⑧ bad JSON, without in_reply_to', () => {
    const t = setup()
    const m = t.msg('status', { state: 'ready' })
    t.arrive(JSON.stringify(m).replace('"payload"', '"id":"x","payload"'))
    expect(t.last()?.payload).toEqual({ result: 'rejected', code: 'invalid_request', reason: 'schema_violation' })
    expect(t.last()).not.toHaveProperty('in_reply_to')
  })

  it('⑨ a forbidden key: forbidden_key, then the bridge closes', () => {
    const t = setup()
    const m = t.msg('status', { state: 'ready', origin: ORIGIN })
    t.arrive(m)
    expect(t.last()).toMatchObject({ in_reply_to: m.id, payload: { result: 'rejected', code: 'invalid_request', reason: 'forbidden_key' } })
    expect(t.closes()).toBe(1)
  })

  it('⑪ another version: supported list; five in 10 s make one soft violation', () => {
    const t = setup()
    const m = t.msg('status', { state: 'ready' }, { v: 2 })
    t.arrive(m)
    expect(t.last()).toMatchObject({ in_reply_to: m.id, payload: { reason: 'unsupported_protocol_version', supported: [3] } })
    // 4 more → 1 soft violation; 4 more soft ones from ⑫ close it, one fewer does not.
    for (let i = 0; i < 4; i++) t.arrive(t.msg('status', { state: 'ready' }, { v: 2 }))
    for (let i = 0; i < 3; i++) t.arrive(t.msg('status', { state: 'nope' }))
    expect(t.closes()).toBe(0)
    t.arrive(t.msg('status', { state: 'nope' }))
    expect(t.closes()).toBe(1)
  })

  it('⑫ schema: counted, except the default ID refusal', () => {
    const t = setup()
    for (let i = 0; i < 6; i++) t.arrive(t.msg('citation', { ...REF, chunk_id: 'a/b' }))
    expect(t.replies().every((r) => r.payload.reason === 'schema_violation')).toBe(true)
    expect(t.closes()).toBe(0)
    for (let i = 0; i < 4; i++) t.arrive(t.msg('citation', { ...REF, chunk_id: '' }))
    expect(t.closes()).toBe(0)
    t.arrive(t.msg('citation', { ...REF, chunk_id: '' }))
    expect(t.closes()).toBe(1)
  })

  it('soft violations older than 10 s no longer count', () => {
    const t = setup()
    for (let i = 0; i < 4; i++) t.arrive(t.msg('status', { state: 'nope' }))
    t.advance(10_000)
    for (let i = 0; i < 4; i++) t.arrive(t.msg('status', { state: 'nope' }))
    expect(t.closes()).toBe(0)
  })
})

describe('⑬–⑯', () => {
  it('⑬ not admitted: not_ready, never counted', () => {
    const t = setup({ open: false })
    for (let i = 0; i < 10; i++) t.arrive(t.msg('status', { state: 'ready' }))
    expect(t.last()?.payload).toEqual({ result: 'rejected', code: 'unavailable', reason: 'not_ready' })
    expect(t.closes()).toBe(0)
  })

  it('⑭ another generation: stale_generation, counted', () => {
    const t = setup()
    t.arrive(t.msg('status', { state: 'ready' }, { generation: 4 }))
    expect(t.last()?.payload.reason).toBe('stale_generation')
    for (let i = 0; i < 4; i++) t.arrive(t.msg('status', { state: 'ready' }, { generation: 6 }))
    expect(t.closes()).toBe(1)
  })

  it('⑮ a repeated id within the document: duplicate_id; a new document forgets', () => {
    const t = setup()
    const m = t.msg('status', { state: 'ready' })
    t.arrive(m)
    t.arrive(m)
    expect(t.replies().map((r) => r.payload.result === 'done' ? 'done' : r.payload.reason)).toEqual(['done', 'duplicate_id'])
    t.bridge.newDocument(2)
    t.arrive({ ...m, document_id: t.bridge.currentDocumentId() }, { document: 2 })
    expect(t.last()?.payload.result).toBe('done')
  })

  it('only messages that pass every step are remembered', () => {
    const t = setup({ open: false })
    const m = t.msg('status', { state: 'ready' })
    t.arrive(m)
    t.admission.open = true
    t.arrive(m)
    expect(t.last()?.payload.result).toBe('done')
  })

  it('the dedupe table keeps the newest 4096 ids', () => {
    const t = setup()
    const first = t.msg('status', { state: 'ready' })
    t.arrive(first)
    for (let i = 0; i < BRIDGE_DEDUPE_LIMIT; i++) {
      t.advance(200)
      t.arrive(t.msg('status', { state: 'ready' }))
    }
    t.advance(200)
    t.arrive(first)
    expect(t.last()?.payload.result).toBe('done')
  })
})

describe('what this slice does with accepted messages', () => {
  it('status is done, cancel finds nothing, citation and intent are not ready yet', () => {
    const t = setup()
    const s = t.msg('status', { state: 'error', error_code: 'unauthorized' })
    const c = t.msg('cancel', { target_id: s.id })
    const r = t.msg('citation', REF)
    const i = t.msg('return_to_chat_intent', { citations: [REF] })
    for (const m of [s, c, r, i]) t.arrive(m)
    expect(t.replies().map((x) => [x.in_reply_to, x.payload])).toEqual([
      [s.id, { result: 'done' }],
      [c.id, { result: 'accepted' }],
      [r.id, { result: 'rejected', code: 'unavailable', reason: 'not_ready' }],
      [i.id, { result: 'rejected', code: 'unavailable', reason: 'not_ready' }],
    ])
  })

  it('replies go to the document they were made for', () => {
    const t = setup()
    t.arrive(t.msg('status', { state: 'ready' }))
    expect(t.last()).toBeDefined()
    expect(t.replies().length).toBe(1)
    expect(t.sent.filter((s) => s.msg.type === 'reply').every((s) => s.document === 1)).toBe(true)
  })

  it('destroy is quiet and not a violation', () => {
    const t = setup()
    t.bridge.destroy()
    t.arrive(t.msg('status', { state: 'ready' }))
    expect(t.closes()).toBe(0)
    expect(t.replies()).toEqual([])
  })
})

const flush = () => new Promise((resolve) => setTimeout(resolve, 0))

type ReadValue = { ok: true; text: string; contentTag: string; title: string | null } | { ok: false; code: string; reason?: string }

function citationHarness(scope0: readonly string[] | null = ['kb-1']) {
  const reads: Array<{ generation: number; signal: AbortSignal; visible: () => boolean }> = []
  const shown: Array<{ title: string | null; text: string }> = []
  const pending: Array<(value: ReadValue) => void> = []
  let scope = scope0
  const citation = {
    scope: () => scope,
    read: (generation: number, _ref: typeof REF, signal: AbortSignal, visible: () => boolean) => {
      reads.push({ generation, signal, visible })
      return new Promise<ReadValue>((resolve) => {
        const finish = (value: ReadValue): void => resolve(value)
        if (signal.aborted) { finish({ ok: false, code: 'cancelled' }); return }
        signal.addEventListener('abort', () => finish({ ok: false, code: 'cancelled' }))
        pending.push(finish)
      })
    },
    show: (s: { title: string | null; text: string }) => { shown.push({ title: s.title, text: s.text }) },
  }
  return { citation, reads, shown, pending, setScope: (next: readonly string[] | null) => { scope = next } }
}

const okRead = (text = 'body', title: string | null = 'Title'): ReadValue => ({ ok: true, text, contentTag: 'tag-1', title })

describe('citation, cancel, and the in-flight table (U7.2, U6.7, §9.3)', () => {
  it('a citation outside the visible set is done/not_found and is not read', () => {
    const c = citationHarness(['other'])
    const t = setup({ citation: c.citation })
    t.arrive(t.msg('citation', REF))
    expect(t.last()?.payload).toEqual({ result: 'done', code: 'not_found' })
    expect(c.reads).toEqual([])
    expect(c.shown).toEqual([])
  })

  it('a null scope is the same not_found, with no read', () => {
    const c = citationHarness(null)
    const t = setup({ citation: c.citation })
    t.arrive(t.msg('citation', REF))
    expect(t.last()?.payload).toEqual({ result: 'done', code: 'not_found' })
    expect(c.reads).toEqual([])
  })

  it('inside the set: accepted, then one show, then done, at the admission generation', async () => {
    const c = citationHarness()
    const t = setup({ citation: c.citation })
    const m = t.msg('citation', REF)
    t.arrive(m)
    expect(t.last()?.payload).toEqual({ result: 'accepted' })
    expect(c.reads.map((r) => r.generation)).toEqual([5])
    t.admission.generation = 9
    c.pending[0](okRead('body', 'Title'))
    await flush()
    expect(c.shown).toEqual([{ title: 'Title', text: 'body' }])
    expect(t.replies().map((r) => [r.in_reply_to, r.payload])).toEqual([
      [m.id, { result: 'accepted' }],
      [m.id, { result: 'done' }],
    ])
  })

  it('a read that waits for a slot keeps the generation captured at acceptance', async () => {
    const c = citationHarness()
    const t = setup({ citation: c.citation })
    t.arrive(t.msg('citation', REF))
    t.arrive(t.msg('citation', REF))
    t.arrive(t.msg('citation', REF))
    expect(c.reads).toHaveLength(2)
    t.admission.generation = 9
    c.pending[0](okRead())
    await flush()
    expect(c.reads).toHaveLength(3)
    expect(c.reads[2].generation).toBe(5)
  })

  it('a failed read is done with the read code and its reason', async () => {
    const c = citationHarness()
    const t = setup({ citation: c.citation })
    const m = t.msg('citation', REF)
    t.arrive(m)
    c.pending[0]({ ok: false, code: 'stale', reason: 'anchor_missing' })
    await flush()
    expect(t.last()?.payload).toEqual({ result: 'done', code: 'stale', reason: 'anchor_missing' })
    expect(c.shown).toEqual([])
  })

  it('a failed read without a reason omits reason', async () => {
    const c = citationHarness()
    const t = setup({ citation: c.citation })
    t.arrive(t.msg('citation', REF))
    c.pending[0]({ ok: false, code: 'not_found' })
    await flush()
    expect(t.last()?.payload).toEqual({ result: 'done', code: 'not_found' })
    expect(t.last()?.payload).not.toHaveProperty('reason')
  })

  it('a read that throws still gets one terminal reply: done/internal (#1662 L3)', async () => {
    const c = citationHarness()
    c.citation.read = () => Promise.reject(new Error('boom'))
    const t = setup({ citation: c.citation })
    const m = t.msg('citation', REF)
    t.arrive(m)
    await flush()
    expect(t.replies().filter((r) => r.in_reply_to === m.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }, { result: 'done', code: 'internal' }])
    expect(c.shown).toEqual([])
  })

  it('a read that waits out the bridge budget is rejected overloaded', async () => {
    const c = citationHarness()
    const t = setup({ citation: c.citation })
    t.arrive(t.msg('citation', REF))
    t.arrive(t.msg('citation', REF))
    const third = t.msg('citation', REF)
    t.arrive(third)
    expect(t.replies().filter((r) => r.in_reply_to === third.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }])
    expect(c.reads).toHaveLength(2)
    t.advance(10_000)
    await flush()
    expect(t.replies().filter((r) => r.in_reply_to === third.id).map((r) => r.payload)).toEqual([
      { result: 'accepted' },
      { result: 'rejected', code: 'unavailable', reason: 'overloaded', retry_after_ms: BRIDGE_BACKOFF_MS },
    ])
    expect(c.shown).toEqual([])
  })

  it('the 5th in-flight message is uncounted overloaded and is not remembered', async () => {
    const c = citationHarness()
    const t = setup({ citation: c.citation })
    for (let i = 0; i < 4; i++) t.arrive(t.msg('citation', REF))
    const fifth = t.msg('citation', REF)
    t.arrive(fifth)
    expect(t.last()?.payload).toEqual({ result: 'rejected', code: 'unavailable', reason: 'overloaded', retry_after_ms: BRIDGE_BACKOFF_MS })
    t.arrive(fifth)
    expect(t.last()?.payload.reason).toBe('overloaded')
    expect(t.replies().filter((r) => r.in_reply_to === fifth.id).every((r) => r.payload.reason === 'overloaded')).toBe(true)
    for (let i = 0; i < 5; i++) t.arrive(t.msg('citation', REF))
    expect(t.closes()).toBe(0)
    c.pending[0](okRead())
    await flush()
    t.arrive(fifth)
    expect(t.last()?.payload).toEqual({ result: 'accepted' })
    expect(t.last()?.in_reply_to).toBe(fifth.id)
  })

  it('the visible set is asked again: the read gets it live, and a set changed before showing is not_found, not shown', async () => {
    const c = citationHarness()
    const t = setup({ citation: c.citation })
    const m = t.msg('citation', REF)
    t.arrive(m)
    expect(c.reads[0].visible()).toBe(true)
    // Settings drop the knowledge base while the read runs.
    c.setScope(['kb-2'])
    expect(c.reads[0].visible()).toBe(false)
    c.pending[0](okRead())
    await flush()
    expect(t.replies().filter((r) => r.in_reply_to === m.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }, { result: 'done', code: 'not_found' }])
    expect(c.shown).toEqual([])
    c.setScope(null)
    const n = t.msg('citation', REF)
    t.arrive(n)
    expect(t.last()?.payload).toEqual({ result: 'done', code: 'not_found' })
  })

  it('a queued citation whose knowledge base leaves the set never reads', async () => {
    const c = citationHarness()
    const read = c.citation.read
    let started = 0
    // What the real read does first (knowledge-readonly): ask visible before the request.
    c.citation.read = (g, r, s, visible) => { started++; return visible() ? read(g, r, s, visible) : Promise.resolve({ ok: false, code: 'not_found' }) }
    const t = setup({ citation: c.citation })
    t.arrive(t.msg('citation', REF))
    t.arrive(t.msg('citation', REF))
    const third = t.msg('citation', REF)
    t.arrive(third)
    expect(started).toBe(2)
    c.setScope([])
    c.pending[0](okRead())
    await flush()
    await flush()
    expect(started).toBe(3)
    expect(c.reads).toHaveLength(2)
    expect(t.replies().filter((r) => r.in_reply_to === third.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }, { result: 'done', code: 'not_found' }])
  })

  it('⑯ full: past the grace and within retry_after_ms, three more are one soft violation (U6.6)', async () => {
    const windows = (n: number) => {
      const c = citationHarness()
      const t = setup({ citation: c.citation })
      for (let i = 0; i < 4; i++) t.arrive(t.msg('citation', REF))
      for (let w = 0; w < n; w++) {
        t.arrive(t.msg('citation', REF)) // opens the window, uncounted
        t.advance(BRIDGE_BACKOFF_GRACE_MS)
        for (let i = 0; i < BRIDGE_BACKOFF_STRIKES; i++) t.arrive(t.msg('citation', REF))
        t.advance(BRIDGE_BACKOFF_MS - BRIDGE_BACKOFF_GRACE_MS)
      }
      return t.closes()
    }
    expect(windows(BRIDGE_SOFT_LIMIT - 1)).toBe(0)
    expect(windows(BRIDGE_SOFT_LIMIT)).toBe(1)
  })

  it('⑯: within the window, arrivals under the limit count even when they are let through (U6.6: arrived, not refused)', async () => {
    const run = async (arrivals: number) => {
      const c = citationHarness()
      const t = setup({ citation: c.citation })
      for (let i = 0; i < 4; i++) t.arrive(t.msg('citation', REF))
      t.arrive(t.msg('citation', REF)) // full: opens ⑯'s window
      // Four soft violations from elsewhere (⑫), so one more closes.
      for (let i = 0; i < BRIDGE_SOFT_LIMIT - 1; i++) t.arrive(t.msg('status', { state: 'nope' }))
      // Two reads finish: the table is no longer full, the window still runs.
      c.pending[0](okRead())
      c.pending[1](okRead())
      await flush()
      t.advance(BRIDGE_BACKOFF_GRACE_MS)
      for (let i = 0; i < arrivals; i++) t.arrive(t.msg('status', { state: 'ready' }))
      const passed = t.replies().slice(-arrivals).every((r) => r.payload.result === 'done')
      return { closes: t.closes(), passed }
    }
    const three = await run(BRIDGE_BACKOFF_STRIKES)
    expect(three.closes).toBe(1)
    const two = await run(BRIDGE_BACKOFF_STRIKES - 1)
    expect(two).toEqual({ closes: 0, passed: true })
  })

  it('⑯: cancel is not under that limit and never strikes', () => {
    const c = citationHarness()
    const t = setup({ citation: c.citation })
    for (let i = 0; i < 4; i++) t.arrive(t.msg('citation', REF))
    t.arrive(t.msg('citation', REF))
    for (let i = 0; i < BRIDGE_SOFT_LIMIT - 1; i++) t.arrive(t.msg('status', { state: 'nope' }))
    t.advance(BRIDGE_BACKOFF_GRACE_MS)
    for (let i = 0; i < BRIDGE_BACKOFF_STRIKES; i++) t.arrive(t.msg('cancel', { target_id: newUlid() }))
    expect(t.closes()).toBe(0)
  })

  it('⑯: the message whose strike closes the bridge still gets its overloaded first', () => {
    const c = citationHarness()
    const t = setup({ citation: c.citation })
    for (let i = 0; i < 4; i++) t.arrive(t.msg('citation', REF))
    let last = t.msg('citation', REF)
    for (let w = 0; w < BRIDGE_SOFT_LIMIT; w++) {
      t.arrive(t.msg('citation', REF))
      t.advance(BRIDGE_BACKOFF_GRACE_MS)
      for (let i = 0; i < BRIDGE_BACKOFF_STRIKES; i++) { last = t.msg('citation', REF); t.arrive(last) }
      if (w < BRIDGE_SOFT_LIMIT - 1) t.advance(BRIDGE_BACKOFF_MS - BRIDGE_BACKOFF_GRACE_MS)
    }
    expect(t.closes()).toBe(1)
    expect(t.replies().filter((r) => r.in_reply_to === last.id).map((r) => r.payload)).toEqual([
      { result: 'rejected', code: 'unavailable', reason: 'overloaded', retry_after_ms: BRIDGE_BACKOFF_MS },
    ])
  })

  it('⑯ full within the grace counts nothing', async () => {
    const c = citationHarness()
    const t = setup({ citation: c.citation })
    for (let i = 0; i < 4; i++) t.arrive(t.msg('citation', REF))
    for (let w = 0; w < BRIDGE_SOFT_LIMIT; w++) {
      for (let i = 0; i < 4; i++) t.arrive(t.msg('citation', REF))
      t.advance(BRIDGE_BACKOFF_MS)
    }
    expect(t.closes()).toBe(0)
  })

  it('cancel of a live target sends rejected/cancelled first, then accepted, and takes no slot', async () => {
    const c = citationHarness()
    const t = setup({ citation: c.citation })
    const target = t.msg('citation', REF)
    t.arrive(target)
    const cancel = t.msg('cancel', { target_id: target.id })
    t.arrive(cancel)
    expect(t.replies().slice(-2).map((r) => [r.in_reply_to, r.payload])).toEqual([
      [target.id, { result: 'rejected', code: 'cancelled' }],
      [cancel.id, { result: 'accepted' }],
    ])
    expect(c.reads[0].signal.aborted).toBe(true)
    await flush()
    expect(t.replies().filter((r) => r.in_reply_to === target.id).map((r) => r.payload)).toEqual([
      { result: 'accepted' },
      { result: 'rejected', code: 'cancelled' },
    ])
    const held: string[] = []
    for (let i = 0; i < 4; i++) {
      const m = t.msg('citation', REF)
      held.push(m.id)
      t.arrive(m)
    }
    const extra = t.msg('cancel', { target_id: ulid() })
    t.arrive(extra)
    expect(t.last()?.payload).toEqual({ result: 'accepted' })
    const blocked = t.msg('citation', REF)
    t.arrive(blocked)
    expect(t.last()?.payload.reason).toBe('overloaded')
    t.arrive(t.msg('cancel', { target_id: held[0] }))
    t.arrive(blocked)
    expect(t.last()?.payload).toEqual({ result: 'accepted' })
    expect(t.last()?.in_reply_to).toBe(blocked.id)
  })

  it('a cancel that misses only accepts', () => {
    const t = setup({ citation: citationHarness().citation })
    const cancel = t.msg('cancel', { target_id: ulid() })
    t.arrive(cancel)
    expect(t.replies().map((r) => r.payload)).toEqual([{ result: 'accepted' }])
  })

  it('return_to_chat_intent is still not ready', () => {
    const t = setup({ citation: citationHarness().citation })
    t.arrive(t.msg('return_to_chat_intent', { citations: [REF] }))
    expect(t.last()?.payload).toEqual({ result: 'rejected', code: 'unavailable', reason: 'not_ready' })
  })

  it('a new document aborts the old read and sends none of its later replies', async () => {
    const c = citationHarness()
    const t = setup({ citation: c.citation })
    const m = t.msg('citation', REF)
    t.arrive(m)
    const doc = t.bridge.currentDocumentId()
    t.bridge.newDocument(2)
    expect(c.reads[0].signal.aborted).toBe(true)
    c.pending[0]?.(okRead())
    await flush()
    expect(t.replies().filter((r) => r.in_reply_to === m.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }])
    expect(t.sent.filter((s) => s.msg.in_reply_to === m.id).every((s) => s.msg.document_id === doc)).toBe(true)
    expect(c.shown).toEqual([])
  })

  it('a read that finishes while away is delivered on resume, with the original document_id', async () => {
    const c = citationHarness()
    const t = setup({ citation: c.citation })
    const m = t.msg('citation', REF)
    const doc = t.bridge.currentDocumentId()
    t.arrive(m)
    t.bridge.leaveDocument()
    c.pending[0](okRead('kept', 'T'))
    await flush()
    expect(c.shown).toEqual([])
    expect(t.replies().filter((r) => r.payload.result === 'done')).toEqual([])
    t.bridge.resumeDocument(1)
    expect(c.shown).toEqual([{ title: 'T', text: 'kept' }])
    const done = t.replies().find((r) => r.payload.result === 'done')
    expect(done?.document_id).toBe(doc)
    expect(done?.in_reply_to).toBe(m.id)
    expect(t.sent.filter((s) => s.msg.in_reply_to === m.id && s.msg.payload.result === 'done').map((s) => s.document)).toEqual([1])
  })

  it('leave then a new document drops the held reply', async () => {
    const c = citationHarness()
    const t = setup({ citation: c.citation })
    const m = t.msg('citation', REF)
    t.arrive(m)
    t.bridge.leaveDocument()
    c.pending[0](okRead())
    await flush()
    t.bridge.newDocument(2)
    await flush()
    expect(t.replies().filter((r) => r.in_reply_to === m.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }])
    expect(c.shown).toEqual([])
  })

  it('a hello still waiting across leave and resume is sent (R2-1)', () => {
    const t = setup()
    for (let i = 0; i < 4; i++) t.bridge.setState({ profile_state: 'ready', theme: 'dark', locale: `l${i}` })
    const hellos = () => t.sent.filter((s) => s.msg.type === 'hello')
    expect(hellos()).toHaveLength(1)
    t.arrive(t.msg('status', { state: 'loading' }, { generation: 0 }))
    expect(hellos()).toHaveLength(1)
    t.bridge.leaveDocument()
    expect(hellos()).toHaveLength(1)
    t.bridge.resumeDocument(1)
    expect(hellos()).toHaveLength(1)
    t.advance(1000)
    expect(hellos()).toHaveLength(2)
    expect(hellos()[1].document).toBe(1)
    expect(hellos()[1].msg.document_id).toBe(t.bridge.currentDocumentId())
  })

  it('topLevel with mainFrame false is a silent drop; mainFrame false alone still closes', () => {
    const quiet = setup()
    quiet.arrive(quiet.msg('status', { state: 'ready' }), { mainFrame: false, topLevel: true })
    expect(quiet.replies()).toEqual([])
    expect(quiet.closes()).toBe(0)
    expect(quiet.bridge.isClosed()).toBe(false)
    quiet.arrive(quiet.msg('status', { state: 'ready' }))
    expect(quiet.last()?.payload.result).toBe('done')
    const hard = setup()
    hard.arrive(hard.msg('status', { state: 'ready' }), { mainFrame: false })
    expect(hard.replies()).toEqual([])
    expect(hard.closes()).toBe(1)
    expect(hard.bridge.isClosed()).toBe(true)
  })

  it('a mismatched document_id still spends a rate token', () => {
    const t = setup()
    const other = 'x'.repeat(22)
    for (let i = 0; i < BRIDGE_BURST; i++) t.arrive(t.msg('status', { state: 'ready' }, { document_id: other }))
    expect(t.replies()).toEqual([])
    t.arrive(t.msg('status', { state: 'ready' }))
    expect(t.last()?.payload).toEqual({ result: 'rejected', code: 'unavailable', reason: 'overloaded', retry_after_ms: BRIDGE_BACKOFF_MS })
  })

  it('generation below 1 sends nothing', () => {
    const t = setup({ generation: 0 })
    expect(t.sent).toEqual([])
    t.bridge.setState({ profile_state: 'ready', theme: 'dark', locale: 'en' })
    expect(t.sent).toEqual([])
    expect(t.logs.some((l) => l.includes('not sent: generation'))).toBe(true)
  })

  it('destroy aborts every in-flight read and sends no further reply', async () => {
    const c = citationHarness()
    const t = setup({ citation: c.citation })
    const m = t.msg('citation', REF)
    t.arrive(m)
    const n = t.replies().length
    t.bridge.destroy()
    expect(c.reads[0].signal.aborted).toBe(true)
    c.pending[0]?.(okRead())
    await flush()
    expect(t.replies()).toHaveLength(n)
    expect(t.replies().filter((r) => r.in_reply_to === m.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }])
    expect(c.shown).toEqual([])
    expect(t.closes()).toBe(0)
  })
})

function intentHarness(initialActivated: number | null = 1_000) {
  const base = citationHarness()
  let activated: number | null = initialActivated
  let room = 16
  let allow = true
  const attached: Array<Array<{ title: string | null; text: string; ref: { knowledge_base_id: string; knowledge_id: string; chunk_id: string } }>> = []
  const confirms: Array<{ ask: { count: number; titles: Array<string | null> }; signal: AbortSignal }> = []
  const pendingConfirm: Array<(value: 'confirmed' | 'declined' | 'unavailable') => void> = []
  const intent = {
    activatedAt: () => activated,
    draftRoom: () => room,
    confirm: (ask: { count: number; titles: Array<string | null> }, signal: AbortSignal) => {
      confirms.push({ ask, signal })
      return new Promise<'confirmed' | 'declined' | 'unavailable'>((resolve, reject) => {
        const fail = (): void => reject(new DOMException('The operation was aborted.', 'AbortError'))
        if (signal.aborted) { fail(); return }
        signal.addEventListener('abort', fail)
        pendingConfirm.push(resolve)
      })
    },
    attach: (items: Array<{ title: string | null; text: string; ref: { knowledge_base_id: string; knowledge_id: string; chunk_id: string } }>) => {
      attached.push(items.map((item) => ({ title: item.title, text: item.text, ref: { ...item.ref } })))
      return allow
    },
  }
  return {
    ...base,
    intent,
    confirms,
    pendingConfirm,
    attached,
    setActivated: (value: number | null) => { activated = value },
    setRoom: (value: number) => { room = value },
    setAllow: (value: boolean) => { allow = value },
  }
}

function reader(h: { pending: Array<(value: ReadValue) => void> }) {
  let n = 0
  return async (value: ReadValue): Promise<void> => {
    expect(h.pending.length).toBe(n + 1)
    h.pending[n](value)
    n += 1
    await flush()
  }
}

const cite = (chunk: string, kb = 'kb-1') => ({ ...REF, knowledge_base_id: kb, chunk_id: chunk })

function wired(activated: number | null = 1_000) {
  const h = intentHarness(activated)
  const t = setup({ citation: h.citation, intent: h.intent })
  return { h, t, read: reader(h) }
}

const forId = (t: ReturnType<typeof setup>, id: string) => t.replies().filter((r) => r.in_reply_to === id)

describe('return_to_chat_intent (U7.2, U6.7, §9.3)', () => {
  it('no activation, or activation older than 5s, is user_activation_required and takes no slot', () => {
    const { h, t } = wired(null)
    for (let i = 0; i < 5; i++) {
      t.arrive(t.msg('return_to_chat_intent', { citations: [REF] }))
      expect(t.last()?.payload).toEqual({ result: 'rejected', code: 'forbidden', reason: 'user_activation_required' })
    }
    expect(t.closes()).toBe(0)
    t.arrive(t.msg('status', { state: 'ready' }))
    expect(t.last()?.payload).toEqual({ result: 'done' })
    h.setActivated(t.time() - INTENT_ACTIVATION_MS - 1)
    t.arrive(t.msg('return_to_chat_intent', { citations: [REF] }))
    expect(t.last()?.payload).toEqual({ result: 'rejected', code: 'forbidden', reason: 'user_activation_required' })
    h.setActivated(t.time() - INTENT_ACTIVATION_MS)
    t.arrive(t.msg('return_to_chat_intent', { citations: [] }))
    expect(t.last()?.payload).toEqual({ result: 'accepted' })
  })

  it('no dialog could be shown (unavailable): not_ready, and no cooldown, since the user said nothing', async () => {
    const { h, t } = wired()
    const m = t.msg('return_to_chat_intent', { citations: [] })
    t.arrive(m)
    h.pendingConfirm[0]('unavailable')
    await flush()
    expect(forId(t, m.id).map((r) => r.payload)).toEqual([
      { result: 'accepted' },
      { result: 'rejected', code: 'unavailable', reason: 'not_ready' },
    ])
    h.setActivated(t.time())
    const next = t.msg('return_to_chat_intent', { citations: [] })
    t.arrive(next)
    expect(forId(t, next.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }])
  })

  it('a knowledge base that leaves the visible set during the validation read is not attached (not_found)', async () => {
    const { h, t } = wired()
    const m = t.msg('return_to_chat_intent', { citations: [REF] })
    t.arrive(m)
    h.pending[0](okRead())
    await flush()
    h.pendingConfirm[0]('confirmed')
    await flush()
    // The validation read is running; settings drop the knowledge base, then it returns ok.
    h.setScope([])
    h.pending[1](okRead())
    await flush()
    await flush()
    expect(forId(t, m.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }, { result: 'done', code: 'not_found' }])
    expect(h.attached).toEqual([])
  })

  it('a validation read that throws counts as internal for that item', async () => {
    const { h, t } = wired()
    let n = 0
    const read = h.citation.read
    h.citation.read = (g, r, s, v) => (n++ === 1 ? Promise.reject(new Error('boom')) : read(g, r, s, v))
    const m = t.msg('return_to_chat_intent', { citations: [REF] })
    t.arrive(m)
    h.pending[0](okRead())
    await flush()
    h.pendingConfirm[0]('confirmed')
    await flush()
    await flush()
    expect(forId(t, m.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }, { result: 'done', code: 'internal' }])
    expect(h.attached).toEqual([])
  })

  it('a decline starts a 30s cooldown, and the instant it ends is outside it', async () => {
    const { h, t } = wired()
    const m = t.msg('return_to_chat_intent', { citations: [] })
    t.arrive(m)
    h.pendingConfirm[0]('declined')
    await flush()
    expect(forId(t, m.id).map((r) => r.payload)).toEqual([
      { result: 'accepted' },
      { result: 'rejected', code: 'forbidden', reason: 'user_declined' },
    ])
    for (let i = 0; i < 5; i++) {
      h.setActivated(t.time())
      t.arrive(t.msg('return_to_chat_intent', { citations: [] }))
      expect(t.last()?.payload).toEqual({ result: 'rejected', code: 'forbidden', reason: 'cooldown' })
    }
    expect(t.closes()).toBe(0)
    t.advance(INTENT_COOLDOWN_MS - 1)
    h.setActivated(t.time())
    t.arrive(t.msg('return_to_chat_intent', { citations: [] }))
    expect(t.last()?.payload).toEqual({ result: 'rejected', code: 'forbidden', reason: 'cooldown' })
    t.advance(1)
    h.setActivated(t.time())
    t.arrive(t.msg('return_to_chat_intent', { citations: [] }))
    expect(t.last()?.payload).toEqual({ result: 'accepted' })
  })

  it('more citations than draft room is attachment_limit and takes no slot; an equal count is accepted', () => {
    const { h, t } = wired()
    h.setRoom(1)
    for (let i = 0; i < 5; i++) {
      t.arrive(t.msg('return_to_chat_intent', { citations: [cite('a'), cite('b')] }))
      expect(t.last()?.payload).toEqual({ result: 'rejected', code: 'invalid_request', reason: 'attachment_limit' })
    }
    expect(h.reads).toEqual([])
    expect(h.attached).toEqual([])
    expect(t.closes()).toBe(0)
    t.arrive(t.msg('return_to_chat_intent', { citations: [cite('c')] }))
    expect(t.last()?.payload).toEqual({ result: 'accepted' })
  })

  it('a second intent is ⑯ overloaded, is not remembered, and does not take a slot', () => {
    const { t } = wired()
    const first = t.msg('return_to_chat_intent', { citations: [REF] })
    t.arrive(first)
    expect(t.last()?.payload).toEqual({ result: 'accepted' })
    const second = t.msg('return_to_chat_intent', { citations: [cite('c-2')] })
    t.arrive(second)
    expect(forId(t, second.id)[0].payload).toEqual({ result: 'rejected', code: 'unavailable', reason: 'overloaded', retry_after_ms: BRIDGE_BACKOFF_MS })
    expect(t.logs.some((line) => line.includes('step=⑯') && line.includes('reason=overloaded') && line.includes(second.id))).toBe(true)
    const alongside = t.msg('citation', REF)
    t.arrive(alongside)
    expect(t.last()?.payload).toEqual({ result: 'accepted' })
    expect(t.last()?.in_reply_to).toBe(alongside.id)
    t.arrive(t.msg('cancel', { target_id: first.id }))
    t.arrive(second)
    expect(forId(t, second.id).at(-1)?.payload).toEqual({ result: 'accepted' })
  })

  it('an intent in flight occupies one of the four slots', () => {
    const { t } = wired()
    t.arrive(t.msg('return_to_chat_intent', { citations: [REF] }))
    for (let i = 0; i < 3; i++) {
      t.arrive(t.msg('citation', REF))
      expect(t.last()?.payload).toEqual({ result: 'accepted' })
    }
    const blocked = t.msg('citation', REF)
    t.arrive(blocked)
    expect(t.last()?.in_reply_to).toBe(blocked.id)
    expect(t.last()?.payload).toEqual({ result: 'rejected', code: 'unavailable', reason: 'overloaded', retry_after_ms: BRIDGE_BACKOFF_MS })
  })

  it('title reads skip anything outside the visible set and pass the rest through plainTitle', async () => {
    const { h, t, read } = wired()
    t.arrive(t.msg('return_to_chat_intent', { citations: [cite('c-out', 'kb-other'), cite('c-in')] }))
    expect(h.reads).toHaveLength(1)
    await read(okRead('discarded', '  Hello\u0000  '))
    expect(h.confirms[0].ask).toEqual({ count: 2, titles: [null, 'Hello'] })
    expect(h.reads).toHaveLength(1)
    expect(h.shown).toEqual([])
  })

  it('confirmation times out at 60s and not one millisecond earlier', async () => {
    const { h, t } = wired()
    const m = t.msg('return_to_chat_intent', { citations: [] })
    t.arrive(m)
    t.advance(INTENT_CONFIRM_MS - 1)
    expect(h.confirms[0].signal.aborted).toBe(false)
    expect(forId(t, m.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }])
    t.advance(1)
    await flush()
    expect(h.confirms[0].signal.aborted).toBe(true)
    expect(t.last()?.payload).toEqual({ result: 'rejected', code: 'forbidden', reason: 'confirmation_timeout' })
    expect(h.attached).toEqual([])
    h.setActivated(t.time())
    t.arrive(t.msg('return_to_chat_intent', { citations: [] }))
    expect(t.last()?.payload).toEqual({ result: 'accepted' })
  })

  it('confirmed with every citation passing is done with no code, and attach is called once with that set', async () => {
    const { h, t, read } = wired()
    const a = cite('c-a')
    const b = cite('c-b')
    const m = t.msg('return_to_chat_intent', { citations: [a, b] })
    t.arrive(m)
    await read(okRead('old-a', 'Old A'))
    await read(okRead('old-b', 'Old B'))
    h.pendingConfirm[0]('confirmed')
    await flush()
    await read(okRead('body-a', 'Kept A'))
    await read(okRead('body-b', 'Kept B'))
    expect(h.attached).toEqual([[
      { title: 'Kept A', text: 'body-a', ref: { knowledge_base_id: 'kb-1', knowledge_id: 'doc-1', chunk_id: 'c-a' } },
      { title: 'Kept B', text: 'body-b', ref: { knowledge_base_id: 'kb-1', knowledge_id: 'doc-1', chunk_id: 'c-b' } },
    ]])
    expect(forId(t, m.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }, { result: 'done' }])
    expect(h.shown).toEqual([])
  })

  it('a partial pass is done with the first failure code and only the passing items', async () => {
    const { h, t, read } = wired()
    const m = t.msg('return_to_chat_intent', { citations: [cite('c-a'), cite('c-b')] })
    t.arrive(m)
    await read(okRead('old-a', 'A'))
    await read(okRead('old-b', 'B'))
    h.pendingConfirm[0]('confirmed')
    await flush()
    await read({ ok: false, code: 'not_found', reason: 'missing' })
    await read(okRead('kept', 'Kept'))
    expect(h.attached).toEqual([[
      { title: 'Kept', text: 'kept', ref: { knowledge_base_id: 'kb-1', knowledge_id: 'doc-1', chunk_id: 'c-b' } },
    ]])
    expect(t.last()?.payload).toEqual({ result: 'done', code: 'not_found' })
  })

  it('#1624 C1: an item that passed but left the visible set during a later read is dropped, and its code comes first in citation order', async () => {
    const { h, t, read } = wired()
    h.setScope(['kb-1', 'kb-2'])
    const m = t.msg('return_to_chat_intent', { citations: [cite('c-a', 'kb-1'), cite('c-b', 'kb-2'), cite('c-c', 'kb-2')] })
    t.arrive(m)
    await read(okRead('old-a', 'A'))
    await read(okRead('old-b', 'B'))
    await read(okRead('old-c', 'C'))
    h.pendingConfirm[0]('confirmed')
    await flush()
    await read(okRead('body-a', 'A'))
    // While b's read runs, settings drop kb-1: a already passed, and must not be attached.
    h.setScope(['kb-2'])
    await read({ ok: false, code: 'stale', reason: 'moved' })
    await read(okRead('body-c', 'C'))
    expect(h.attached).toEqual([[
      { title: 'C', text: 'body-c', ref: { knowledge_base_id: 'kb-2', knowledge_id: 'doc-1', chunk_id: 'c-c' } },
    ]])
    expect(forId(t, m.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }, { result: 'done', code: 'not_found' }])
  })

  it('#1624 C4: a title read that waits out the bridge budget ends the intent overloaded, with no dialog and no cooldown', async () => {
    const { h, t } = wired()
    t.arrive(t.msg('citation', REF))
    t.arrive(t.msg('citation', REF))
    const m = t.msg('return_to_chat_intent', { citations: [REF] })
    t.arrive(m)
    expect(h.reads).toHaveLength(2)
    t.advance(10_000)
    await flush()
    expect(forId(t, m.id).map((r) => r.payload)).toEqual([
      { result: 'accepted' },
      { result: 'rejected', code: 'unavailable', reason: 'overloaded', retry_after_ms: BRIDGE_BACKOFF_MS },
    ])
    expect(h.confirms).toEqual([])
    expect(h.attached).toEqual([])
    h.setActivated(t.time())
    const next = t.msg('return_to_chat_intent', { citations: [] })
    t.arrive(next)
    expect(forId(t, next.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }])
  })

  it('#1624 C4: a validation read that waits out the bridge budget ends the intent overloaded, and nothing is attached', async () => {
    const { h, t, read } = wired()
    const m = t.msg('return_to_chat_intent', { citations: [cite('c-a'), cite('c-b')] })
    t.arrive(m)
    await read(okRead('old-a', 'A'))
    await read(okRead('old-b', 'B'))
    // Two citations take the budget's two slots before the user answers.
    t.arrive(t.msg('citation', REF))
    t.arrive(t.msg('citation', REF))
    h.pendingConfirm[0]('confirmed')
    await flush()
    expect(h.reads).toHaveLength(4)
    t.advance(10_000)
    await flush()
    expect(forId(t, m.id).map((r) => r.payload)).toEqual([
      { result: 'accepted' },
      { result: 'rejected', code: 'unavailable', reason: 'overloaded', retry_after_ms: BRIDGE_BACKOFF_MS },
    ])
    expect(h.attached).toEqual([])
    expect(h.reads).toHaveLength(4)
  })

  it('when nothing passes, done carries the first failure code and attach is never called', async () => {
    const { h, t, read } = wired()
    const m = t.msg('return_to_chat_intent', { citations: [cite('c-a'), cite('c-b')] })
    t.arrive(m)
    await read(okRead('old-a', 'A'))
    await read(okRead('old-b', 'B'))
    h.pendingConfirm[0]('confirmed')
    await flush()
    await read({ ok: false, code: 'stale', reason: 'moved' })
    await read({ ok: false, code: 'not_found' })
    expect(h.attached).toEqual([])
    expect(forId(t, m.id).at(-1)?.payload).toEqual({ result: 'done', code: 'stale' })
  })

  it('attach returning false is a terminal attachment_limit', async () => {
    const { h, t, read } = wired()
    h.setAllow(false)
    const m = t.msg('return_to_chat_intent', { citations: [REF] })
    t.arrive(m)
    await read(okRead('body', 'Title'))
    h.pendingConfirm[0]('confirmed')
    await flush()
    await read(okRead('body', 'Title'))
    expect(h.attached).toHaveLength(1)
    expect(forId(t, m.id).map((r) => r.payload)).toEqual([
      { result: 'accepted' },
      { result: 'rejected', code: 'invalid_request', reason: 'attachment_limit' },
    ])
  })

  it('zero citations are done after confirmation, with no read and no attach', async () => {
    const { h, t } = wired()
    const m = t.msg('return_to_chat_intent', { citations: [] })
    t.arrive(m)
    expect(h.reads).toEqual([])
    expect(h.confirms[0].ask).toEqual({ count: 0, titles: [] })
    h.pendingConfirm[0]('confirmed')
    await flush()
    expect(forId(t, m.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }, { result: 'done' }])
    expect(h.attached).toEqual([])
    expect(h.shown).toEqual([])
  })

  it('cancel before attach is cancelled, aborts the dialog, and attaches nothing', async () => {
    const { h, t, read } = wired()
    const m = t.msg('return_to_chat_intent', { citations: [REF] })
    t.arrive(m)
    await read(okRead('discard', 'Title'))
    const cancel = t.msg('cancel', { target_id: m.id })
    t.arrive(cancel)
    expect(h.confirms[0].signal.aborted).toBe(true)
    expect(h.attached).toEqual([])
    expect(t.replies().slice(-3).map((r) => [r.in_reply_to, r.payload])).toEqual([
      [m.id, { result: 'accepted' }],
      [m.id, { result: 'rejected', code: 'cancelled' }],
      [cancel.id, { result: 'accepted' }],
    ])
    h.pendingConfirm[0]('confirmed')
    await flush()
    expect(h.attached).toEqual([])
    expect(forId(t, m.id).map((r) => r.payload)).toEqual([
      { result: 'accepted' },
      { result: 'rejected', code: 'cancelled' },
    ])
  })

  it('cancel after attach only accepts', async () => {
    const { h, t, read } = wired()
    const m = t.msg('return_to_chat_intent', { citations: [REF] })
    t.arrive(m)
    await read(okRead('old', 'Old'))
    h.pendingConfirm[0]('confirmed')
    await flush()
    await read(okRead('body', 'Kept'))
    expect(h.attached).toHaveLength(1)
    const cancel = t.msg('cancel', { target_id: m.id })
    t.arrive(cancel)
    expect(forId(t, m.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }, { result: 'done' }])
    expect(t.last()?.in_reply_to).toBe(cancel.id)
    expect(t.last()?.payload).toEqual({ result: 'accepted' })
  })

  it('a document change while waiting sends no reply and aborts the dialog', async () => {
    const { h, t } = wired()
    const m = t.msg('return_to_chat_intent', { citations: [] })
    t.arrive(m)
    t.bridge.newDocument(2)
    expect(h.confirms[0].signal.aborted).toBe(true)
    await flush()
    expect(forId(t, m.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }])
    expect(h.attached).toEqual([])
  })

  it('destroy while waiting sends no reply and aborts the dialog', async () => {
    const { h, t } = wired()
    const m = t.msg('return_to_chat_intent', { citations: [] })
    t.arrive(m)
    const n = t.replies().length
    t.bridge.destroy()
    expect(h.confirms[0].signal.aborted).toBe(true)
    await flush()
    expect(t.replies()).toHaveLength(n)
    expect(forId(t, m.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }])
    expect(h.attached).toEqual([])
    expect(t.closes()).toBe(0)
  })

  it('admission closing while waiting is not_ready, and not before the first poll', async () => {
    const { h, t } = wired()
    const m = t.msg('return_to_chat_intent', { citations: [] })
    t.arrive(m)
    t.admission.open = false
    t.advance(INTENT_ADMISSION_POLL_MS - 1)
    expect(h.confirms[0].signal.aborted).toBe(false)
    expect(forId(t, m.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }])
    t.advance(1)
    await flush()
    expect(h.confirms[0].signal.aborted).toBe(true)
    expect(t.last()?.payload).toEqual({ result: 'rejected', code: 'unavailable', reason: 'not_ready' })
    expect(h.attached).toEqual([])
  })

  it('title and validation reads use the generation captured at acceptance', async () => {
    const { h, t, read } = wired()
    const m = t.msg('return_to_chat_intent', { citations: [REF] })
    t.arrive(m)
    expect(h.reads.map((r) => r.generation)).toEqual([5])
    t.admission.generation = 9
    await read(okRead('old', 'Old'))
    h.pendingConfirm[0]('confirmed')
    await flush()
    await read(okRead('body', 'Kept'))
    expect(h.reads.map((r) => r.generation)).toEqual([5, 5])
    expect(h.attached).toHaveLength(1)
    expect(forId(t, m.id).at(-1)?.payload).toEqual({ result: 'done' })
  })

  it('a terminal reply that finishes while away is delivered on resume with the original document_id', async () => {
    const { h, t } = wired()
    const doc = t.bridge.currentDocumentId()
    const m = t.msg('return_to_chat_intent', { citations: [] })
    t.arrive(m)
    t.bridge.leaveDocument()
    h.pendingConfirm[0]('declined')
    await flush()
    expect(forId(t, m.id).map((r) => r.payload)).toEqual([{ result: 'accepted' }])
    t.bridge.resumeDocument(1)
    expect(forId(t, m.id).map((r) => r.payload)).toEqual([
      { result: 'accepted' },
      { result: 'rejected', code: 'forbidden', reason: 'user_declined' },
    ])
    expect(forId(t, m.id)[1].document_id).toBe(doc)
  })
})
