// LLM Gateway — unified interface for all capability modules.
// Provider priority: oMLX (OpenAI-compat, port 8088) → Ollama (port 11434) → error
// M3: model declaration support — ensureModel() loads declared models on demand.
// Configure via environment: OMLX_URL, OMLX_API_KEY, DEFAULT_MODEL

import http from 'node:http'
import type { ChatMessage, LLMProvider, LLMRequest, LLMUsage } from './types'

const OMLX_URL   = process.env['OMLX_URL']     ?? 'http://127.0.0.1:8088'
const OMLX_KEY   = process.env['OMLX_API_KEY'] ?? 'xiaobao8088'
const OLLAMA_URL = 'http://127.0.0.1:11434'
const DEFAULT_MODEL = process.env['DEFAULT_MODEL'] ?? 'Qwen3-8B-4bit'

// ── HTTP helpers ──────────────────────────────────────────────────────────────

function httpRequest(
  method: 'GET' | 'POST' | 'PUT',
  url: string,
  body?: unknown,
  headers: Record<string, string> = {},
): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const payload = body ? JSON.stringify(body) : undefined
    const parsed = new URL(url)
    const opts: http.RequestOptions = {
      hostname: parsed.hostname,
      port: parsed.port || (parsed.protocol === 'https:' ? 443 : 80),
      path: parsed.pathname + parsed.search,
      method,
      headers: {
        'Content-Type': 'application/json',
        ...(payload ? { 'Content-Length': Buffer.byteLength(payload) } : {}),
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
    if (payload) req.write(payload)
    req.end()
  })
}

function omlxHeaders(): Record<string, string> {
  return OMLX_KEY ? { 'Authorization': `Bearer ${OMLX_KEY}` } : {}
}

// ── oMLX admin API ────────────────────────────────────────────────────────────

interface OmlxModelEntry {
  id: string
  engine?: string      // 'loaded' | 'unloaded' | undefined
  status?: string
}

async function omlxListModels(): Promise<OmlxModelEntry[]> {
  try {
    const raw = await httpRequest('GET', `${OMLX_URL}/admin/api/models`, undefined, omlxHeaders()) as unknown
    if (Array.isArray(raw)) return raw as OmlxModelEntry[]
    const data = raw as { models?: OmlxModelEntry[] }
    return Array.isArray(data.models) ? data.models : []
  } catch { return [] }
}

async function omlxLoadModel(modelId: string): Promise<boolean> {
  try {
    await httpRequest('POST', `${OMLX_URL}/admin/api/models/${encodeURIComponent(modelId)}/load`, {}, omlxHeaders())
    return true
  } catch (err) {
    console.warn(`[gateway] oMLX load model ${modelId} failed:`, err instanceof Error ? err.message : err)
    return false
  }
}

async function omlxDownloadModel(modelId: string): Promise<string | null> {
  try {
    const raw = await httpRequest('POST', `${OMLX_URL}/admin/api/hf/download`, { model_id: modelId }, omlxHeaders()) as { task_id?: string }
    return raw.task_id ?? null
  } catch (err) {
    console.warn(`[gateway] oMLX download ${modelId} failed:`, err instanceof Error ? err.message : err)
    return null
  }
}

async function omlxPollDownload(taskId: string, timeoutMs = 300_000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    try {
      const raw = await httpRequest('GET', `${OMLX_URL}/admin/api/hf/tasks`, undefined, omlxHeaders()) as { tasks?: Array<{ id: string; status: string }> }
      const task = raw.tasks?.find((t) => t.id === taskId)
      if (task?.status === 'completed') return true
      if (task?.status === 'failed') return false
    } catch { /* keep polling */ }
    await new Promise((r) => setTimeout(r, 5_000))
  }
  return false
}

// ── Chat providers ────────────────────────────────────────────────────────────

async function chatViaOmlx(req: LLMRequest): Promise<ChatMessage> {
  const raw = await httpRequest('POST', `${OMLX_URL}/v1/chat/completions`, {
    model: req.model ?? DEFAULT_MODEL,
    messages: req.messages,
    stream: false,
  }, omlxHeaders()) as Record<string, unknown>

  const choice = (raw['choices'] as Array<{ message: ChatMessage }> | undefined)?.[0]
  if (!choice?.message) throw new Error('oMLX returned no choices')
  return choice.message
}

async function chatViaOllama(req: LLMRequest): Promise<ChatMessage> {
  const raw = await httpRequest('POST', `${OLLAMA_URL}/api/chat`, {
    model: req.model ?? DEFAULT_MODEL,
    messages: req.messages,
    stream: false,
  }) as Record<string, unknown>
  const message = raw['message'] as ChatMessage | undefined
  if (!message) throw new Error('Ollama returned no message')
  return message
}

// ── Gateway ───────────────────────────────────────────────────────────────────

export class LLMGateway {
  private usageLog: LLMUsage[] = []

  /**
   * M3: Ensure a required model is available in oMLX.
   * Strategy: loaded → done; downloaded-not-loaded → load; not present → download+load.
   * Called at module registration time, non-blocking (fire-and-forget via caller).
   */
  async ensureModel(modelId: string): Promise<void> {
    const models = await omlxListModels()
    const entry = models.find((m) => m.id === modelId || m.id.includes(modelId))

    if (!entry) {
      // Model not downloaded — trigger async download
      console.log(`[gateway] model not found, downloading: ${modelId}`)
      const taskId = await omlxDownloadModel(modelId)
      if (taskId) {
        // Poll in background — don't block registration
        void omlxPollDownload(taskId).then((ok) => {
          if (ok) {
            console.log(`[gateway] download complete, loading: ${modelId}`)
            void omlxLoadModel(modelId)
          } else {
            console.warn(`[gateway] download failed for: ${modelId}`)
          }
        })
      }
      return
    }

    // Model is present — check if loaded
    const isLoaded = entry.engine === 'loaded' || entry.status === 'loaded'
    if (!isLoaded) {
      console.log(`[gateway] loading model: ${modelId}`)
      await omlxLoadModel(entry.id)
    }
  }

  /** Ensure all models declared by a module manifest. */
  async ensureModels(models: string[]): Promise<void> {
    await Promise.all(models.map((m) => this.ensureModel(m)))
  }

  /** List all models known to oMLX with their status. */
  async listModels(): Promise<OmlxModelEntry[]> {
    return omlxListModels()
  }

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
        if (!isConnRefused) throw lastErr
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
