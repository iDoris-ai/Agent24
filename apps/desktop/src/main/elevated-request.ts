// M5 B3 (#1717): main's daemon requests in service mode, ported from the
// CLI's `send_elevating` (agent24-cli main.rs). The daemon the LaunchAgent
// runs gave this app a daily FirstPartyShell token; on an Elevated route
// (route_policy.rs, the only place that decides which) it answers
// `403 presence_required`. Then, once, in one turn on the token connection
// (no renew in it): the request again with the token of now, as the daemon
// remembers it by token; a grant (the daemon puts the challenge to the
// presence connection, where main shows the native box); and the retry with
// the grant in `X-Agent24-Elevation`. The grant lives 60 s, is single use and
// binds method, path, body and token, so the retry sends exactly the bytes hashed.
//
// Only main sets the header: renderer requests carry no headers of their own
// (BackendProxyRequest), and the grant never leaves this function. With our
// own child (host token, presence over HTTP) or a legacy daemon there is no
// enrolment and this is exactly one plain request.

import type { BackendProxyRequest, BackendProxyResponse } from '../shared/ipc-types'
import { proxyError, proxyRequest, type ProxyEndpoint, type ProxyOptions } from './backend-proxy'
import { ELEVATION_HEADER, EnrolError, type EnrolSession } from './daemon-enroll'

/** The one switch for approvals in service mode (General 11-09). There
 * `POST /approvals/{id}` is Elevated, so the daemon's presence box asks for
 * it, with K-B3b's ApprovalDecision summary (decision, run, tool): one box per
 * approval, as with our own child. ApprovalDecide's own box would be a second
 * one with the same facts, so it is skipped. False brings it back (two boxes).
 * Our own child is never affected: no presence asked, its own box stays.
 * This rests on the daemon keeping `POST /approvals/{id}` Elevated
 * (route_policy.rs), pinned there by rust/apps/agent24d/src/capabilities/
 * tests/route_policy.rs (`ApprovalDecision` → Elevated, and every row against
 * the design table): a daemon that lowered it would run approvals unasked. */
export const SKIP_APPROVAL_CONFIRM_IN_SERVICE_MODE = true

/** Does ApprovalDecide show its own box? Always with our own child; in
 * service mode (an enrolment to elevate on) unless the switch is on. */
export function approvalConfirmShown(serviceMode: boolean, skip: boolean = SKIP_APPROVAL_CONFIRM_IN_SERVICE_MODE): boolean {
  return !(serviceMode && skip)
}

export interface ElevatingDeps {
  connection: () => ProxyEndpoint | null
  /** A turn on the enrolment's token connection, or null when there is none (backend-manager `daemonElevation`). */
  elevation: () => EnrolSession['turn'] | null
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
  const turn = deps.elevation()
  // The daemon refuses a grant on a request with a query: it could not be described.
  if (!turn || !presenceRequired(first) || /[?#]/.test(req.path)) return first
  // As proxyRequest serializes it, so the hash is of the bytes the retry sends.
  let body: string
  try {
    body = req.body === undefined ? '' : (JSON.stringify(req.body) ?? '')
  } catch {
    return first
  }
  try {
    return await turn(async (elevate, token) => {
      // A renew may have come since the first try, and another request of the
      // same template may have been remembered over it: so again, now, with
      // the token the grant will be bound to (Codex B3b C1). Only on this
      // enrolment's connection: one published since (our own child's host
      // token, which no presence check constrains) would just run it (r2 C1).
      const now = deps.connection()
      if (now?.authHeaders['Authorization'] !== `Bearer ${token}`) {
        return proxyError('backend_not_ready', 'The daemon connection changed; try again')
      }
      const again = await send(now, req, opts)
      if (!presenceRequired(again)) return again
      const grant = await elevate({ method: req.method, path: req.path, body, summary: elevationSummary(req) })
      return send({ port: now.port, authHeaders: { ...now.authHeaders, [ELEVATION_HEADER]: grant } }, req, opts)
    })
  } catch (error) {
    const code = error instanceof EnrolError ? error.code : 'internal'
    return { ok: false, status: 403, data: { error: { code, message: 'The action was not confirmed' } } }
  }
}
