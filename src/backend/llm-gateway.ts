// LLM Gateway — all capability modules call LLM through this interface.
// M2: routes to Ollama (localhost:11434).
// M3: configurable via LLM_BACKEND env (ollama | openai-compat | claude).
// Production: replace with Fastify-aware version; interface stays the same.

import http from 'node:http'
import type { ChatMessage, LLMRequest, LLMUsage } from './types'

const OLLAMA_HOST = 'localhost'
const OLLAMA_PORT = 11434
const DEFAULT_MODEL = process.env['DEFAULT_MODEL'] ?? 'llama3'

function postJson(
  host: string,
  port: number,
  path: string,
  body: unknown,
): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const payload = JSON.stringify(body)
    const req = http.request(
      { host, port, path, method: 'POST', headers: { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(payload) } },
      (res) => {
        const chunks: Buffer[] = []
        res.on('data', (chunk: Buffer) => chunks.push(chunk))
        res.on('end', () => {
          try {
            resolve(JSON.parse(Buffer.concat(chunks).toString()))
          } catch (e) {
            reject(e)
          }
        })
      },
    )
    req.on('error', reject)
    req.write(payload)
    req.end()
  })
}

export class LLMGateway {
  private usageLog: LLMUsage[] = []

  async chat(req: LLMRequest, moduleId: string): Promise<ChatMessage> {
    const model = req.model ?? DEFAULT_MODEL
    let raw: unknown
    try {
      raw = await postJson(OLLAMA_HOST, OLLAMA_PORT, '/api/chat', {
        model,
        messages: req.messages,
        stream: false,
      })
    } catch (err) {
      const isConnRefused =
        err instanceof Error && err.message.includes('ECONNREFUSED')
      if (isConnRefused) {
        throw Object.assign(
          new Error(
            'Ollama is not running. Start it with: ollama serve',
          ),
          { statusCode: 503 },
        )
      }
      throw err
    }

    const data = raw as Record<string, unknown>
    const message = (data['message'] as ChatMessage | undefined) ?? {
      role: 'assistant' as const,
      content: '',
    }

    // Estimate token count from content length (Ollama returns eval_count when available).
    const tokens =
      typeof data['eval_count'] === 'number'
        ? (data['eval_count'] as number)
        : Math.ceil((message.content?.length ?? 0) / 4)

    this.usageLog.push({ tokens, model, moduleId, timestamp: Date.now() })

    return message
  }

  getUsage(): LLMUsage[] {
    return [...this.usageLog]
  }

  clearUsage(): void {
    this.usageLog = []
  }
}
