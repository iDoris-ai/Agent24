import http from 'node:http'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { createIdorisStatusProvider, fetchIdorisOverview, fetchIdorisStatus, parseAdminPort, parseAdminTokenFd, parseBackendsResponse, parseModelsResponse, parseStatusResponse, parseTokenFrame, readAdminTokenFromFd } from './idoris-admin'

const TOKEN = 'a'.repeat(64)
const VALID_STATUS = {
  status: 'ok', service: 'idoris', version: '1.0.0', contract_version: 'admin-v0', instance_id: 'i-1',
  components: 1, runtimes: 2, subscriptions: 0, budget_configured: true, audit_configured: false,
  capacity: { state: 'observed', entries: [{ id: 'm1', capability: 'chat', resident: true, estimated_memory_gb: 3.5, ctx_limit: 4096, queue_depth: 0, admission_status: 'ready' }] },
} as const
const VALID_BACKENDS = [{ provider_id: 'omlx-local', locality: 'loopback', form: 'http_service', lifecycle_runtime_bound: true }] as const
const VALID_MODELS = { sources: [{ provider_id: 'omlx-local', source: 'http_models_endpoint', state: 'observed', models: ['qwen3-8b'] }] } as const

const servers: http.Server[] = []
afterEach(async () => {
  await Promise.all(servers.splice(0).map((server) => new Promise<void>((resolve) => server.close(() => resolve()))))
})

async function listen(handler: http.RequestListener): Promise<{ server: http.Server; port: number }> {
  const server = http.createServer(handler)
  servers.push(server)
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
  const address = server.address()
  if (!address || typeof address === 'string') throw new Error('missing test listener')
  return { server, port: address.port }
}

describe('iDoris Admin launch handoff', () => {
  it('accepts exactly one non-secret inherited fd selector', () => {
    expect(parseAdminTokenFd(['electron', TOKEN_ARG(), '3'])).toBe(3)
    expect(parseAdminTokenFd(['electron'])).toBe('missing')
    expect(parseAdminTokenFd(['electron', TOKEN_ARG(), '2'])).toBe('invalid')
    expect(parseAdminTokenFd(['electron', `${TOKEN_ARG()}=3`])).toBe('invalid')
    expect(parseAdminTokenFd(['electron', TOKEN_ARG(), '3', TOKEN_ARG(), '4'])).toBe('invalid')
  })

  it('accepts only the canonical 64-lowercase-hex newline frame', () => {
    expect(parseTokenFrame(Buffer.from(`${TOKEN}\n`))).toBe(TOKEN)
    expect(parseTokenFrame(Buffer.from(`${'A'.repeat(64)}\n`))).toBeNull()
    expect(parseTokenFrame(Buffer.from(TOKEN))).toBeNull()
    expect(parseTokenFrame(Buffer.from(`${TOKEN}\nextra`))).toBeNull()
  })

  it('reads once and closes the inherited fd before returning', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-idoris-token-'))
    const file = path.join(dir, 'frame')
    fs.writeFileSync(file, `${TOKEN}\n`, { mode: 0o600 })
    const fd = fs.openSync(file, 'r')
    try {
      expect(await readAdminTokenFromFd(fd, 250)).toBe(TOKEN)
      expect(() => fs.fstatSync(fd)).toThrow()
    } finally {
      fs.rmSync(dir, { recursive: true, force: true })
    }
  })

  it('closes every explicitly claimed candidate when duplicate selectors fail closed', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-idoris-invalid-fd-'))
    const first = fs.openSync(path.join(dir, 'first'), 'w+')
    const second = fs.openSync(path.join(dir, 'second'), 'w+')
    try {
      const provider = await createIdorisStatusProvider(
        ['electron', TOKEN_ARG(), String(first), TOKEN_ARG(), String(second)],
        {},
      )
      expect(await provider()).toEqual({ state: 'unavailable', reason: 'handoff-invalid' })
      expect(() => fs.fstatSync(first)).toThrow()
      expect(() => fs.fstatSync(second)).toThrow()
    } finally {
      fs.rmSync(dir, { recursive: true, force: true })
    }
  })

  it('parses only trusted non-secret Admin port configuration', () => {
    expect(parseAdminPort(undefined)).toBe(8741)
    expect(parseAdminPort('9443')).toBe(9443)
    expect(parseAdminPort('0')).toBeNull()
    expect(parseAdminPort('https://evil')).toBeNull()
  })
})

