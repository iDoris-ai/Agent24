import { describe, it, expect, vi } from 'vitest'
import { askWithKnowledge, createChatSession, KNOWLEDGE_HINT, knowledgePrompt, knowledgeQuery, MAX_WAIT_MS } from './chat-knowledge'

type Req = { method: string; path: string; body?: unknown }
const ok = (data: unknown) => ({ ok: true, status: 200, data })

function proxyWith(statuses: unknown[]) {
  const calls: Req[] = []
  let i = 0
  const proxy = vi.fn(async (req: Req) => {
    calls.push(req)
    if (req.method === 'POST' && req.path === '/api/v1/runs') return ok({ id: 'run_1' })
    if (req.method === 'POST' && req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: 'sess_1' } }
    if (req.method === 'GET') return ok(statuses[Math.min(i++, statuses.length - 1)])
    return ok({})
  })
  return { calls, proxy: proxy as unknown as Window['agent24']['backendProxy'] }
}

describe('chat-knowledge', () => {
  it('creates a real session for the conversation', async () => {
    const { calls, proxy } = proxyWith([])
    expect(await createChatSession(proxy)).toBe('sess_1')
    expect(calls[0]).toEqual({ method: 'POST', path: '/api/v1/sessions', body: { title: '', channel: 'desktop' } })
  })

  it('asks as a run in the session, naming no binding, and waits for the answer', async () => {
    const knowledge = { searched: true, read_chunks: [{ knowledge_id: 'doc-7', chunk_id: 'chk-71' }] }
    const { calls, proxy } = proxyWith([{ status: 'running' }, { status: 'completed', output: { text: '答案 [来源]' }, knowledge }])
    const turn = await askWithKnowledge('报销流程？', 'sess_1', { proxy, sleep: async () => {} })
    expect(turn).toEqual({ kind: 'answer', text: '答案 [来源]', knowledge, runId: 'run_1', documents: null })
    expect(calls[0]).toEqual({ method: 'POST', path: '/api/v1/runs', body: { prompt: '报销流程？', session_id: 'sess_1' } })
    expect(calls.filter((c) => c.method === 'GET').every((c) => c.path === '/api/v1/runs/run_1')).toBe(true)
  })

  it('sends the iDoris headroom confirmation only when the user gave it', async () => {
    const plain = proxyWith([{ status: 'completed', output: { text: 'x' } }])
    await askWithKnowledge('q', 's', { proxy: plain.proxy, sleep: async () => {} })
    expect(plain.calls[0]?.body).not.toHaveProperty('idoris_headroom_confirmed')
    const confirmed = proxyWith([{ status: 'completed', output: { text: 'x' } }])
    await askWithKnowledge('q', 's', { proxy: confirmed.proxy, sleep: async () => {} }, true)
    expect(confirmed.calls[0]?.body).toEqual({ prompt: 'q', session_id: 's', idoris_headroom_confirmed: true })
  })

  it('reports an approval wait once, then its end', async () => {
    const seen: boolean[] = []
    const { proxy } = proxyWith([{ status: 'awaiting_approval' }, { status: 'awaiting_approval' }, { status: 'completed', output: { text: 'x' } }])
    await askWithKnowledge('q', 's', { proxy, sleep: async () => {}, onAwaitingApproval: (w) => seen.push(w) })
    expect(seen).toEqual([true, false])
  })

  it('returns the run failure and cancels a run it gives up on', async () => {
    const failed = proxyWith([{ status: 'failed', error: { code: 'model_unavailable', message: 'no model' } }])
    expect(await askWithKnowledge('q', 's', { proxy: failed.proxy, sleep: async () => {} }))
      .toEqual({ kind: 'failed', code: 'model_unavailable', message: 'no model' })

    // M4 A4: a run it never saw is not left running unseen.
    let t = 0
    const unseen = proxyWith([])
    const lost = vi.fn(async (req: Req) => (req.method === 'GET' ? { ok: false, status: 503, data: null } : (unseen.proxy as unknown as (r: Req) => Promise<unknown>)(req)))
    const turn = await askWithKnowledge('q', 's', { proxy: lost as unknown as Window['agent24']['backendProxy'], sleep: async (ms) => { t += ms }, now: () => t * (MAX_WAIT_MS / 3000) })
    expect(turn).toMatchObject({ kind: 'failed', code: 'run_timeout' })
    expect(lost.mock.calls.at(-1)?.[0]).toEqual({ method: 'POST', path: '/api/v1/runs/run_1/cancel' })
  })

  it.each(['queued', 'running', 'awaiting_approval'])('M4 A4: leaves a run still %s to go on when it stops waiting, as the conversation\'s task', async status => {
    let t = 0
    const slow = proxyWith([{ status }])
    const turn = await askWithKnowledge('q', 's', { proxy: slow.proxy, sleep: async (ms) => { t += ms }, now: () => t * (MAX_WAIT_MS / 3000) })
    expect(turn).toEqual({ kind: 'ongoing', runId: 'run_1', status })
    expect(slow.calls.some(c => c.path.endsWith('/cancel'))).toBe(false)
  })
})

describe('knowledgePrompt (M1, 0.6.72: 「烧开水要多久」 never searched)', () => {
  it('says to search the knowledge bases first, in front of a plain question', () => {
    expect(knowledgePrompt('烧开水要多久')).toBe(`${KNOWLEDGE_HINT}\n烧开水要多久`)
  })
  it('leaves 「记住…」 as said, so personal memory still takes it', () => {
    for (const text of ['记住我家的猫叫豆豆，今年三岁。', '记住：我每周三下午去游泳', '请帮我记一下明天交报告', '你记着我不吃香菜']) {
      expect(knowledgePrompt(text)).toBe(text)
    }
  })
})

describe('knowledgeQuery (M1 retrieval first, David 2026-11-21)', () => {
  it('is the user\'s own words, and none on a 「记住…」 turn', () => {
    expect(knowledgeQuery(' 烧开水要多久 ')).toBe('烧开水要多久')
    expect(knowledgeQuery('记住：我最喜欢的水果是芒果。')).toBeNull()
    expect(knowledgeQuery('   ')).toBeNull()
  })
  it('none when asked to write something new (General, 0.6.75)', () => {
    for (const text of ['写一篇八百字散文', '帮我写一段公司介绍', '请帮我起草一封邮件', '给我写首诗']) expect(knowledgeQuery(text)).toBeNull()
    for (const text of ['烧开水要多久', '高血压每天吃多少盐', '这份合同的押金是多少']) expect(knowledgeQuery(text)).toBe(text)
  })
  it('goes on the run as knowledge_query', async () => {
    const proxy = vi.fn()
      .mockResolvedValueOnce({ ok: true, status: 201, data: { id: 'r1' } })
      .mockResolvedValue({ ok: true, status: 200, data: { status: 'completed', output: { text: 'ok' } } })
    await askWithKnowledge('p', 's1', { proxy, sleep: async () => {} }, false, null, '烧开水要多久')
    expect(proxy.mock.calls[0][0].body).toEqual({ prompt: 'p', session_id: 's1', knowledge_query: '烧开水要多久' })
  })
})
