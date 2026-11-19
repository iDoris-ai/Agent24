import { describe, expect, it, vi } from 'vitest'
import { KnowledgeReachability, stateOfStatus, type ProbeInputs } from './knowledge-reachability'

const INPUTS: ProbeInputs = { endpoint: 'http://127.0.0.1:18282/api/v1/', knowledgeBaseId: 'kb 1', key: 'k-secret' }
const answer = (status: number) => vi.fn(async () => new Response('{}', { status }))

describe('KnowledgeReachability (K2)', () => {
  it('reads the server answer as a state', () => {
    expect(stateOfStatus(200)).toBe('ok')
    expect(stateOfStatus(401)).toBe('unauthorized')
    expect(stateOfStatus(403)).toBe('unauthorized')
    expect(stateOfStatus(404)).toBe('not_found')
    expect(stateOfStatus(502)).toBe('unreachable')
  })

  it('asks for the first knowledge base with the key, and says ok', async () => {
    const fetch = answer(200)
    const r = new KnowledgeReachability({ inputs: () => INPUTS, fetch, now: () => new Date('2026-11-19T08:00:00Z') })
    expect(await r.check()).toEqual({ state: 'ok', checkedAt: '2026-11-19T08:00:00.000Z' })
    const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit]
    expect(url).toBe('http://127.0.0.1:18282/api/v1/knowledge-bases/kb%201')
    expect((init.headers as Record<string, string>)['X-API-Key']).toBe('k-secret')
    expect(init.redirect).toBe('error')
  })

  it('a server that does not answer is unreachable; a refused key is unauthorized', async () => {
    const down = new KnowledgeReachability({ inputs: () => INPUTS, fetch: vi.fn(async () => { throw new TypeError('fetch failed') }) })
    expect((await down.check()).state).toBe('unreachable')
    const refused = new KnowledgeReachability({ inputs: () => INPUTS, fetch: answer(401) })
    expect((await refused.check()).state).toBe('unauthorized')
  })

  it('off or incomplete is unknown, and nothing is asked', async () => {
    const fetch = answer(200)
    const r = new KnowledgeReachability({ inputs: () => null, fetch })
    expect(await r.check()).toEqual({ state: 'unknown', checkedAt: null })
    expect(fetch).not.toHaveBeenCalled()
  })

  it('tells listeners when the state changes, not on every check; checks under way are shared', async () => {
    let status = 200
    const fetch = vi.fn(async () => new Response('{}', { status }))
    const r = new KnowledgeReachability({ inputs: () => INPUTS, fetch })
    const seen: string[] = []
    r.onChange((v) => seen.push(v.state))
    await Promise.all([r.check(), r.check()])
    expect(fetch).toHaveBeenCalledTimes(1)
    await r.check()
    status = 503
    await r.check()
    status = 200
    await r.check()
    expect(seen).toEqual(['ok', 'unreachable', 'ok'])
  })
})
