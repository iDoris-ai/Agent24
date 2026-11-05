import { afterEach, describe, expect, it, vi } from 'vitest'
import { Agent24Client } from './agent24.js'

describe('Agent24Client without a bearer sends nothing', () => {
  afterEach(() => vi.unstubAllGlobals())

  for (const token of ['', undefined as unknown as string]) {
    it(`token=${JSON.stringify(token)}: every call refuses before fetch`, async () => {
      const fetchSpy = vi.fn()
      vi.stubGlobal('fetch', fetchSpy)
      const c = new Agent24Client({ base: 'http://127.0.0.1:1', token })
      await expect(c.createSession('t')).rejects.toThrow(/bearer/)
      await expect(c.runToCompletion('hi')).rejects.toThrow(/bearer/)
      await expect(c.awaitRun('r1')).rejects.toThrow(/bearer/)
      expect(fetchSpy).not.toHaveBeenCalled()
    })
  }

  it('with a bearer it sends Authorization', async () => {
    const fetchSpy = vi.fn(async () => new Response(JSON.stringify({ id: 's1' }), { status: 200 }))
    vi.stubGlobal('fetch', fetchSpy)
    await new Agent24Client({ base: 'http://127.0.0.1:1', token: 'b' }).createSession('t')
    const init = (fetchSpy.mock.calls[0] as unknown as [string, RequestInit])[1]
    expect((init.headers as Record<string, string>).Authorization).toBe('Bearer b')
  })
})
