// Documenting OS desktop channel (ADR-DOC-01 D5): `window.agent24.documents.*`.
// The renderer names an operation, never a path: main maps each operation to
// a fixed route template under /api/v1/documents, checks every id against its
// pattern first, and does not go through the generic `backendProxy`.

import http from 'node:http'
import type { BackendEndpoint } from './backend-manager'
import type { ModuleEvent } from './agentear-events'
import type { DocumentsError, DocumentsEvent, DocumentsResponse } from '../shared/ipc-types'

const BASE = '/api/v1/documents'
const DOCUMENT_ID = /^doc_[0-7][0-9A-HJKMNP-TV-Z]{25}$/
const JOB_ID = /^job_[0-7][0-9A-HJKMNP-TV-Z]{25}$/
const CURSOR = /^[0-9a-f]{1,4096}$/
/** Longer than the kernel proxy's 30 s total, so the proxy answers first. */
const TIMEOUT_MS = 35_000
/** Above the proxy's 1 MiB response cap: only a broken daemon reaches it. */
const MAX_REPLY_BYTES = 2 * 1024 * 1024

export interface Route {
  method: 'GET' | 'POST'
  path: string
}

/** The fixed route for a request, or null if the request is not one of the
 * allowed operations or carries a malformed id, cursor or limit. */
export function documentsRoute(req: unknown): Route | null {
  if (typeof req !== 'object' || req === null) return null
  const r = req as Record<string, unknown>
  const op = r['op'] // read once
  const id = (key: string, pattern: RegExp): string | null => {
    const v = r[key]
    return typeof v === 'string' && pattern.test(v) ? v : null
  }
  switch (op) {
    case 'capabilities':
      return { method: 'GET', path: `${BASE}/capabilities` }
    case 'list': {
      const query = new URLSearchParams()
      const { cursor, limit } = r
      if (cursor !== undefined) {
        if (typeof cursor !== 'string' || !CURSOR.test(cursor)) return null
        query.set('cursor', cursor)
      }
      if (limit !== undefined) {
        if (typeof limit !== 'number' || !Number.isInteger(limit) || limit < 1 || limit > 200) return null
        query.set('limit', String(limit))
      }
      const qs = query.toString()
      return { method: 'GET', path: `${BASE}/documents${qs ? `?${qs}` : ''}` }
    }
    case 'get': {
      const documentId = id('documentId', DOCUMENT_ID)
      return documentId ? { method: 'GET', path: `${BASE}/documents/${documentId}` } : null
    }
    case 'job':
    case 'cancelJob':
    case 'retryJob': {
      const jobId = id('jobId', JOB_ID)
      if (!jobId) return null
      if (op === 'job') return { method: 'GET', path: `${BASE}/jobs/${jobId}` }
      return { method: 'POST', path: `${BASE}/jobs/${jobId}/${op === 'cancelJob' ? 'cancel' : 'retry'}` }
    }
    default:
      return null
  }
}

export interface SendOptions {
  headers?: Record<string, string>
  body?: Buffer
}

/** The daemon's reply, or a transport failure main describes itself. */
export type Reply = { status: number; body: Buffer } | { status: number; error: DocumentsError }

/** Sends one request to the daemon. */
export type Sender = (route: Route, options?: SendOptions) => Promise<Reply>

function failed(status: number, code: string, message: string): Reply {
  return { status, error: { code, message } }
}

/** A sender for the daemon at `endpoint()`. It settles exactly once: on the
 * whole reply, on a broken or truncated one, after `timeoutMs` (the outcome
 * of a POST is then unknown; it is never retried here), or once the reply
 * passes `maxBytes`. */