describe('iDoris Admin status transport', () => {
  it('projects only the frozen #409 status contract', () => {
    expect(parseStatusResponse(VALID_STATUS)).toEqual(VALID_STATUS)
    expect(parseStatusResponse({ ...VALID_STATUS, token: TOKEN })).toBeNull()
    expect(parseStatusResponse({ ...VALID_STATUS, capacity: { state: 'observed', entries: [{ ...VALID_STATUS.capacity.entries[0], estimated_memory_gb: -1 }] } })).toBeNull()
  })

  it('projects only the frozen backends/models contracts', () => {
    expect(parseBackendsResponse(VALID_BACKENDS)).toEqual(VALID_BACKENDS)
    expect(parseBackendsResponse([{ ...VALID_BACKENDS[0], endpoint: 'http://127.0.0.1:8088/v1' }])).toBeNull()
    expect(parseModelsResponse(VALID_MODELS)).toEqual(VALID_MODELS)
    expect(parseModelsResponse({ sources: [{ ...VALID_MODELS.sources[0], token: TOKEN }] })).toBeNull()
    expect(parseModelsResponse({ sources: [{ provider_id: 'sub', source: 'subscription_registration', state: 'configured', models: [] }] })).toBeNull()
  })

  it('reads exactly the three fixed Admin status-card resources', async () => {
    const seen = new Set<string>()
    const { port } = await listen((req, res) => {
      seen.add(req.url ?? '')
      expect(req.headers.authorization).toBe(`Bearer ${TOKEN}`)
      res.setHeader('content-type', 'application/json')
      if (req.url === '/admin/api/v1/status') res.end(JSON.stringify(VALID_STATUS))
      else if (req.url === '/admin/api/v1/backends') res.end(JSON.stringify(VALID_BACKENDS))
      else if (req.url === '/admin/api/v1/models') res.end(JSON.stringify(VALID_MODELS))
      else { res.statusCode = 404; res.end() }
    })
    const result = await fetchIdorisOverview(TOKEN, port)
    expect(result).toEqual({ state: 'available', status: VALID_STATUS, backends: VALID_BACKENDS, models: VALID_MODELS })
    expect([...seen].sort()).toEqual(['/admin/api/v1/backends', '/admin/api/v1/models', '/admin/api/v1/status'])
    expect(JSON.stringify(result)).not.toContain(TOKEN)
  })

  it('uses fixed loopback/status path and the main-only bearer header', async () => {
    let seenPath = ''
    let seenAuth = ''
    const { port } = await listen((req, res) => {
      seenPath = req.url ?? ''
      seenAuth = req.headers.authorization ?? ''
      res.setHeader('content-type', 'application/json')
      res.end(JSON.stringify(VALID_STATUS))
    })
    const result = await fetchIdorisStatus(TOKEN, port)
    expect(result.state).toBe('available')
    expect(seenPath).toBe('/admin/api/v1/status')
    expect(seenAuth).toBe(`Bearer ${TOKEN}`)
    expect(JSON.stringify(result)).not.toContain(TOKEN)
  })

  it('does not follow redirects', async () => {
    let redirected = false
    const target = await listen((_req, res) => { redirected = true; res.end(JSON.stringify(VALID_STATUS)) })
    const source = await listen((_req, res) => { res.statusCode = 302; res.setHeader('location', `http://127.0.0.1:${target.port}/steal`); res.end() })
    expect(await fetchIdorisStatus(TOKEN, source.port)).toEqual({ state: 'unavailable', reason: 'http-error' })
    expect(redirected).toBe(false)
  })

  it('destroys a streaming non-200 response instead of draining it in the background', async () => {
    let observedClose: (() => void) | null = null
    const closed = new Promise<void>((resolve) => { observedClose = resolve })
    const { port } = await listen((_req, res) => {
      res.statusCode = 500
      res.write('error')
      const drip = setInterval(() => res.write('x'.repeat(1024)), 5)
      res.on('close', () => { clearInterval(drip); observedClose?.() })
    })
    expect(await fetchIdorisStatus(TOKEN, port, 500)).toEqual({ state: 'unavailable', reason: 'http-error' })
    await Promise.race([closed, new Promise((_resolve, reject) => setTimeout(() => reject(new Error('response was not destroyed')), 100))])
  })

  it('bounds body size and sanitizes transport failures', async () => {
    const { port } = await listen((_req, res) => res.end('x'.repeat(1024)))
    const result = await fetchIdorisStatus(TOKEN, port, 500, 64)
    expect(result).toEqual({ state: 'unavailable', reason: 'invalid-response' })
    expect(JSON.stringify(result)).not.toContain(TOKEN)
  })

  it('enforces a wall-clock deadline even when the server keeps sending data', async () => {
    const { port } = await listen((_req, res) => {
      const drip = setInterval(() => res.write(' '), 5)
      res.on('close', () => clearInterval(drip))
    })
    const started = Date.now()
    expect(await fetchIdorisStatus(TOKEN, port, 40)).toEqual({ state: 'unavailable', reason: 'unreachable' })
    expect(Date.now() - started).toBeLessThan(250)
  })
})

function TOKEN_ARG(): string { return '--idoris-admin-token-fd' }
