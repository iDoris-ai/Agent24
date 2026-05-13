// Backend daemon — Node.js built-in http server (M2 zero-dependency approach).
// M3: replace with Fastify for schema validation, plugin system, and better perf.

import http from 'node:http'
import { URL } from 'node:url'
import { LLMGateway } from './llm-gateway'
import {
  registerAll,
  getAllModules,
  loadCommunityModules,
  registerCommunityModule,
  unregisterCommunityModule,
} from './capability-registry'
import { loadState, isEnabled, setEnabled } from './module-state'
import { installModule, uninstallModule, loadInstalledModule } from './module-installer'
import type { SimpleRouter, RouteContext, RouteHandler } from './capabilities/base'
import type { LLMRequest } from './types'

const PORT = 8765
const HOST = '127.0.0.1'

type RouteKey = `${'GET' | 'POST'} ${string}`

interface RouteEntry {
  handler: RouteHandler
  moduleId: string
}

const routes = new Map<RouteKey, RouteEntry>()

const gateway = new LLMGateway()

function buildRouter(moduleId: string): SimpleRouter {
  return {
    get(path: string, handler: RouteHandler) {
      routes.set(`GET ${path}`, { handler, moduleId })
    },
    post(path: string, handler: RouteHandler) {
      routes.set(`POST ${path}`, { handler, moduleId })
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

  // Special: module enable/disable (parameterised — not in the routes Map)
  const enableMatch = url.pathname.match(/^\/api\/modules\/([^/]+)\/(enable|disable)$/)
  if (enableMatch && method === 'POST') {
    let id: string
    try {
      id = decodeURIComponent(enableMatch[1])
    } catch {
      send(res, 400, { error: 'Invalid module id encoding' })
      return
    }
    if (!getAllModules().some((m) => m.manifest.id === id)) {
      send(res, 404, { error: 'Unknown module', id })
      return
    }
    const action = enableMatch[2] as 'enable' | 'disable'
    setEnabled(id, action === 'enable')
    send(res, 200, { ok: true, id, enabled: action === 'enable' })
    return
  }

  // Special: module install (POST /api/modules/install)
  if (url.pathname === '/api/modules/install' && method === 'POST') {
    const body = await readBody(req) as { packageName?: string }
    const packageName = body?.packageName
    if (typeof packageName !== 'string' || !packageName) {
      send(res, 400, { ok: false, error: 'packageName required' })
      return
    }
    const result = await installModule(packageName)
    if (!result.ok || !result.modulePath) {
      send(res, 500, { ok: false, error: result.error })
      return
    }
    const mod = loadInstalledModule(result.modulePath)
    if (!mod) {
      // Roll back: uninstall the package so the system stays consistent
      await uninstallModule(packageName)
      send(res, 500, { ok: false, error: 'Package installed but does not export a valid CapabilityModule — rolled back' })
      return
    }
    registerCommunityModule(mod, buildRouter, { llm: gateway })
    send(res, 200, { ok: true, id: mod.manifest.id, manifest: mod.manifest })
    return
  }

  // Special: module uninstall (POST /api/modules/uninstall)
  if (url.pathname === '/api/modules/uninstall' && method === 'POST') {
    const body = await readBody(req) as { packageName?: string; id?: string }
    const packageName = body?.packageName
    const id = body?.id
    if (typeof packageName !== 'string' || !packageName) {
      send(res, 400, { ok: false, error: 'packageName required' })
      return
    }
    if (typeof id === 'string') {
      unregisterCommunityModule(id)
    }
    const result = await uninstallModule(packageName)
    send(res, result.ok ? 200 : 500, { ok: result.ok, error: result.error })
    return
  }

  const entry = routes.get(`${method} ${url.pathname}` as RouteKey)
  if (!entry) {
    send(res, 404, { error: 'Not found', path: url.pathname })
    return
  }

  // Check module enabled state (system routes use moduleId 'system' — always pass)
  if (entry.moduleId !== 'system' && !isEnabled(entry.moduleId)) {
    send(res, 503, { error: 'Module disabled', id: entry.moduleId })
    return
  }

  const body = method === 'POST' ? await readBody(req) : {}
  const ctx: RouteContext = {
    params: {},
    query: parseQuery(url.search),
    body,
  }

  try {
    const result = await entry.handler(ctx)
    send(res, 200, result)
  } catch (err) {
    const statusCode = (err as { statusCode?: number }).statusCode ?? 500
    const message = err instanceof Error ? err.message : 'Internal error'
    send(res, statusCode, { error: message })
  }
}

function registerCoreRoutes(): void {
  routes.set('GET /health', { handler: () => ({ status: 'ok', ts: Date.now() }), moduleId: 'system' })

  // Return manifests + enabled state for all registered capability modules
  routes.set('GET /api/modules', {
    handler: () => getAllModules().map((m) => ({ ...m.manifest, enabled: isEnabled(m.manifest.id) })),
    moduleId: 'system',
  })

  routes.set('GET /api/llm/usage', { handler: () => gateway.getUsage(), moduleId: 'system' })

  // M3: list all models known to oMLX with load status
  routes.set('GET /api/llm/models', {
    handler: () => gateway.listModels(),
    moduleId: 'system',
  })

  routes.set('POST /api/llm/chat', {
    handler: async (ctx) => {
      const req = ctx.body as LLMRequest
      if (!req.messages || !Array.isArray(req.messages)) {
        throw Object.assign(new Error('messages array required'), { statusCode: 400 })
      }
      const message = await gateway.chat(req, 'direct')
      return { message }
    },
    moduleId: 'system',
  })
}

function start(): void {
  loadState()
  loadCommunityModules()       // M3: load previously installed community modules
  registerCoreRoutes()
  registerAll((moduleId) => buildRouter(moduleId), { llm: gateway })

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
