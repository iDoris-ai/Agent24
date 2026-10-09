import http from 'node:http'
import type { AddressInfo } from 'node:net'
import { afterEach, describe, expect, it } from 'vitest'
import { documentsCall, documentsRoute, httpSender, toResponse, type Route } from './documents'

const DOC = 'doc_01K74Z3QJ8V5N2W9RTX6YB4MCD'
const JOB = 'job_01K75A0B1C2D3E4F5G6H7J8K9M'

describe('documentsRoute', () => {
  it('maps each allowed operation to its fixed route', () => {
    const cases: Array<[unknown, Route]> = [
      [{ op: 'capabilities' }, { method: 'GET', path: '/api/v1/documents/capabilities' }],
      [{ op: 'list' }, { method: 'GET', path: '/api/v1/documents/documents' }],
      [{ op: 'list', cursor: 'ab12', limit: 20 }, { method: 'GET', path: '/api/v1/documents/documents?cursor=ab12&limit=20' }],
      [{ op: 'get', documentId: DOC }, { method: 'GET', path: `/api/v1/documents/documents/${DOC}` }],
      [{ op: 'job', jobId: JOB }, { method: 'GET', path: `/api/v1/documents/jobs/${JOB}` }],
      [{ op: 'cancelJob', jobId: JOB }, { method: 'POST', path: `/api/v1/documents/jobs/${JOB}/cancel` }],
      [{ op: 'retryJob', jobId: JOB }, { method: 'POST', path: `/api/v1/documents/jobs/${JOB}/retry` }],
    ]
    for (const [req, route] of cases) expect(documentsRoute(req), JSON.stringify(req)).toEqual(route)
  })

  it('refuses anything else: no path ever comes from the renderer', () => {
    for (const req of [
      null,
      'list',
      { op: 'proxy', path: '/api/v1/runs' },
      { op: '__proto__' },
      { op: 'constructor' },
      { op: 'get', documentId: '../../runs' },
      { op: 'get', documentId: `${DOC}/../x` },
      { op: 'get', documentId: `${DOC}%2F..` },
      { op: 'get', documentId: 'doc_x' },
      // The first ULID character is 0–7, and I, L, O, U are not Crockford.
      { op: 'get', documentId: 'doc_81K74Z3QJ8V5N2W9RTX6YB4MCD' },
      { op: 'get', documentId: 'doc_01K74Z3QJ8V5N2W9RTX6YB4MCI' },
      { op: 'get', documentId: 'doc_01k74z3qj8v5n2w9rtx6yb4mcd' },
      { op: 'get' },
      { op: 'job', jobId: DOC },
      { op: 'cancelJob', jobId: `${JOB}?x=1` },
      { op: 'list', cursor: 'not hex' },
      { op: 'list', cursor: 'AB12' },
      { op: 'list', cursor: 'ab&limit=1' },
      { op: 'list', cursor: '' },
      { op: 'list', cursor: 12 },
      { op: 'list', limit: 0 },
      { op: 'list', limit: 201 },
      { op: 'list', limit: 1.5 },
      { op: 'list', limit: '20' },
    ]) {
      expect(documentsRoute(req), JSON.stringify(req)).toBeNull()
    }
  })
})

describe('toResponse', () => {
  const reply = (status: number, body: string) => ({ status, body: Buffer.from(body) })
  it('a 2xx JSON body is the resource', () => {
    expect(toResponse(reply(200, '{"documents":[],"next_cursor":null}'))).toEqual({
      ok: true,
      status: 200,
      data: { documents: [], next_cursor: null },
    })
  })
  it('an error status carries its envelope', () => {
    expect(toResponse(reply(404, '{"error":{"code":"not_found","message":"no such job"}}'))).toEqual({
      ok: false,
      status: 404,
      error: { code: 'not_found', message: 'no such job' },
    })
  })
  it('anything else is invalid_response, never ok', () => {
    for (const [status, body] of [
      [200, 'not json'],
      [200, ''],
      [200, 'null'],
      [302, '{"documents":[]}'],
      [500, 'oops'],
      [503, '{"error":"text only"}'],
      [600, '{"error":{"code":"not_found","message":"x"}}'],
    ] as const) {
      expect(toResponse(reply(status, body)), `${status} ${body}`).toMatchObject({
        ok: false,
        status,
        error: { code: 'invalid_response' },
      })
    }
  })
})

