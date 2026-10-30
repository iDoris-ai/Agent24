import http from 'node:http'
import fs from 'node:fs'
import path from 'node:path'
import type { AddressInfo } from 'node:net'
import { afterEach, describe, expect, it } from 'vitest'
import {
  ALLOWED_PROXY_ROUTES,
  CHAT_TIMEOUT_MS,
  PROXY_MAX_BODY_BYTES,
  PROXY_TIMEOUT_MS,
  findAllowedRoute,
  isAllowedProxyRoute,
  matchTemplate,
  proxyRequest,
  withKnowledgeBinding,
} from './backend-proxy'

describe('A1 template matching ({param} = one safe segment)', () => {
  it('binds exactly one non-empty segment', () => {
    expect(matchTemplate('/api/v1/runs/{id}', '/api/v1/runs/r1')).toBe(true)
    expect(matchTemplate('/api/v1/runs/{id}', '/api/v1/runs/')).toBe(false)
    expect(matchTemplate('/api/v1/runs/{id}', '/api/v1/runs/a/b')).toBe(false)
    expect(matchTemplate('/api/v1/runs/{id}', '/api/v1/runs')).toBe(false)
  })
  it('refuses traversal and separator smuggling inside a param', () => {
    for (const bad of ['..', '.', '%2e%2e', '%2E', 'a%2Fb', 'a%5Cb', 'a%00b', '%zz']) {
      expect(matchTemplate('/api/v1/runs/{id}', `/api/v1/runs/${bad}`)).toBe(false)
    }
  })
  it('accepts percent-encoded ordinary values (renderer uses encodeURIComponent)', () => {
    expect(matchTemplate('/api/v1/os/{name}/commands/{command}', '/api/v1/os/my%20mod/commands/speak')).toBe(true)
  })
})

describe('A1 findAllowedRoute', () => {
  it('allows creating a confirmed rerun through the backend proxy', () => {
    expect(isAllowedProxyRoute('POST', '/api/v1/runs')).toBe(true)
  })
  it('ignores the query string and requires the exact method', () => {
    expect(isAllowedProxyRoute('GET', '/api/v1/approvals?status=pending')).toBe(true)
    expect(isAllowedProxyRoute('GET', '/api/v1/usage?module=agentear')).toBe(true)
    expect(isAllowedProxyRoute('POST', '/api/v1/approvals?status=pending')).toBe(false)
    expect(isAllowedProxyRoute('get', '/api/v1/models')).toBe(false)
  })
  it('rejects control characters anywhere in path or query (header/request-line smuggling)', () => {
    for (const bad of ['\n', '\r', '\0', '\t', '\u001f', '\u007f']) {
      expect(isAllowedProxyRoute('GET', `/api/v1/runs?x=1${bad}Host: evil`), JSON.stringify(bad)).toBe(false)
      expect(isAllowedProxyRoute('GET', `/api/v1/runs/a${bad}b`), JSON.stringify(bad)).toBe(false)
    }
    expect(isAllowedProxyRoute('GET', '/api/v1/runs?x=1&y=%0d%0a')).toBe(true)
  })
  it('rejects fragments and non-absolute paths', () => {
    expect(isAllowedProxyRoute('GET', '/api/v1/models#x')).toBe(false)
    expect(isAllowedProxyRoute('GET', 'api/v1/models')).toBe(false)
    expect(isAllowedProxyRoute('GET', '//api/v1/models')).toBe(false)
  })
  it('never contains an elevated route', () => {
    const forbidden: [string, string][] = [
      ['POST', '/api/v1/approvals/x'], ['POST', '/api/v1/module-approvals/x'], ['POST', '/api/v1/shutdown'],
      ['PUT', '/api/v1/tool-overrides/x'], ['DELETE', '/api/v1/tool-overrides/x'], ['DELETE', '/api/v1/standing-grants/x'],
      ['POST', '/api/v1/capabilities/creative'], ['POST', '/api/v1/capabilities/x/revoke'],
      ['POST', '/api/v1/attached'], ['PATCH', '/api/v1/attached/x'], ['DELETE', '/api/v1/attached/x'],
      ['PATCH', '/api/v1/os/x'], ['POST', '/api/v1/os/x/stop'],
    ]
    for (const [m, p] of forbidden) expect(isAllowedProxyRoute(m, p), `${m} ${p}`).toBe(false)
    for (const rt of ALLOWED_PROXY_ROUTES) {
      expect(rt.template).not.toMatch(/approvals\/\{|shutdown|tool-overrides|standing-grants|capabilities|\/os\/\{name\}\/stop$/)
    }
  })
  it('gives chat a longer deadline than the default', () => {
    expect(findAllowedRoute('POST', '/api/v1/chat')?.timeoutMs).toBe(CHAT_TIMEOUT_MS)
    expect(CHAT_TIMEOUT_MS).toBeGreaterThan(PROXY_TIMEOUT_MS)
    expect(findAllowedRoute('GET', '/api/v1/runs')?.timeoutMs).toBeUndefined()
  })
})

// Positive control: every /api/v1 path the renderer sources mention must be
// reachable through the allowlist (any method) -- or be one of the two paths
// that moved to a dedicated IPC. If someone adds a backendProxy call to a new
// route, this fails until the route is allowlisted on purpose.
describe('A1 renderer call sites are covered by the allowlist (positive control)', () => {
  const root = path.join(__dirname, '..', 'renderer')
  const files: string[] = []
  const walk = (dir: string): void => {
    for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, e.name)
      if (e.isDirectory()) walk(full)
      else if (/\.(ts|tsx)$/.test(e.name) && !/\.test\./.test(e.name)) files.push(full)
    }
  }
  walk(root)
  const dedicated = [/^\/api\/v1\/approvals\/\$?\{?id\}?$/, /^\/api\/v1\/os\/[^/]+$/]
  const found = new Map<string, string>()
  for (const f of files) {
    const src = fs.readFileSync(f, 'utf8')
    for (const m of src.matchAll(/[`'"](\/api\/v1\/[^`'"]*)[`'"]/g)) {
      // Template literals: a ${...} that is a whole segment -> a concrete safe
      // segment; one glued onto a word (a query suffix) is dropped. Then drop the query.
      const concrete = (m[1] as string)
        .replace(/(?<=\/)\$\{[^}]*\}(?=\/|$)/g, 'x')
        .replace(/\$\{[^}]*\}/g, '')
        .split('?')[0] as string
      found.set(concrete, path.relative(root, f))
    }
  }
  it('finds a sane number of call sites', () => {
    expect(found.size).toBeGreaterThan(15)
  })
  for (const [p, file] of found) {
    it(`${p} (${file})`, () => {
      const methods = ['GET', 'POST', 'PUT', 'PATCH', 'DELETE']
      const reachable = methods.some((m) => isAllowedProxyRoute(m, p))
      const moved = p === '/api/v1/approvals/x' || p === '/api/v1/os/x'
      expect(reachable || moved).toBe(true)
      void dedicated
    })
  }
})

