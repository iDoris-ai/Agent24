import http from 'node:http'
import { describe, expect, it } from 'vitest'
import { isAllowedProxyRoute, proxyRequest } from './backend-proxy'

describe('IL-13 exact daemon proxy capability', () => {
  it('allows only implemented lifecycle methods and paths', () => {
    for (const [method, path] of [
      ['GET', '/api/v1/idoris/status'], ['GET', '/api/v1/idoris/omlx/candidates'],
      ['GET', '/api/v1/idoris/operations/op-1'], ['POST', '/api/v1/idoris/operations/op-1/cancel'],
      ...['install', 'connect', 'start', 'stop'].map(action => ['POST', `/api/v1/idoris/${action}`]),
    ]) expect(isAllowedProxyRoute(method, path), `${method} ${path}`).toBe(true)
    for (const [method, path] of [
      ['POST', '/api/v1/idoris/status'], ['GET', '/api/v1/idoris/connect'],
      ['POST', '/api/v1/idoris/upgrade'], ['POST', '/api/v1/idoris/keys/rotate'],
      ['GET', '/admin/api/v1/status'], ['POST', '/api/v1/idoris/operations/%2e%2e/cancel'],
    ]) expect(isAllowedProxyRoute(method, path), `${method} ${path}`).toBe(false)
  })
  it('forwards Idempotency-Key over HTTP without allowing renderer authentication headers', async () => {
    let key: string | string[] | undefined
    let authorization: string | undefined
    const server = http.createServer((req, res) => { key = req.headers['idempotency-key']; authorization = req.headers.authorization; res.end('{}') })
    await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve))
    try {
      const address = server.address()
      if (!address || typeof address === 'string') throw new Error('Missing test listener')
      await proxyRequest({ port: address.port, authHeaders: { Authorization: 'Bearer host' } }, { method: 'POST', path: '/api/v1/idoris/install', body: { consent: true }, idempotencyKey: 'il13-test-key' })
      expect(key).toBe('il13-test-key')
      expect(authorization).toBe('Bearer host')
    } finally { await new Promise<void>(resolve => server.close(() => resolve())) }
  })
})

describe('IL-13 idempotency header boundary', () => {
  it.each(['', 'a\r\nAuthorization: injected', 'a'.repeat(129), 'with space', 123, null])('rejects invalid key %j before any socket is created', async idempotencyKey => {
    const request = { method: 'POST', path: '/api/v1/idoris/connect', idempotencyKey }
    // IPC input is untrusted even when the TypeScript caller lies about it.
    const result = await proxyRequest({ port: 1, authHeaders: {} }, request as never)
    expect(result).toMatchObject({ ok: false, status: 400, data: { error: { code: 'invalid_request' } } })
  })
})
