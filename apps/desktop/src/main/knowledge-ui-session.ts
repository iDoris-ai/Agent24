// KB-C2-04a (#1623): the host side of the UI session (CONTRACT-UI-BRIDGE-v1).
// This file holds the parts that do not talk to the server: reading the
// exchange, probe and error bodies the way UIB says (U3.8 "解码"), and the
// per-binding ledger that decides when a view-level rebuild is still automatic
// (U3.8, decisions #3 and #36). The calls are knowledge-ui-client.ts, one
// instance's session knowledge-ui-view-session.ts, injection
// knowledge-ui-inject.ts, and knowledge-ui-instance.ts ties them to the view.

import { UI_OPS, type UiOp } from './knowledge-ui-routes'

/** U3.8's window: the last 10 minutes of the host's monotonic clock. */
export const UI_SESSION_WINDOW_MS = 10 * 60 * 1000
/** Decision #36: restart events left out of the count per binding and window. */
export const UI_RESTART_EXEMPT_LIMIT = 3
/** U3.8: the third "probe said revoked, yet issuing with C worked" in the window closes the binding's views. */
export const UI_REVOCATION_MISMATCH_LIMIT = 3

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v)

// ── Bodies ──────────────────────────────────────────────────────────────────

export interface UiExchange {
  sessionToken: string
  expiresInS: number
  ops: UiOp[]
  knowledgeBaseIds: string[]
  /** How many listed ops fell outside the closed set and were dropped (a count only, never the names). */
  droppedOps: number
}

/**
 * A 200 from the exchange (Weknora C2-01b2: `{session_token, expires_in, ops,
 * effective_scope: {knowledge_base_ids}}`). Unknown fields are ignored; null
 * when anything the host relies on is off: `ops` is not an array of strings, a
 * known op is given twice, or a scope that is empty, repeats an id or holds a
 * non-string. Not injected then.
 *
 * An op outside the closed set is dropped, not a reason to refuse the whole
 * answer (READONLY §0.3 receiver side; it has no host route, so it gets no
 * session). The duplicate check runs before that drop and only on known ops:
 * a known op given twice still refuses it; a repeated unknown op does not.
 * Kept ops stay in order and may be empty. How many were dropped goes back as
 * `droppedOps`; this module logs nothing (U2.5), knowledge-view.ts logs the
 * count (David 2026-11-11; decisions/2026-11-11-c2-exchange-unknown-ops and
 * 2026-11-11-c2-ops-drop-log-placement).
 *
 * The scope E is the server's to compute from C (U2.1: the ticket carries no
 * Q), so it is taken as it comes and not held against the bases chosen in
 * Settings: the knowledge page shows every base the key can see, while chat
 * retrieval still asks only for the chosen ones (David, 2026-11-08, option C).
 */
export function readUiExchange(body: unknown): UiExchange | null {
  if (!isObject(body)) return null
  const { session_token: token, expires_in: expires, ops, effective_scope: scope } = body
  if (typeof token !== 'string' || token === '') return null
  if (typeof expires !== 'number' || !Number.isInteger(expires) || expires <= 0) return null
  if (!Array.isArray(ops) || ops.some((op) => typeof op !== 'string')) return null
  const seen = new Set<string>()
  for (const op of ops as string[]) {
    if (!(UI_OPS as readonly string[]).includes(op)) continue
    if (seen.has(op)) return null
    seen.add(op)
  }
  const known = (ops as string[]).filter((op) => seen.has(op)) as UiOp[]
  const ids = isObject(scope) ? scope.knowledge_base_ids : undefined
  if (!Array.isArray(ids) || ids.length === 0 || ids.some((id) => typeof id !== 'string')) return null
  if (new Set(ids).size !== ids.length) return null
  return { sessionToken: token, expiresInS: expires, ops: known, knowledgeBaseIds: ids as string[], droppedOps: ops.length - known.length }
}

/** A 200 from the session probe: `{expires_in}`, whole seconds, ≥ 0. */
export function readUiProbe(body: unknown): { expiresInS: number } | null {
  if (!isObject(body)) return null
  const expires = body.expires_in
  return typeof expires === 'number' && Number.isInteger(expires) && expires >= 0 ? { expiresInS: expires } : null
}

