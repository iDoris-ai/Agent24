// M5 B3 (#1717): main's daemon requests in service mode, ported from the
// CLI's `send_elevating` (agent24-cli main.rs). The daemon the LaunchAgent
// runs gave this app a daily FirstPartyShell token; on an Elevated route
// (route_policy.rs, the only place that decides which) it answers
// `403 presence_required`. Then, once: ask for a grant on the token
// connection (the daemon puts the challenge to the presence connection, where
// main shows the native box), and retry the same request with the grant in
// `X-Agent24-Elevation`. The grant lives 60 s, is single use and binds
// method, path and body, so the retry sends exactly the bytes hashed.
//
// Only main sets the header: renderer requests carry no headers of their own
// (BackendProxyRequest), and the grant never leaves this function. With our
// own child (host token, presence over HTTP) or a legacy daemon there is no
// enrolment and this is exactly one plain request.

import type { BackendProxyRequest, BackendProxyResponse } from '../shared/ipc-types'
import { proxyError, proxyRequest, type ProxyEndpoint, type ProxyOptions } from './backend-proxy'
import { ELEVATION_HEADER, EnrolError, type EnrolSession } from './daemon-enroll'

export interface ElevatingDeps {
  connection: () => ProxyEndpoint | null
  /** The enrolment's elevate, or null when there is none (backend-manager `daemonElevation`). */
  elevation: () => EnrolSession['elevate'] | null
  send?: typeof proxyRequest
}

/** The daemon's answer that this request needs the user's confirmation. */
export function presenceRequired(res: BackendProxyResponse): boolean {
  const error = (res.data as { error?: { code?: unknown } } | null)?.error
  return res.status === 403 && error?.code === 'presence_required'
}

/** The line main asks the daemon to show; the daemon writes its own for the
 * operations it knows (K-B3), so this is only the fallback. */
export function elevationSummary(req: BackendProxyRequest): string {
  return `Agent24: ${req.method} ${req.path}`
}

/** Send `req`; on `403 presence_required` with an enrolment, elevate once and
 * retry once. Never rejects. A confirmation that does not come (denied,
 * timed out, no presence connection) answers 403 with the daemon's code. */
export async function sendElevating(
  req: BackendProxyRequest,
  opts: ProxyOptions,
  deps: ElevatingDeps,
): Promise<BackendProxyResponse> {
  const send = deps.send ?? proxyRequest
  const conn = deps.connection()
  if (!conn) return proxyError('backend_not_ready', 'Backend daemon is not ready (no announced port or credential)')
  const first = await send(conn, req, opts)
  const elevate = deps.elevation()
  // The daemon refuses a grant on a request with a query: it could not be described.
  if (!elevate || !presenceRequired(first) || /[?#]/.test(req.path)) return first
  // As proxyRequest serializes it, so the hash is of the bytes the retry sends.
  let body: string
  try {
    body = req.body === undefined ? '' : (JSON.stringify(req.body) ?? '')
  } catch {
    return first
  }
  try {
    return await elevate({ method: req.method, path: req.path, body, summary: elevationSummary(req) }, async (grant) => {
      // The token now, under the same exclusive turn as the grant: no renew between.
      const now = deps.connection()
      if (!now) return proxyError('backend_not_ready', 'Backend daemon is not ready (no announced port or credential)')
      return send({ port: now.port, authHeaders: { ...now.authHeaders, [ELEVATION_HEADER]: grant } }, req, opts)
    })
  } catch (error) {
    const code = error instanceof EnrolError ? error.code : 'internal'
    return { ok: false, status: 403, data: { error: { code, message: 'The action was not confirmed' } } }
  }
}
