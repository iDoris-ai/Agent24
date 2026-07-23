import { describe, it, expect, vi } from 'vitest'
import helloUiModule from './example-hello-ui'
import type { SimpleRouter, RouteContext } from './base'

function makeRouter() {
  const handlers = new Map<string, (ctx: RouteContext) => unknown>()
  const router: SimpleRouter = {
    get: (path, handler) => handlers.set(`GET ${path}`, handler),
    post: (path, handler) => handlers.set(`POST ${path}`, handler),
  }
  const invoke = (method: string, path: string, body = {}) => {
    const key = `${method} ${path}`
    const handler = handlers.get(key)
    if (!handler) throw new Error(`No handler for ${key}`)
    return handler({ params: {}, query: {}, body })
  }
  return { router, invoke }
}

describe('example-hello-ui module', () => {
  it('has ui manifest with navItem', () => {
    expect(helloUiModule.manifest.type).toBe('ui')
    expect(helloUiModule.manifest.navItem).toBeDefined()
    expect(helloUiModule.manifest.navItem?.route).toBe('/modules/hello')
    expect(helloUiModule.manifest.navItem?.icon).toBe('👋')
    expect(helloUiModule.manifest.permissions).toContain('llm')
  })

  it('POST /greet calls LLM and returns greeting', async () => {
    const mockLlm = {
      chat: vi.fn().mockResolvedValue({ role: 'assistant', content: 'Hello, Jason! Great to meet you.' }),
      getUsage: vi.fn(),
      clearUsage: vi.fn(),
    }
    const ctx = { llm: mockLlm, moduleId: helloUiModule.manifest.id }
    const { router, invoke } = makeRouter()
    helloUiModule.register(router, ctx as never)

    const result = await invoke('POST', '/api/modules/hello/greet', { name: 'Jason' }) as { greeting: string }
    expect(result.greeting).toBe('Hello, Jason! Great to meet you.')
    expect(mockLlm.chat).toHaveBeenCalledOnce()
    const [req] = mockLlm.chat.mock.calls[0] as [{ messages: Array<{content: string}> }]
    expect(req.messages.some(m => m.content.includes('Jason'))).toBe(true)
  })

  it('POST /greet with no name falls back to "stranger"', async () => {
    const mockLlm = {
      chat: vi.fn().mockResolvedValue({ role: 'assistant', content: 'Hey there, stranger!' }),
      getUsage: vi.fn(),
      clearUsage: vi.fn(),
    }
    const { router, invoke } = makeRouter()
    helloUiModule.register(router, { llm: mockLlm, moduleId: 'hello' } as never)
    await invoke('POST', '/api/modules/hello/greet', {})
    const [req] = mockLlm.chat.mock.calls[0] as [{ messages: Array<{content: string}> }]
    expect(req.messages.some(m => m.content.includes('stranger'))).toBe(true)
  })

  it('GET /info returns module metadata', async () => {
    const mockLlm = { chat: vi.fn(), getUsage: vi.fn(), clearUsage: vi.fn() }
    const { router, invoke } = makeRouter()
    helloUiModule.register(router, { llm: mockLlm, moduleId: helloUiModule.manifest.id } as never)

    const info = await invoke('GET', '/api/modules/hello/info') as Record<string, unknown>
    expect(info.moduleId).toBe(helloUiModule.manifest.id)
    expect(info.version).toBeTruthy()
    expect(info.description).toBeTruthy()
  })
})
