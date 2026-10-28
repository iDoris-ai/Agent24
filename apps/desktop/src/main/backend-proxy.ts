// The renderer's window onto the daemon (`backendProxy` IPC), hardened for the
// pre-release review (R1 / C-01 / S-02):
//   - A1: only (method, path-template) pairs in ALLOWED_PROXY_ROUTES are
//         forwarded. Elevated operations are NOT here; they have dedicated IPC
//         channels that ask the user through a native dialog (ipc/index.ts).
//   - A2: 30 s deadline, 8 MiB response cap, one error shape for all failures.
// Pure of Electron imports so the whole thing is unit tested against a real
// loopback http server (backend-proxy.test.ts).

import http from 'node:http'
import type { BackendProxyRequest, BackendProxyResponse, HttpMethod } from '../shared/ipc-types'

export const PROXY_TIMEOUT_MS = 30_000
export const PROXY_MAX_BODY_BYTES = 8 * 1024 * 1024
/** `POST /api/v1/chat` is one blocking call today (no streaming); a local model
 *  can legitimately take minutes, so it gets its own, longer deadline. */
export const CHAT_TIMEOUT_MS = 10 * 60_000

export interface ProxyRoute {
  method: HttpMethod
  /** Same syntax as the daemon's axum `MatchedPath` templates in
   *  `rust/apps/agent24d/src/capabilities/route_policy.rs` (POLICIES; that file lives on the `ab/core` branch and is not in main yet): one
   *  `{param}` = exactly one non-empty path segment. */
  template: string
  timeoutMs?: number
}

const r = (method: HttpMethod, template: string, timeoutMs?: number): ProxyRoute =>
  timeoutMs === undefined ? { method, template } : { method, template, timeoutMs }

/**
 * Allowlist = every route the renderer actually calls through `backendProxy`
 * (reads and low-risk writes). Templates and method spellings are copied from
 * the `POLICIES` table in route_policy.rs; the renderer-side call sites are
 * pages/agent/api.ts, pages/comm/api.ts, pages/voice/api.ts, pages/Chat.tsx,
 * pages/Models.tsx and App.tsx.
 *
 * Deliberately ABSENT (elevated in route_policy.rs; renderer must use a
 * dedicated IPC with a native confirmation, or has no business calling them):
 *   POST   /api/v1/approvals/{id}            -> IpcChannels.ApprovalDecide
 *   POST   /api/v1/module-approvals/{id}     -> (no renderer caller yet)
 *   POST   /api/v1/shutdown
 *   PUT|DELETE /api/v1/tool-overrides/{pattern}
 *   DELETE /api/v1/standing-grants/{id}
 *   POST   /api/v1/capabilities/*
 *   POST|PATCH|DELETE /api/v1/attached*
 *   PATCH  /api/v1/os/{name}                 -> IpcChannels.OsModuleSetEnabled
 *   POST   /api/v1/os/{name}/stop
 */
export const ALLOWED_PROXY_ROUTES: readonly ProxyRoute[] = [
  r('GET', '/api/v1/health'),
  r('GET', '/api/v1/models'),
  r('POST', '/api/v1/chat', CHAT_TIMEOUT_MS),
  r('GET', '/api/v1/usage'),
  // runs
  r('GET', '/api/v1/runs'),
  r('POST', '/api/v1/runs'),
  r('GET', '/api/v1/runs/{id}'),
  r('POST', '/api/v1/runs/{id}/cancel'),
  // schedules
  r('GET', '/api/v1/schedules'),
  // ELEVATED in route_policy.rs but deliberately still proxied (rows marked
  // [E] below): these are routine, user-initiated page actions, and a native
  // dialog per click would make the schedule / communication pages unusable.
  // Presence proof for elevated operations is done once, from the policy's
  // level metadata, by CORE-2d + CORE-7c; these rows are closed there.
  r('POST', '/api/v1/schedules'), // [E]
  r('PATCH', '/api/v1/schedules/{id}'), // [E]
  r('DELETE', '/api/v1/schedules/{id}'), // [E]
  r('POST', '/api/v1/schedules/{id}/run_now'), // [E]
  // approvals: read only (the decision goes through ApprovalDecide)
  r('GET', '/api/v1/approvals'),
  // domain OS / attached modules: read, plus module commands (speak, stop_playback)
  r('GET', '/api/v1/os'),
  r('POST', '/api/v1/os/{name}/commands/{command}'), // [E]
  r('GET', '/api/v1/attached'),
  // memory
  r('GET', '/api/v1/memory/assertions'),
  r('DELETE', '/api/v1/memory/assertions/{id}'),
  r('GET', '/api/v1/memory/settings'),
  r('PUT', '/api/v1/memory/settings'), // [E]
  // communication (Hyphae)
  r('GET', '/api/v1/comm/identity'),
  r('GET', '/api/v1/comm/contact'),
  r('GET', '/api/v1/comm/relay'),
  r('GET', '/api/v1/comm/daemon'),
  r('POST', '/api/v1/comm/daemon/start'), // [E]
  r('POST', '/api/v1/comm/daemon/stop'), // [E]
  r('POST', '/api/v1/comm/unlock'), // [E]
]

/** A path segment value a `{param}` may bind: no traversal, no separators,
 *  even after one round of percent-decoding. */
function isSafeParam(raw: string): boolean {
  if (raw === '') return false
  let decoded: string
  try {
    decoded = decodeURIComponent(raw)
  } catch {
    return false
  }
  return decoded !== '.' && decoded !== '..' && !/[/\\\0]/.test(decoded)
}

