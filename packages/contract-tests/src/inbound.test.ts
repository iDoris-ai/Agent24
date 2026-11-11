// X11-2f inbound reads and host-only writes.
// The node mock has no inbound routes, so the whole file skips unless
// A24_TARGET is the Rust daemon. That daemon's contract job is legacy mode:
// writes must be the host-only 403, and reads return the empty shapes.

import { describe, expect, it } from 'vitest'
import WsClient from 'ws'
import { BASE_URL, TOKEN, get, post, request } from './helpers.js'

const POLICY_VERSION = /^[0-9a-f]{64}$/

const IS_RUST = (process.env['A24_TARGET'] || 'node') !== 'node'
const WS_URL = `${BASE_URL.replace(/^http/, 'ws')}/api/v1/events`

describe('inbound allowlist, policy, and event list (X11-2f)', () => {
  it('legacy token cannot write the allowlist or the policy', async (ctx) => {
    if (!IS_RUST) return ctx.skip()
    const cases = [
      ['/api/v1/inbound/allowlist', { entries: [{ source: 'hyphae', actor: 'npub1' }] }],
      ['/api/v1/inbound/policy', { accept_mode: 'auto' }],
    ] as const
    for (const [path, body] of cases) {
      const res = await request('PUT', path, body)
      expect(res.status, JSON.stringify(res.body)).toBe(403)
      const err = (res.body as { error: { code: string; message: string } }).error
      expect(err.code).toBe('forbidden')
      expect(err.message).toBe('Operation not allowed in this auth mode')
    }
  })

  it('legacy token reads an empty allowlist, observe, and a metadata-only page', async (ctx) => {
    if (!IS_RUST) return ctx.skip()
    const allow = await get('/api/v1/inbound/allowlist')
    expect(allow.status, JSON.stringify(allow.body)).toBe(200)
    const allowBody = allow.body as { entries: unknown[]; policy_version: string }
    expect(allowBody.entries).toEqual([])
    expect(allowBody.policy_version).toMatch(POLICY_VERSION)

    const policy = await get('/api/v1/inbound/policy')
    expect(policy.status, JSON.stringify(policy.body)).toBe(200)
    const policyBody = policy.body as {
      accept_mode: string
      updated_at: string | null
      policy_version: string
    }
    expect(policyBody.accept_mode).toBe('observe')
    expect(policyBody.updated_at).toBeNull()
    expect(policyBody.policy_version).toMatch(POLICY_VERSION)

    const events = await get('/api/v1/inbound/events')
    expect(events.status, JSON.stringify(events.body)).toBe(200)
    const page = events.body as { events: Array<Record<string, unknown>> }
    expect(Array.isArray(page.events)).toBe(true)
    for (const row of page.events) {
      expect(row).not.toHaveProperty('text')
      expect(row).not.toHaveProperty('meta')
      const preview = row['text_preview']
      if (typeof preview === 'string') expect([...preview].length).toBeLessThanOrEqual(50)
    }

    const bad = await get('/api/v1/inbound/events?limit=101')
    expect(bad.status).toBe(400)
    expect((bad.body as { error: { code: string } }).error.code).toBe('invalid_request')
  })

  it('reads do not publish inbound content on the events websocket', async (ctx) => {
    if (!IS_RUST) return ctx.skip()
    const frames: string[] = []
    const ws = new WsClient(WS_URL, {
      headers: TOKEN ? { Authorization: `Bearer ${TOKEN}` } : {},
    })
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`WS connect timed out: ${WS_URL}`)), 5_000)
      ws.on('open', () => {
        clearTimeout(timer)
        resolve()
      })
      ws.on('error', (err) => {
        clearTimeout(timer)
        reject(err)
      })
    })
    ws.on('message', (data) => {
      frames.push(String(data))
    })
    // Positive control: a dead socket would also satisfy the negative check.
    const started = new Promise<string>((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new Error('no run.started on the events websocket')),
        8_000,
      )
      const onMessage = (data: unknown) => {
        const text = String(data)
        if (text.includes('"run.started"')) {
          clearTimeout(timer)
          ws.off('message', onMessage)
          resolve(text)
        }
      }
      ws.on('message', onMessage)
    })
    const chat = post('/api/v1/chat', { messages: [{ role: 'user', content: 'ping' }] })
    const proof = await started
    expect(proof).toContain('run.started')
    await chat
    const mark = frames.length
    await get('/api/v1/inbound/allowlist')
    await get('/api/v1/inbound/policy')
    await get('/api/v1/inbound/events')
    await new Promise((resolve) => setTimeout(resolve, 400))
    ws.close()
    const tail = frames.slice(mark).join('\n')
    expect(tail).not.toContain('text_preview')
    expect(tail).not.toContain('/inbound/')
  })
})
