// LLM Gateway — unified interface for all capability modules.
// Provider priority: oMLX (OpenAI-compat, port 8088) → Ollama (port 11434) → error
// Configure via environment: OMLX_URL, OMLX_API_KEY, DEFAULT_MODEL

import http from 'node:http'
import type { ChatMessage, LLMProvider, LLMRequest, LLMUsage } from './types'

const OMLX_URL  = process.env['OMLX_URL']     ?? 'http://127.0.0.1:8088'
const OMLX_KEY  = process.env['OMLX_API_KEY'] ?? 'xiaobao8088'
const OLLAMA_URL = 'http://127.0.0.1:11434'
const DEFAULT_MODEL = process.env['DEFAULT_MODEL'] ?? 'Qwen3-8B-4bit'

function httpPost(url: string, body: unknown, headers: Record<string, string> = {}): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const payload = JSON.stringify(body)
    const parsed = new URL(url)
    const opts: http.RequestOptions = {
      hostname: parsed.hostname,
      port: parsed.port || (parsed.protocol === 'https:' ? 443 : 80),
      path: parsed.pathname + parsed.search,
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Content-Length': Buffer.byteLength(payload),
        ...headers,
      },
      timeout: 120_000,
    }
    const req = http.request(opts, (res) => {
      const chunks: Buffer[] = []
      res.on('data', (c: Buffer) => chunks.push(c))
      res.on('end', () => {
        try { resolve(JSON.parse(Buffer.concat(chunks).toString())) }
        catch (e) { reject(e) }
      })
    })
    req.on('error', reject)
    req.on('timeout', () => { req.destroy(); reject(new Error('LLM request timed out')) })
    req.write(payload)
    req.end()
  })
}

async function chatViaOmlx(req: LLMRequest): Promise<ChatMessage> {
  const headers: Record<string, string> = {}
  if (OMLX_KEY) headers['Authorization'] = `Bearer ${OMLX_KEY}`
  const raw = await httpPost(`${OMLX_URL}/v1/chat/completions`, {
    model: req.model ?? DEFAULT_MODEL,
    messages: req.messages,
    stream: false,
  }, headers) as Record<string, unknown>

  // OpenAI-compat response: { choices: [{ message: { role, content } }] }
  const choice = (raw['choices'] as Array<{ message: ChatMessage }> | undefined)?.[0]
  if (!choice?.message) throw new Error('oMLX returned no choices')
  return choice.message
}

async function chatViaOllama(req: LLMRequest): Promise<ChatMessage> {
  const raw = await httpPost(`${OLLAMA_URL}/api/chat`, {
    model: req.model ?? DEFAULT_MODEL,
    messages: req.messages,
    stream: false,
  }) as Record<string, unknown>
  const message = raw['message'] as ChatMessage | undefined
  if (!message) throw new Error('Ollama returned no message')
  return message
}

export class LLMGateway {
  private usageLog: LLMUsage[] = []

  async chat(req: LLMRequest, moduleId: string): Promise<ChatMessage> {
    const providers: Array<{ name: LLMProvider; fn: () => Promise<ChatMessage> }> = [
      { name: 'omlx',   fn: () => chatViaOmlx(req) },
      { name: 'ollama', fn: () => chatViaOllama(req) },
    ]

    let lastErr: Error = new Error('No LLM provider available')
    for (const { name, fn } of providers) {
      try {
        const message = await fn()
        this.usageLog.push({
          tokens: Math.ceil((message.content?.length ?? 0) / 4),
          model: req.model ?? DEFAULT_MODEL,
          provider: name,
          moduleId,
          timestamp: Date.now(),
        })
        return message
      } catch (err) {
        lastErr = err instanceof Error ? err : new Error(String(err))
        const isConnRefused = lastErr.message.includes('ECONNREFUSED') || lastErr.message.includes('ENOTFOUND')
        if (!isConnRefused) throw lastErr  // unexpected error — don't try next provider
        // connection refused → try next provider
      }
    }

    throw Object.assign(
      new Error(`All LLM providers unavailable. Last error: ${lastErr.message}\nStart oMLX: omlx serve --port 8088 --api-key xiaobao8088`),
      { statusCode: 503 },
    )
  }

  getUsage(): LLMUsage[] { return [...this.usageLog] }
  clearUsage(): void { this.usageLog = [] }
}