/** Does `pathname` (no query) match `template`? `{param}` = one safe segment. */
export function matchTemplate(template: string, pathname: string): boolean {
  const want = template.split('/')
  const got = pathname.split('/')
  if (want.length !== got.length) return false
  for (let i = 0; i < want.length; i += 1) {
    const w = want[i] as string
    const g = got[i] as string
    if (w.startsWith('{') && w.endsWith('}')) {
      if (!isSafeParam(g)) return false
    } else if (w !== g) {
      return false
    }
  }
  return true
}

/** The matching allowlist entry, or null. The query string is not matched. */
export function findAllowedRoute(method: string, requestPath: string): ProxyRoute | null {
  // Control characters (CR/LF/NUL...) must never reach the request line.
  // eslint-disable-next-line no-control-regex
  const hasControl = /[\u0000-\u001f\u007f]/.test(requestPath)
  if (!requestPath.startsWith('/') || requestPath.includes('#') || hasControl) return null
  const q = requestPath.indexOf('?')
  const pathname = q === -1 ? requestPath : requestPath.slice(0, q)
  return (
    ALLOWED_PROXY_ROUTES.find(
      (rt) => rt.method === method && matchTemplate(rt.template, pathname),
    ) ?? null
  )
}

export function isAllowedProxyRoute(method: string, requestPath: string): boolean {
  return findAllowedRoute(method, requestPath) !== null
}

export type ProxyErrorCode =
  | 'invalid_request'
  | 'forbidden'
  | 'timeout'
  | 'payload_too_large'
  | 'proxy_failed'
  | 'backend_not_ready'

const ERROR_STATUS: Record<ProxyErrorCode, number> = {
  invalid_request: 400,
  forbidden: 403,
  timeout: 504,
  payload_too_large: 502,
  proxy_failed: 502,
  backend_not_ready: 503,
}

export function proxyError(code: ProxyErrorCode, message: string): BackendProxyResponse {
  return { ok: false, status: ERROR_STATUS[code], data: { error: { code, message } } }
}

/** The daemon's port and the headers that authenticate main to it (the shape
 *  of backend-manager's `DaemonConnection`; the credential never travels as a
 *  bare token). */
export interface ProxyEndpoint {
  port: number
  authHeaders: Record<string, string>
}

export interface ProxyOptions {
  host?: string
  timeoutMs?: number
  maxBytes?: number
}

/** Forwards one request to the daemon. Never rejects. No allowlist check here:
 *  callers (the generic handler, or a dedicated IPC) decide what may be sent. */
export function proxyRequest(
  endpoint: ProxyEndpoint,
  req: BackendProxyRequest,
  opts: ProxyOptions = {},
): Promise<BackendProxyResponse> {
  const timeoutMs = opts.timeoutMs ?? PROXY_TIMEOUT_MS
  const maxBytes = opts.maxBytes ?? PROXY_MAX_BODY_BYTES
  return new Promise((resolve) => {
    let body: string | undefined
    try {
      body = req.body !== undefined ? JSON.stringify(req.body) : undefined
    } catch (err) {
      // BigInt, cycles, throwing toJSON: refuse before any timer or socket exists.
      resolve(
        proxyError(
          'invalid_request',
          `request body is not JSON-serializable: ${(err as Error).message}`,
        ),
      )
      return
    }
    let settled = false
    // Hard overall deadline (a daemon dripping one byte at a time would defeat
    // a socket-idle timeout alone). `onTimeout` is hoisted; it only runs later.
    const deadline = setTimeout(() => onTimeout(), timeoutMs)
    const finish = (value: BackendProxyResponse): void => {
      if (settled) return
      settled = true
      clearTimeout(deadline)
      resolve(value)
    }

    let outReq: http.ClientRequest | undefined
    try {
      outReq = http.request(
        {
          host: opts.host ?? '127.0.0.1',
          port: endpoint.port,
          path: req.path,
          method: req.method,
          headers: {
            'Content-Type': 'application/json',
            ...endpoint.authHeaders,
            ...(body ? { 'Content-Length': Buffer.byteLength(body) } : {}),
          },
        },
        (res) => {
          const declared = Number(res.headers['content-length'])
          if (Number.isFinite(declared) && declared > maxBytes) {
            finish(proxyError('payload_too_large', `daemon response exceeds ${maxBytes} bytes`))
            outReq?.destroy()
            return
          }
          const chunks: Buffer[] = []
          let received = 0
          res.on('data', (chunk: Buffer) => {
            received += chunk.length
            if (received > maxBytes) {
              finish(proxyError('payload_too_large', `daemon response exceeds ${maxBytes} bytes`))
              outReq?.destroy()
              return
            }
            chunks.push(chunk)
          })
          res.on('end', () => {
            let data: unknown = null
            try {
              data = JSON.parse(Buffer.concat(chunks).toString())
            } catch {
              data = null
            }
            const status = res.statusCode ?? 500
            finish({ ok: status < 400, status, data })
          })
          res.on('error', (err) => finish(proxyError('proxy_failed', err.message)))
        },
      )
    } catch (err) {
      finish(proxyError('proxy_failed', (err as Error).message))
      return
    }

    function onTimeout(): void {
      finish(proxyError('timeout', `daemon did not answer within ${timeoutMs} ms`))
      outReq?.destroy()
    }
    const sent = outReq
    sent.setTimeout(timeoutMs, onTimeout)

    sent.on('error', (err) => finish(proxyError('proxy_failed', err.message)))
    if (body) sent.write(body)
    sent.end()
  })
}

/** Effective deadline for a request that already passed the allowlist. */
export function timeoutFor(route: ProxyRoute | null): number {
  return route?.timeoutMs ?? PROXY_TIMEOUT_MS
}
