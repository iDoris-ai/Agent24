import { describe, it, expect, vi } from 'vitest'

// Mock boxlite-service so registerAll doesn't try to start real containers in tests
vi.mock('./boxlite-service', () => ({
  startService: vi.fn().mockResolvedValue({ ok: true, hostPort: 18000 }),
  stopService: vi.fn().mockResolvedValue(undefined),
  stopAll: vi.fn().mockResolvedValue(undefined),
  getHostPort: vi.fn().mockReturnValue(null),
  isServiceAvailable: vi.fn().mockReturnValue(false),
  proxyToService: vi.fn().mockResolvedValue({ status: 200, body: {} }),
}))

import { MODULES, registerAll } from './capability-registry'
import type { SimpleRouter } from './capabilities/base'

describe('MODULES', () => {
  it('contains exactly 5 built-in modules', () => {
    expect(MODULES).toHaveLength(5)
  })

  it('all modules have valid manifests', () => {
    for (const mod of MODULES) {
      expect(mod.manifest.id).toBeTruthy()
      expect(mod.manifest.version).toMatch(/^\d+\.\d+\.\d+/)
      expect(['ui', 'headless', 'hybrid']).toContain(mod.manifest.type)
      expect(Array.isArray(mod.manifest.permissions)).toBe(true)
    }
  })

  it('includes ping, summarize and hello-ui modules', () => {
    const ids = MODULES.map(m => m.manifest.id)
    expect(ids).toContain('ping')
    expect(ids).toContain('@auraaihq/example-summarize')
    expect(ids).toContain('@auraaihq/example-hello')
  })
})

describe('registerAll', () => {
  it('calls register on every module with correct moduleId in context', () => {
    const registeredRoutes: string[] = []
    const routerFactory = (_moduleId: string): SimpleRouter => ({
      get: (path) => { registeredRoutes.push(`GET ${path}`) },
      post: (path) => { registeredRoutes.push(`POST ${path}`) },
    })
    const mockLlm = { chat: vi.fn(), getUsage: vi.fn(), clearUsage: vi.fn() }
    registerAll(routerFactory, { llm: mockLlm as never })

    expect(registeredRoutes).toContain('GET /api/capabilities/ping')
    expect(registeredRoutes).toContain('POST /api/capabilities/summarize')
    expect(registeredRoutes).toContain('POST /api/modules/hello/greet')
    expect(registeredRoutes).toContain('GET /api/modules/hello/info')
  })
})