export function httpSender(
  endpoint: () => BackendEndpoint | null,
  host = '127.0.0.1',
  timeoutMs = TIMEOUT_MS,
  maxBytes = MAX_REPLY_BYTES,
): Sender {
  return (route, options = {}) =>
    new Promise((resolve) => {
      const ep = endpoint()
      if (!ep) {
        resolve(failed(503, 'backend_not_ready', 'Backend daemon has not announced readiness yet'))
        return
      }
      let settled = false
      const settle = (reply: Reply): void => {
        if (settled) return
        settled = true
        clearTimeout(timer)
        outReq.destroy()
        resolve(reply)
      }
      const headers: Record<string, string | number> = { ...options.headers }
      if (ep.token) headers['Authorization'] = `Bearer ${ep.token}`
      if (options.body) headers['Content-Length'] = options.body.length
      const outReq = http.request({ host, port: ep.port, path: route.path, method: route.method, headers }, (res) => {
        const chunks: Buffer[] = []
        let size = 0
        const broken = (): void => settle(failed(502, 'backend_unreachable', 'the reply was cut short'))
        res.on('data', (chunk: Buffer) => {
          size += chunk.length
          if (size > maxBytes) settle(failed(502, 'invalid_response', 'the reply is too large'))
          else chunks.push(chunk)
        })
        res.on('end', () => {
          if (res.complete) settle({ status: res.statusCode ?? 500, body: Buffer.concat(chunks) })
          else broken()
        })
        res.on('aborted', broken)
        res.on('error', broken)
        res.on('close', () => {
          if (!res.complete) broken()
        })
      })
      const timer = setTimeout(
        () => settle(failed(504, 'backend_timeout', 'no reply in time; the outcome is unknown')),
        timeoutMs,
      )
      outReq.on('error', (err) => settle(failed(503, 'backend_unreachable', err.message)))
      if (options.body) outReq.write(options.body)
      outReq.end()
    })
}

function isError(v: unknown): v is DocumentsError {
  if (typeof v !== 'object' || v === null) return false
  const e = v as Record<string, unknown>
  return typeof e['code'] === 'string' && typeof e['message'] === 'string'
}

/** The reply as the page sees it: a 2xx JSON body is the resource; a 4xx or
 * 5xx carries the error envelope; anything else (a redirect, a body that is
 * not JSON, an error without its envelope) is `invalid_response`. */
export function toResponse(reply: Reply): DocumentsResponse {
  if ('error' in reply) return { ok: false, status: reply.status, error: reply.error }
  const { status, body } = reply
  let parsed: unknown
  try {
    parsed = JSON.parse(body.toString('utf8'))
  } catch {
    parsed = undefined
  }
  if (status >= 200 && status < 300 && parsed !== undefined && parsed !== null) {
    return { ok: true, status, data: parsed as never }
  }
  const envelope = (parsed as { error?: unknown } | undefined)?.error
  if (status >= 400 && status < 600 && isError(envelope)) return { ok: false, status, error: envelope }
  return { ok: false, status, error: { code: 'invalid_response', message: `unexpected ${status} reply from the daemon` } }
}

/** Runs one allowed operation; anything else is refused here, unsent. */
export async function documentsCall(req: unknown, send: Sender): Promise<DocumentsResponse> {
  const route = documentsRoute(req)
  if (!route) return toResponse(failed(400, 'invalid_request', 'not an allowed documents operation'))
  return toResponse(await send(route))
}

/** The Documenting OS's event in a WS module event, or null: another
 * module's, a kind this build does not know, or one without the id the page
 * reads by. Only that id is checked: the page reads the job or the list
 * again, so the rest is never trusted. */
export function documentsEvent(event: ModuleEvent): DocumentsEvent | null {
  if (event.module !== 'documents') return null
  const { kind, payload } = event
  const has = (key: string, pattern: RegExp): boolean => {
    const v = payload[key]
    return typeof v === 'string' && pattern.test(v)
  }
  switch (kind) {
    case 'job.progress':
    case 'job.finished':
      return has('job_id', JOB_ID) ? ({ kind, payload } as DocumentsEvent) : null
    case 'document.imported':
    case 'revision.committed':
      return has('document_id', DOCUMENT_ID) ? ({ kind, payload } as DocumentsEvent) : null
    default:
      return null
  }
}