describe('documentsCall', () => {
  it('sends an allowed operation and refuses the rest unsent', async () => {
    const sent: Route[] = []
    const send = async (route: Route) => {
      sent.push(route)
      return { status: 200, body: Buffer.from('{"documents":[],"next_cursor":null}') }
    }
    expect(await documentsCall({ op: 'list' }, send)).toEqual({ ok: true, status: 200, data: { documents: [], next_cursor: null } })
    const refused = await documentsCall({ op: 'get', documentId: '../x' }, send)
    expect(refused).toMatchObject({ ok: false, status: 400, error: { code: 'invalid_request' } })
    expect(sent).toEqual([{ method: 'GET', path: '/api/v1/documents/documents' }])
  })
})

describe('httpSender', () => {
  let server: http.Server | null = null
  afterEach(async () => {
    server?.closeAllConnections()
    await new Promise<void>((r) => (server ? server.close(() => r()) : r()))
    server = null
  })

  async function serve(handler: http.RequestListener): Promise<number> {
    server = http.createServer(handler)
    await new Promise<void>((r) => server!.listen(0, '127.0.0.1', () => r()))
    return (server!.address() as AddressInfo).port
  }

  it('sends the route with the daemon token and returns the whole reply', async () => {
    const seen: Array<{ method?: string; url?: string; auth?: string; type?: string; body: string }> = []
    const port = await serve((req, res) => {
      const chunks: Buffer[] = []
      req.on('data', (c: Buffer) => chunks.push(c))
      req.on('end', () => {
        seen.push({
          method: req.method,
          url: req.url,
          auth: req.headers['authorization'],
          type: req.headers['content-type'],
          body: Buffer.concat(chunks).toString(),
        })
        res.writeHead(404, { 'content-type': 'application/json' })
        res.end('{"error":{"code":"not_found","message":"no such job"}}')
      })
    })
    const send = httpSender(() => ({ port, token: 't0k' }))
    const reply = await send(
      { method: 'POST', path: `/api/v1/documents/jobs/${JOB}/cancel` },
      { headers: { 'Content-Type': 'application/octet-stream' }, body: Buffer.from([1, 2, 3]) },
    )
    expect(toResponse(reply)).toEqual({ ok: false, status: 404, error: { code: 'not_found', message: 'no such job' } })
    expect(seen).toEqual([
      {
        method: 'POST',
        url: `/api/v1/documents/jobs/${JOB}/cancel`,
        auth: 'Bearer t0k',
        type: 'application/octet-stream',
        body: '\u0001\u0002\u0003',
      },
    ])
  })

  it('a reply cut short settles as unreachable instead of hanging', async () => {
    const port = await serve((_req, res) => {
      res.writeHead(200, { 'content-type': 'application/json', 'content-length': '1000' })
      res.write('{"documents":[')
      setTimeout(() => res.socket?.destroy(), 20)
    })
    const reply = await httpSender(() => ({ port, token: '' }))({ method: 'GET', path: '/x' })
    expect(reply).toMatchObject({ status: 502, error: { code: 'backend_unreachable' } })
  })

  it('no reply in time is a timeout whose outcome is unknown', async () => {
    const port = await serve(() => {
      /* never answers */
    })
    const reply = await httpSender(() => ({ port, token: '' }), '127.0.0.1', 50)({ method: 'POST', path: '/x' })
    expect(reply).toMatchObject({ status: 504, error: { code: 'backend_timeout' } })
  })

  it('a reply past the byte cap is refused', async () => {
    const port = await serve((_req, res) => res.end('x'.repeat(4096)))
    const reply = await httpSender(() => ({ port, token: '' }), '127.0.0.1', 5000, 1024)({ method: 'GET', path: '/x' })
    expect(reply).toMatchObject({ status: 502, error: { code: 'invalid_response' } })
  })

  it('is 503 before the daemon is ready, and when it cannot be reached', async () => {
    const notReady = await httpSender(() => null)({ method: 'GET', path: '/x' })
    expect(notReady).toMatchObject({ status: 503, error: { code: 'backend_not_ready' } })
    const port = await serve((_req, res) => res.end())
    await new Promise<void>((r) => server!.close(() => r()))
    server = null
    const unreachable = await httpSender(() => ({ port, token: '' }))({ method: 'GET', path: '/x' })
    expect(unreachable).toMatchObject({ status: 503, error: { code: 'backend_unreachable' } })
  })
})
