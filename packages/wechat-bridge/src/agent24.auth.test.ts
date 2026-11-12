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

  // Without the status check a 5xx body would be parsed as a record with no
  // `outcome`, and the bridge would drop the failure instead of answering
  // 「记录失败」.
  for (const status of [400, 401, 404, 413, 500]) {
    it(`HTTP ${status} rejects instead of returning a record`, async () => {
      vi.stubGlobal(
        'fetch',
        vi.fn(async () => new Response(JSON.stringify({ error: { message: 'SECRET' } }), { status })),
      )
      const c = new Agent24Client({ base: 'http://127.0.0.1:1', token: 'b' })
      let thrown: unknown
      try {
        await c.recordInbound(event)
      } catch (err) {
        thrown = err
      }
      expect(thrown).toBeInstanceOf(Error)
      const message = thrown instanceof Error ? thrown.message : String(thrown)
      expect(message).toMatch(new RegExp(`HTTP ${status}`))
      expect(message).not.toContain('SECRET')
    })
  }
})
