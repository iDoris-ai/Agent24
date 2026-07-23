import { describe, it, expect, vi } from 'vitest'
import pingModule from './example-ping'
import type { SimpleRouter, RouteContext } from './base'

function makeRouter() {
  const handlers = new Map<string, (ctx: RouteContext) => unknown>()
  const router: SimpleRouter = {
    get: (path, handler) => handlers.set(`GET ${path}`, handler),
    post: (path, handler) => handlers.set(`POST ${path}`, handler),
  }
  const invoke = async (method: string, path: string, body = {}) => {
    const key = `${method} ${path}`
    const handler = handlers.get(key)
    if (!handler) throw new Error(`No handler for ${key}`)
    return handler({ params: {}, query: {}, body })
  }
  return { router, invoke }
}

const mockCtx = { llm: { chat: vi.fn(), getUsage: vi.fn(), clearUsage: vi.fn() }, moduleId: 'ping' }

describe('example-ping module', () => {
  it('has headless manifest', () => {
    expect(pingModule.manifest.type).toBe('headless')
    expect(pingModule.manifest.id).toBe('ping')
    expect(pingModule.manifest.permissions).toEqual([])
  })

  it('GET /api/capabilities/ping returns status ok and ts', async () => {
    const { router, invoke } = makeRouter()
    pingModule.register(router, mockCtx as never)
    const result = await invoke('GET', '/api/capabilities/ping') as Record<string, unknown>
    expect(result.status).toBe('ok')
    expect(result.moduleId).toBe('ping')
    expect(typeof result.ts).toBe('number')
  })
})
