import http from 'node:http'
import type { AddressInfo } from 'node:net'
import { afterEach, describe, expect, it, vi } from 'vitest'

vi.mock('./backend-manager', () => ({ daemonConnection: () => null }))

import { MAX_ADMISSION_RUNS } from './knowledge-entry-admission'
import { daemonFloorReader, floorPath, parseFloor, testing } from './knowledge-entry-floor'

const conn = { port: 1, authHeaders: { Authorization: 'Bearer t' } }

describe('floorPath', () => {
  it('lists every run id, encoded, and the session id as one segment', () => {
    expect(floorPath('chat_abc', [])).toBe('/api/v1/sessions/chat_abc/source-floor')
    expect(floorPath('a/b', ['run_1', 'r&2'])).toBe('/api/v1/sessions/a%2Fb/source-floor?run_id=run_1&run_id=r%262')
  })
})

describe('parseFloor', () => {
  it('reads each of the four words', () => {
    for (const word of ['ordinary', 'restricted', 'unknown', 'missing']) {
      expect(parseFloor({ status: 200, body: JSON.stringify({ classification: word }) })).toBe(word)
    }
  })

  it('throws on any other answer', () => {
    const bad = [
      { status: 404, body: '{"classification":"ordinary"}' },
      { status: 200, body: '{"classification":"public"}' },
      { status: 200, body: '{"classification":"ordinary","extra":1}' },
      { status: 200, body: '["ordinary"]' },
      { status: 200, body: 'null' },
      { status: 200, body: 'ordinary' },
      { status: 200, body: '' },
    ]
    for (const response of bad) expect(() => parseFloor(response), response.body).toThrow()
  })
})

describe('daemonFloorReader', () => {
  it('asks the daemon on the current connection and returns its word', async () => {
    const transport = vi.fn(async () => ({ status: 200, body: '{"classification":"restricted"}' }))
    const reader = daemonFloorReader({ connection: () => conn, transport })
    expect(await reader.read('chat_abc', ['run_1'])).toBe('restricted')
    expect(transport).toHaveBeenCalledWith(conn, '/api/v1/sessions/chat_abc/source-floor?run_id=run_1')
  })

  it('throws without a daemon connection, and never asks about more runs than the limit', async () => {
    const transport = vi.fn(async () => ({ status: 200, body: '{"classification":"ordinary"}' }))
    await expect(daemonFloorReader({ connection: () => null, transport }).read('c', [])).rejects.toThrow()
    const many = Array.from({ length: MAX_ADMISSION_RUNS + 1 }, (_, i) => `run_${i}`)
    expect(await daemonFloorReader({ connection: () => conn, transport }).read('c', many)).toBe('unknown')
    expect(transport).not.toHaveBeenCalled()
  })
})

describe('the loopback transport', () => {
  let server: http.Server | null = null
  afterEach(async () => {
    await new Promise<void>((resolve) => (server ? server.close(() => resolve()) : resolve()))
    server = null
  })

  async function serve(handler: http.RequestListener) {
    server = http.createServer(handler)
    await new Promise<void>((resolve) => server!.listen(0, '127.0.0.1', resolve))
    return { port: (server.address() as AddressInfo).port, authHeaders: { Authorization: 'Bearer t' } }
  }

  it('sends a GET with the auth header and returns status and body', async () => {
    const seen: Array<{ method?: string; url?: string; auth?: string }> = []
    const c = await serve((req, res) => {
      seen.push({ method: req.method, url: req.url, auth: req.headers.authorization })
      res.writeHead(200, { 'Content-Type': 'application/json' }).end('{"classification":"ordinary"}')
    })
    expect(await testing.httpTransport(c, '/api/v1/sessions/x/source-floor')).toEqual({ status: 200, body: '{"classification":"ordinary"}' })
    expect(seen).toEqual([{ method: 'GET', url: '/api/v1/sessions/x/source-floor', auth: 'Bearer t' }])
  })

  it('refuses an answer longer than one small object', async () => {
    const c = await serve((_req, res) => {
      res.writeHead(200).end(`{"classification":"ordinary","pad":"${'x'.repeat(4096)}"}`)
    })
    await expect(testing.httpTransport(c, '/p')).rejects.toThrow()
  })
})