export type UiErrorRead =
  | { kind: 'error'; code: string; reason: string | null; restarted: boolean; retryAfterMs: number | null }
  | { kind: 'unknown' }

/**
 * An error body on a UI path (R6.1 shape, U2.7, U3.4). Decoded here, not with
 * the READONLY wire types, which do not take `details.restarted`. `restarted`
 * is read only from the session probe and only next to `credential_invalid`;
 * there a non-boolean makes the whole answer unknown (U3.8). Anywhere else it
 * is ignored and the reason decides.
 */
export function readUiError(body: unknown, from: 'probe' | 'exchange' | 'resource'): UiErrorRead {
  const error = isObject(body) ? body.error : undefined
  if (!isObject(error) || typeof error.code !== 'string') return { kind: 'unknown' }
  const details = isObject(error.details) ? error.details : {}
  const reason = typeof details.reason === 'string' ? details.reason : null
  let restarted = false
  if (from === 'probe' && error.code === 'unauthorized' && reason === 'credential_invalid' && 'restarted' in details) {
    if (typeof details.restarted !== 'boolean') return { kind: 'unknown' }
    restarted = details.restarted
  }
  const after = details.retry_after_ms
  const retryAfterMs = typeof after === 'number' && Number.isFinite(after) && after >= 0 ? after : null
  return { kind: 'error', code: error.code, reason, restarted, retryAfterMs }
}

// ── The per-binding ledger (U3.8, decisions #3 and #36) ─────────────────────

export type ViewLevelCause =
  | { kind: 'invalid' }
  /** A probe's `credential_invalid` with `restarted: true`, for a view whose JWT was exchanged at `jwtExchangedAt`. */
  | { kind: 'restarted'; jwtExchangedAt: number }

export interface ViewLevelDecision {
  /** Rebuild the view (new instance, new vph), or stop: `closed/session_rejected`. */
  action: 'rebuild' | 'closed'
  /** Merged into a restart event already seen: nothing is spent or counted. */
  merged: boolean
  /** Spent one of the window's restart exemptions. */
  exempt: boolean
}

/**
 * One per binding, shared by its views. Every view-level handling caused by
 * `credential_invalid` goes through `handle`; crashes, Off→On, port changes and
 * channel recovery do not (they are not counted at all). `now` is the host's
 * monotonic clock, and for a restarted probe the moment its answer arrived.
 */
export class UiSessionLedger {
  private counted: number[] = []
  private exempted: number[] = []
  private lastRestartEventAt: number | null = null
  private mismatches: number[] = []

  handle(cause: ViewLevelCause, now: number): ViewLevelDecision {
    const since = now - UI_SESSION_WINDOW_MS
    this.counted = this.counted.filter((t) => t > since)
    this.exempted = this.exempted.filter((t) => t > since)
    if (cause.kind === 'restarted') {
      // Same restart as the last event: this view's JWT is older than it, so
      // the rebuild belongs to that event (one restart, several views).
      if (this.lastRestartEventAt !== null && cause.jwtExchangedAt <= this.lastRestartEventAt) {
        return { action: 'rebuild', merged: true, exempt: false }
      }
      this.lastRestartEventAt = now
      if (this.exempted.length < UI_RESTART_EXEMPT_LIMIT) {
        this.exempted.push(now)
        return { action: 'rebuild', merged: false, exempt: true }
      }
      // Over the limit: this event counts once, like a plain credential_invalid.
    }
    this.counted.push(now)
    return { action: this.counted.length >= 2 ? 'closed' : 'rebuild', merged: false, exempt: false }
  }

  /**
   * U3.8: a probe answered `credential_revoked` and issuing a ticket with the
   * current C then worked (the view renewed). The third such event for this
   * binding within the window is `closed/revocation_mismatch`. Kept apart from
   * the view-level counts above.
   */
  revocationMismatch(now: number): 'renewed' | 'closed' {
    const since = now - UI_SESSION_WINDOW_MS
    this.mismatches = this.mismatches.filter((t) => t > since)
    this.mismatches.push(now)
    return this.mismatches.length >= UI_REVOCATION_MISMATCH_LIMIT ? 'closed' : 'renewed'
  }

  /** The user's way out of `closed` (§9.2) starts the binding afresh. */
  reset(): void {
    this.counted = []
    this.exempted = []
    this.lastRestartEventAt = null
    this.mismatches = []
  }
}
