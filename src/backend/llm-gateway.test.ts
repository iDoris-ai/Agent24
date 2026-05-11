import { describe, it, expect, vi, beforeEach } from 'vitest'
import http from 'node:http'

// Mock node:http before importing the gateway
vi.mock('node:http', () => {
  const mockRequest = {
    write: vi.fn(),
    end: vi.fn(),
    on: vi.fn(),
    destroy: vi.fn(),
  }
  return {
    default: {
      request: vi.fn(() => mockRequest),
      __mockRequest: mockRequest,
    },
  }
})

function getMockReq() {
  return (http as unknown as { __mockRequest: ReturnType<typeof vi.fn> & { on: ReturnType<typeof vi.fn>; write: ReturnType<typeof vi.fn>; end: ReturnType<typeof vi.fn> } }).__mockRequest
}

function simulateResponse(body: unknown, statusCode = 200) {
  const mockReq = getMockReq()
  const httpMock = http as unknown as { request: ReturnType<typeof vi.fn> }
  httpMock.request.mockImplementation((_opts: unknown, cb: (res: unknown) => void) => {
    const chunks: Buffer[] = [Buffer.from(JSON.stringify(body))]
    const res = {
      statusCode,
      on: (event: string, handler: (chunk?: Buffer) => void) => {
        if (event === 'data') chunks.forEach(c => handler(c))
        if (event === 'end') handler()
      },
      resume: vi.fn(),
    }
    cb(res)
    return mockReq
  })
}

function simulateError(message: string) {
  const mockReq = getMockReq()
  const httpMock = http as unknown as { request: ReturnType<typeof vi.fn> }
  httpMock.request.mockImplementation(() => {
    mockReq.on.mockImplementation((event: string, handler: (err: Error) => void) => {
      if (event === 'error') handler(new Error(message))
    })
    return mockReq
  })
}

describe('LLMGateway', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('chat() succeeds via oMLX when choices returned', async () => {
    simulateResponse({
      choices: [{ message: { role: 'assistant', content: 'Hello!' } }],
    })
    const { LLMGateway } = await import('./llm-gateway')
    const gw = new LLMGateway()
    const msg = await gw.chat({ messages: [{ role: 'user', content: 'hi' }] }, 'test')
    expect(msg.content).toBe('Hello!')
    expect(msg.role).toBe('assistant')
  })

  it('chat() records usage after success', async () => {
    simulateResponse({
      choices: [{ message: { role: 'assistant', content: 'ok' } }],
    })
    const { LLMGateway } = await import('./llm-gateway')
    const gw = new LLMGateway()
    await gw.chat({ messages: [{ role: 'user', content: 'hi' }] }, 'mod-a')
    const usage = gw.getUsage()
    expect(usage).toHaveLength(1)
    expect(usage[0].moduleId).toBe('mod-a')
    expect(usage[0].provider).toBe('omlx')
  })

  it('clearUsage() empties the log', async () => {
    simulateResponse({ choices: [{ message: { role: 'assistant', content: 'x' } }] })
    const { LLMGateway } = await import('./llm-gateway')
    const gw = new LLMGateway()
    await gw.chat({ messages: [{ role: 'user', content: 'hi' }] }, 'mod')
    gw.clearUsage()
    expect(gw.getUsage()).toHaveLength(0)
  })

  it('chat() falls back to Ollama when oMLX returns ECONNREFUSED', async () => {
    const httpMock = http as unknown as { request: ReturnType<typeof vi.fn> }
    const mockReq = getMockReq()
    let callCount = 0
    httpMock.request.mockImplementation((_opts: unknown, cb: (res: unknown) => void) => {
      callCount++
      if (callCount === 1) {
        // oMLX fails with ECONNREFUSED
        mockReq.on.mockImplementation((event: string, handler: (err: Error) => void) => {
          if (event === 'error') handler(new Error('ECONNREFUSED'))
        })
        return mockReq
      }
      // Ollama succeeds
      const chunks = [Buffer.from(JSON.stringify({ message: { role: 'assistant', content: 'ollama reply' } }))]
      const res = {
        on: (event: string, handler: (chunk?: Buffer) => void) => {
          if (event === 'data') chunks.forEach(c => handler(c))
          if (event === 'end') handler()
        },
      }
      cb(res)
      return mockReq
    })

    const { LLMGateway } = await import('./llm-gateway')
    const gw = new LLMGateway()
    const msg = await gw.chat({ messages: [{ role: 'user', content: 'hi' }] }, 'mod')
    expect(msg.content).toBe('ollama reply')
    const usage = gw.getUsage()
    expect(usage[0].provider).toBe('ollama')
  })

  it('chat() throws 503 when both providers are unavailable', async () => {
    simulateError('ECONNREFUSED')
    const { LLMGateway } = await import('./llm-gateway')
    const gw = new LLMGateway()
    await expect(
      gw.chat({ messages: [{ role: 'user', content: 'hi' }] }, 'mod'),
    ).rejects.toMatchObject({ statusCode: 503 })
  })

  it('chat() re-throws non-connection errors immediately without fallback', async () => {
    simulateError('unexpected parse error')
    const { LLMGateway } = await import('./llm-gateway')
    const gw = new LLMGateway()
    await expect(
      gw.chat({ messages: [{ role: 'user', content: 'hi' }] }, 'mod'),
    ).rejects.toThrow('unexpected parse error')
  })
})