describe('A2 proxyRequest', () => {
  const servers: http.Server[] = []
  afterEach(async () => {
    await Promise.all(servers.splice(0).map((s) => new Promise<void>((r) => { s.closeAllConnections(); s.close(() => r()) })))
  })
  async function serve(handler: http.RequestListener): Promise<{ port: number; authHeaders: Record<string, string> }> {
    const s = http.createServer(handler)
    servers.push(s)
    await new Promise<void>((r) => s.listen(0, '127.0.0.1', r))
    return { port: (s.address() as AddressInfo).port, authHeaders: { Authorization: 'Bearer t' } }
  }

  it('defaults are 30 s and 8 MiB', () => {
    expect(PROXY_TIMEOUT_MS).toBe(30_000)
    expect(PROXY_MAX_BODY_BYTES).toBe(8 * 1024 * 1024)
  })

  it('passes a normal JSON response through', async () => {
    const ep = await serve((_q, res) => res.end('{"a":1}'))
    const out = await proxyRequest(ep, { method: 'GET', path: '/x' })
    expect(out).toEqual({ ok: true, status: 200, data: { a: 1 } })
  })

  it('a hung daemon yields a timeout error in the unified shape', async () => {
    const ep = await serve(() => { /* never answers */ })
    const started = Date.now()
    const out = await proxyRequest(ep, { method: 'GET', path: '/x' }, { timeoutMs: 150 })
    expect(Date.now() - started).toBeLessThan(2000)
    expect(out.ok).toBe(false)
    expect(out.status).toBe(504)
    expect(out.data).toEqual({ error: { code: 'timeout', message: expect.any(String) } })
  })

  it('a daemon that drips bytes forever still hits the overall deadline', async () => {
    const ep = await serve((_q, res) => {
      res.writeHead(200)
      const t = setInterval(() => res.write(' '), 20)
      res.on('close', () => clearInterval(t))
    })
    const out = await proxyRequest(ep, { method: 'GET', path: '/x' }, { timeoutMs: 200 })
    expect((out.data as { error: { code: string } }).error.code).toBe('timeout')
  })

  it('an oversize body is cut off with payload_too_large (streamed, no content-length)', async () => {
    const ep = await serve((_q, res) => {
      res.writeHead(200)
      res.write(Buffer.alloc(600))
      res.write(Buffer.alloc(600))
      res.end(Buffer.alloc(600))
    })
    const out = await proxyRequest(ep, { method: 'GET', path: '/x' }, { maxBytes: 1024 })
    expect(out.ok).toBe(false)
    expect((out.data as { error: { code: string } }).error.code).toBe('payload_too_large')
  })

  it('an oversize body is refused up front when content-length says so', async () => {
    const ep = await serve((_q, res) => res.end(Buffer.alloc(5000, 0x20)))
    const out = await proxyRequest(ep, { method: 'GET', path: '/x' }, { maxBytes: 1024 })
    expect((out.data as { error: { code: string } }).error.code).toBe('payload_too_large')
  })

  it('a refused connection becomes proxy_failed in the same shape', async () => {
    const s = http.createServer()
    await new Promise<void>((r) => s.listen(0, '127.0.0.1', r))
    const port = (s.address() as AddressInfo).port
    await new Promise<void>((r) => s.close(() => r()))
    const out = await proxyRequest({ port, authHeaders: {} }, { method: 'GET', path: '/x' })
    expect(out.ok).toBe(false)
    expect((out.data as { error: { code: string; message: string } }).error.code).toBe('proxy_failed')
  })

  it('a synchronous http.request throw becomes proxy_failed, with no stray timer error', async () => {
    const ep = await serve((_q, res) => res.end('{}'))
    // A space in the path makes node throw ERR_UNESCAPED_CHARACTERS synchronously.
    const out = await proxyRequest(ep, { method: 'GET', path: '/bad path' }, { timeoutMs: 50 })
    expect(out.ok).toBe(false)
    expect((out.data as { error: { code: string } }).error.code).toBe('proxy_failed')
    // Outlive the (cleared) deadline: a leftover timer would throw here.
    await new Promise((r) => setTimeout(r, 120))
  })

  it('an invalid method also stays inside the unified error shape', async () => {
    const ep = await serve((_q, res) => res.end('{}'))
    const out = await proxyRequest(ep, { method: 'BAD METHOD' as never, path: '/x' }, { timeoutMs: 50 })
    expect((out.data as { error: { code: string } }).error.code).toBe('proxy_failed')
    await new Promise((r) => setTimeout(r, 120))
  })

  it('a body JSON cannot serialize (BigInt, cycle) resolves invalid_request instead of rejecting', async () => {
    const ep = await serve((_q, res) => res.end('{}'))
    const cyc: Record<string, unknown> = {}
    cyc['self'] = cyc
    for (const body of [{ x: 1n }, cyc]) {
      const out = await proxyRequest(ep, { method: 'POST', path: '/x', body })
      expect(out.ok).toBe(false)
      expect(out.status).toBe(400)
      expect((out.data as { error: { code: string } }).error.code).toBe('invalid_request')
    }
  })

  it('sends the bearer token and JSON body', async () => {
    let seen: { auth?: string; body: string } = { body: '' }
    const ep = await serve((req, res) => {
      const c: Buffer[] = []
      req.on('data', (d: Buffer) => c.push(d))
      req.on('end', () => { seen = { auth: req.headers.authorization, body: Buffer.concat(c).toString() }; res.end('{}') })
    })
    await proxyRequest(ep, { method: 'POST', path: '/x', body: { k: 1 } })
    expect(seen.auth).toBe('Bearer t')
    expect(JSON.parse(seen.body)).toEqual({ k: 1 })
  })
})

