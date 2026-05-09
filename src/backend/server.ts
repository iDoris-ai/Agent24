// Backend daemon — Node.js built-in http server (M2 zero-dependency approach).
// M3: replace with Fastify for schema validation, plugin system, and better perf.

import http from 'node:http'
import { URL } from 'node:url'
import { LLMGateway } from './llm-gateway'
import { registerAll } from './capability-registry'
import type { SimpleRouter, RouteContext, RouteHandler } from './capabilities/base'
import type { LLMRequest } from './types'

const PORT = 8765
const HOST = '127.0.0.1'

type RouteKey = `${'GET' | 'POST'} ${string}`

const routes = new Map<RouteKey, RouteHandler>()

const gateway = new LLMGateway()

function buildRouter(): SimpleRouter {
  return {
    get(path: string, handler: RouteHandler) {
      routes.set(`GET ${path}`, handler)
    },
    post(path: string, handler: RouteHandler) {
      routes.set(`POST ${path}`, handler)
    },
  }
}

function parseQuery(search: string): Record<string, string> {
  const params: Record<string, string> = {}
  new URLSearchParams(search).forEach((v, k) => { params[k] = v })
  return params
}

function readBody(req: http.IncomingMessage): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = []
    req.on('data', (chunk: Buffer) => chunks.push(chunk))
    req.on('end', () => {
      const raw = Buffer.concat(chunks).toString()
      if (!raw) { resolve({}); return }
      try { resolve(JSON.parse(raw)) } catch { resolve({}) }
    })
    req.on('error', reject)
  })
}

function send(res: http.ServerResponse, status: number, body: unknown): void {
  const payload = JSON.stringify(body)
  res.writeHead(status, { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(payload) })
  res.end(payload)
}

async function handleRequest(req: http.IncomingMessage, res: http.ServerResponse): Promise<void> {
  const base = `http://${HOST}`
  const url = new URL(req.url ?? '/', base)
  const method = (req.method ?? 'GET').toUpperCase() as 'GET' | 'POST'

  const handler = routes.get(`${method} ${url.pathname}` as RouteKey)
  if (!handler) {
    send(res, 404, { error: 'Not found', path: url.pathname })
    return
  }

  const body = method === 'POST' ? await readBody(req) : {}
  const ctx: RouteContext = {
    params: {},
    query: parseQuery(url.search),
    body,
  }

  try {
    const result = await handler(ctx)
    send(res, 200, result)
  } catch (err) {
    const statusCode = (err as { statusCode?: number }).statusCode ?? 500
    const message = err instanceof Error ? err.message : 'Internal error'
    send(res, statusCode, { error: message })
  }
}

function registerCoreRoutes(): void {
  routes.set('GET /health', () => ({ status: 'ok', ts: Date.now() }))

  routes.set('GET /api/llm/usage', () => gateway.getUsage())

  routes.set('POST /api/llm/chat', async (ctx) => {
    const req = ctx.body as LLMRequest
    if (!req.messages || !Array.isArray(req.messages)) {
      throw Object.assign(new Error('messages array required'), { statusCode: 400 })
    }
    const message = await gateway.chat(req, 'direct')
    return { message }
  })
}

function start(): void {
  registerCoreRoutes()
  registerAll(buildRouter(), { llm: gateway })

  const server = http.createServer((req, res) => {
    void handleRequest(req, res)
  })

  server.listen(PORT, HOST, () => {
    console.log(`[backend] listening on http://${HOST}:${PORT}`)
  })

  server.on('error', (err) => {
    console.error('[backend] server error', err)
    process.exit(1)
  })

  process.on('SIGTERM', () => {
    server.close(() => process.exit(0))
  })
}

start()
