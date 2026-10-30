import { describe, it, expect, vi } from 'vitest'
import { askWithKnowledge, createChatSession, MAX_WAIT_MS } from './chat-knowledge'

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
    const { calls, proxy } = proxyWith([{ status: 'running' }, { status: 'completed', output: { text: '答案 [来源]' } }])
    const turn = await askWithKnowledge('报销流程？', 'sess_1', { proxy, sleep: async () => {} })
    expect(turn).toEqual({ kind: 'answer', text: '答案 [来源]' })
    expect(calls[0]).toEqual({ method: 'POST', path: '/api/v1/runs', body: { prompt: '报销流程？', session_id: 'sess_1' } })
    expect(calls.filter((c) => c.method === 'GET').every((c) => c.path === '/api/v1/runs/run_1')).toBe(true)
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

    let t = 0
    const slow = proxyWith([{ status: 'running' }])
    const turn = await askWithKnowledge('q', 's', { proxy: slow.proxy, sleep: async (ms) => { t += ms }, now: () => t * (MAX_WAIT_MS / 3000) })
    expect(turn).toMatchObject({ kind: 'failed', code: 'run_timeout' })
    expect(slow.calls.at(-1)).toEqual({ method: 'POST', path: '/api/v1/runs/run_1/cancel' })
  })
})