describe('withKnowledgeBinding (decision 2026-10-30-main-chat-knowledge-path)', () => {
  const runs = findAllowedRoute('POST', '/api/v1/runs')!
  const chat = findAllowedRoute('POST', '/api/v1/chat')!

  it('drops the renderer’s binding from a run it creates and sets main’s', () => {
    const req = { method: 'POST' as const, path: '/api/v1/runs', body: { prompt: 'q', knowledge_binding_id: 'evil' } }
    expect(withKnowledgeBinding(runs, req, 'default')?.body).toEqual({ prompt: 'q', knowledge_binding_id: 'default' })
    expect(withKnowledgeBinding(runs, req, null)?.body).toEqual({ prompt: 'q' })
  })

  it('leaves every other route as it is', () => {
    const other = { method: 'POST' as const, path: '/api/v1/chat', body: { knowledge_binding_id: 'x' } }
    expect(withKnowledgeBinding(chat, other, 'default')).toBe(other)
  })

  it('refuses a run whose body is no JSON object, which could name a binding by position', () => {
    for (const body of [[false, null, null, 'q', null, 'normal', 'default'], 'q', null, undefined]) {
      expect(withKnowledgeBinding(runs, { method: 'POST' as const, path: '/api/v1/runs', body }, null)).toBeNull()
    }
  })

  it('lets the renderer create a session for the main chat', () => {
    expect(findAllowedRoute('POST', '/api/v1/sessions')).toBeTruthy()
  })
})
