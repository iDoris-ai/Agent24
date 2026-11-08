import { describe, expect, it } from 'vitest'
import {
  BRIDGE_BACKOFF_MS,
  BRIDGE_BURST,
  BRIDGE_DEDUPE_LIMIT,
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

function setup(opts: { open?: boolean; generation?: number } = {}) {
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
  })
  bridge.newDocument(1)
  const arrive = (body: unknown, over: Partial<BridgeArrival> = {}): void => {
    const bytes = typeof body === 'string' ? Buffer.from(body) : body instanceof Uint8Array ? body : Buffer.from(JSON.stringify(body))
    bridge.receive({ fromView: true, mainFrame: true, documentOrigin: ORIGIN, document: 1, bytes, senderKey: 's', ...over })
  }
  const msg = (type: string, payload: unknown, extra: Record<string, unknown> = {}) =>
    ({ v: 2, direction: 'page_to_host', type, id: ulid(), generation: admission.generation, payload, ...extra })
  const replies = () => sent.filter((s) => s.msg.type === 'reply').map((s) => s.msg)
  const last = () => replies().at(-1)
  const advance = (ms: number): void => {
    now += ms
    for (const t of [...timers].sort((a, b) => a.at - b.at)) {
      if (t.at <= now) { timers.splice(timers.indexOf(t), 1); t.fn() }
    }
  }
  return { bridge, arrive, msg, sent, replies, last, logs, admission, closes: () => closes, advance, setNow: (n: number) => { now = n } }
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
    expect(t.sent[0].msg).toMatchObject({ v: 2, direction: 'host_to_page', generation: 5, payload: { supported: [2], view_instance_id: 'V'.repeat(22) } })
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
    const m = t.msg('status', { state: 'ready' }, { v: 3 })
    t.arrive(m)
    expect(t.last()).toMatchObject({ in_reply_to: m.id, payload: { reason: 'unsupported_protocol_version', supported: [2] } })
    // 4 more → 1 soft violation; 4 more soft ones from ⑫ close it, one fewer does not.
    for (let i = 0; i < 4; i++) t.arrive(t.msg('status', { state: 'ready' }, { v: 3 }))
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
    t.arrive(m, { document: 2 })
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
