import { describe, it, expect, vi } from 'vitest'
import summarizeModule from './example-summarize'
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

describe('example-summarize module', () => {
  it('has headless manifest with llm permission', () => {
    expect(summarizeModule.manifest.type).toBe('headless')
    expect(summarizeModule.manifest.permissions).toContain('llm')
    expect(summarizeModule.manifest.navItem).toBeUndefined()
  })

  it('POST /summarize calls LLM and returns summary', async () => {
    const mockLlm = {
      chat: vi.fn().mockResolvedValue({ role: 'assistant', content: 'Short summary.' }),
      getUsage: vi.fn(),
      clearUsage: vi.fn(),
    }
    const ctx = { llm: mockLlm, moduleId: summarizeModule.manifest.id }
    const { router, invoke } = makeRouter()
    summarizeModule.register(router, ctx as never)

    const result = await invoke('POST', '/api/capabilities/summarize', {
      text: 'A long piece of text that needs summarizing.',
    }) as { summary: string }

    expect(result.summary).toBe('Short summary.')
    expect(mockLlm.chat).toHaveBeenCalledOnce()
    const [req] = mockLlm.chat.mock.calls[0] as [{ messages: Array<{role: string; content: string}> }]
    expect(req.messages.some(m => m.content.includes('A long piece'))).toBe(true)
  })

  it('POST /summarize without text throws 400', async () => {
    const mockLlm = { chat: vi.fn(), getUsage: vi.fn(), clearUsage: vi.fn() }
    const { router, invoke } = makeRouter()
    summarizeModule.register(router, { llm: mockLlm, moduleId: 'sum' } as never)

    await expect(
      invoke('POST', '/api/capabilities/summarize', {}),
    ).rejects.toMatchObject({ statusCode: 400 })
  })

  it('passes language option to LLM system prompt', async () => {
    const mockLlm = {
      chat: vi.fn().mockResolvedValue({ role: 'assistant', content: 'résumé' }),
      getUsage: vi.fn(),
      clearUsage: vi.fn(),
    }
    const { router, invoke } = makeRouter()
    summarizeModule.register(router, { llm: mockLlm, moduleId: 'sum' } as never)
    await invoke('POST', '/api/capabilities/summarize', { text: 'hello', language: 'French' })
    const [req] = mockLlm.chat.mock.calls[0] as [{ messages: Array<{content: string}> }]
    expect(req.messages[0].content).toContain('French')
  })
})
