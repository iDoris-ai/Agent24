import { afterEach, describe, expect, it, vi } from 'vitest'
import { Agent24Client } from './agent24.js'

const event = { id: 'wechat:1', actor: 'alice', at: '2026-11-12T00:00:00Z', text: 'hi' }

describe('Agent24Client without a bearer sends nothing', () => {
  afterEach(() => vi.unstubAllGlobals())

  for (const token of ['', undefined as unknown as string]) {
    it(`token=${JSON.stringify(token)}: recordInbound refuses before fetch`, async () => {
      const fetchSpy = vi.fn()
      vi.stubGlobal('fetch', fetchSpy)
      const c = new Agent24Client({ base: 'http://127.0.0.1:1', token })
      await expect(c.recordInbound(event)).rejects.toThrow(/bearer/)
      expect(fetchSpy).not.toHaveBeenCalled()
    })
  }

  it('with a bearer it posts the inbound event', async () => {
    const fetchSpy = vi.fn(
      async () => new Response(JSON.stringify({ id: 'wechat:1', outcome: 'ignored' }), { status: 200 }),
    )
    vi.stubGlobal('fetch', fetchSpy)
    await new Agent24Client({ base: 'http://127.0.0.1:1', token: 'b' }).recordInbound(event)
    const [url, init] = fetchSpy.mock.calls[0] as unknown as [string, RequestInit]
    expect(url).toBe('http://127.0.0.1:1/api/v1/inbound/events')
    expect(init.method).toBe('POST')
    expect((init.headers as Record<string, string>).Authorization).toBe('Bearer b')
    expect(JSON.parse(String(init.body))).toEqual(event)
  })
})
