// v1 REST adapter (A5 mock scope: M-A endpoints only).
// Contract: protocol/openapi.yaml + docs/specs/SPEC-002-protocol.md.
// Error format is the v1 envelope { error: { code, message } } — never the
// legacy flat { error: string } shape.

import type http from 'node:http'
import type { LLMGateway } from '../llm-gateway'
import type { LLMRequest } from '../types'
import type { EventsHub, UsageBody } from './events-hub'
import { readBodyLimited, ulid, PayloadTooLargeError } from './util'

// eslint-disable-next-line @typescript-eslint/no-require-imports, @typescript-eslint/no-var-requires
const PKG = require('../../package.json') as { version: string }

function send(res: http.ServerResponse, status: number, body: unknown): void {
  const payload = JSON.stringify(body)
  res.writeHead(status, { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(payload) })
  res.end(payload)
}

function sendError(res: http.ServerResponse, status: number, code: string, message: string): void {
  send(res, status, { error: { code, message } })
}

interface ChatBody {
  messages?: Array<{ role?: unknown; content?: unknown }>
  model?: unknown
}

function isValidChatBody(b: ChatBody): b is { messages: Array<{ role: string; content: string }>; model?: string } {
  return (
    Array.isArray(b.messages) &&
    b.messages.length > 0 &&
    b.messages.every((m) => typeof m?.role === 'string' && typeof m?.content === 'string') &&
    (b.model === undefined || b.model === null || typeof b.model === 'string')
  )
}

// Token estimate mirrors llm-gateway.ts usage logging (content length / 4)
function estimateUsage(text: string): UsageBody {
  const completion = Math.ceil(text.length / 4)
  return { prompt_tokens: 0, completion_tokens: completion, total_tokens: completion, cost_usd: 0 }
}

/**
 * Handle a request under /api/v1/*. Returns true if the request was handled.
 * Mounted BEFORE the legacy route table in server.ts so v1 semantics
 * (error envelope, status codes) never leak through the legacy catch.
 */
export function createV1Handler(gateway: LLMGateway, hub: EventsHub) {
  return async function handleV1(
    req: http.IncomingMessage,
    res: http.ServerResponse,
    pathname: string,
    method: string,
  ): Promise<boolean> {
    if (pathname !== '/api/v1' && !pathname.startsWith('/api/v1/')) return false

    if (method === 'GET' && pathname === '/api/v1/health') {
      send(res, 200, { status: 'ok', version: PKG.version, backend: 'node' })
      return true
    }

    if (method === 'GET' && pathname === '/api/v1/models') {
      const entries = await gateway.listModels()
      const models = entries.map((m) => ({
        id: m.id,
        provider: 'omlx',
        tier: 'local',
        loaded: m.engine === 'loaded' || m.status === 'loaded',
      }))
      send(res, 200, { models })
      return true
    }

    if (method === 'GET' && pathname === '/api/v1/usage') {
      const entries = gateway.getUsage()
      const completion = entries.reduce((sum, e) => sum + e.tokens, 0)
      send(res, 200, {
        prompt_tokens: 0,
        completion_tokens: completion,
        total_tokens: completion,
        cost_usd: 0,
      } satisfies UsageBody)
      return true
    }

    if (method === 'POST' && pathname === '/api/v1/chat') {
      let body: ChatBody
      try {
        body = (await readBodyLimited(req)) as ChatBody
      } catch (err) {
        if (err instanceof PayloadTooLargeError) {
          sendError(res, 413, 'payload_too_large', err.message)
          return true
        }
        throw err
      }
      if (!isValidChatBody(body)) {
        sendError(res, 400, 'invalid_request', 'messages must be a non-empty array of {role, content}')
        return true
      }
      // Transient run: session_id null, normal run lifecycle events (SPEC-002 §2)
      const runId = `run_${ulid()}`
      hub.broadcast({ type: 'run.started', payload: { run_id: runId, session_id: null, schedule_id: null } })
      try {
        const message = await gateway.chat(
          { messages: body.messages, model: body.model ?? undefined } as LLMRequest,
          'v1-chat',
        )
        const text = message.content ?? ''
        const usage = estimateUsage(text)
        hub.broadcast({ type: 'model.delta', payload: { run_id: runId, text } })
        hub.broadcast({ type: 'run.completed', payload: { run_id: runId, output: { text }, usage } })
        send(res, 200, { message, usage })
      } catch (err) {
        const statusCode = (err as { statusCode?: number }).statusCode ?? 500
        const code = statusCode === 503 ? 'provider_unavailable' : 'internal'
        const msg = err instanceof Error ? err.message : 'Internal error'
        hub.broadcast({ type: 'run.failed', payload: { run_id: runId, error: { code, message: msg } } })
        sendError(res, statusCode, code, msg)
      }
      return true
    }

    sendError(res, 404, 'not_found', `No v1 route: ${method} ${pathname}`)
    return true
  }
}
