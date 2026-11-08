// KB-C2-04a (#1623): the host side of the UI session (CONTRACT-UI-BRIDGE-v1).
// This file holds the parts that do not talk to the server yet: reading the
// exchange, probe and error bodies the way UIB says (U3.8 "解码"), and the
// per-binding ledger that decides when a view-level rebuild is still automatic
// (U3.8, decisions #3 and #36). The HTTP calls, injection and renewal come in
// the same slice once WeKnora C2-01b/c land.

import { UI_OPS, type UiOp } from './knowledge-ui-routes'

/** U3.8's window: the last 10 minutes of the host's monotonic clock. */
export const UI_SESSION_WINDOW_MS = 10 * 60 * 1000
/** Decision #36: restart events left out of the count per binding and window. */
export const UI_RESTART_EXEMPT_LIMIT = 3

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v)

// ── Bodies ──────────────────────────────────────────────────────────────────

export interface UiExchange {
  sessionToken: string
  expiresInS: number
  ops: UiOp[]
  knowledgeBaseIds: string[]
}

/**
 * A 200 from the exchange (Weknora C2-01b2: `{session_token, expires_in, ops,
 * effective_scope: {knowledge_base_ids}}`). Unknown fields are ignored; null
 * when anything the host relies on is off: `ops` outside the closed set, an
 * empty scope, or a knowledge base the settings do not bind. Not injected then.
 */
export function readUiExchange(body: unknown, boundKnowledgeBaseIds: readonly string[]): UiExchange | null {
  if (!isObject(body)) return null
  const { session_token: token, expires_in: expires, ops, effective_scope: scope } = body
  if (typeof token !== 'string' || token === '') return null
  if (typeof expires !== 'number' || !Number.isInteger(expires) || expires <= 0) return null
  if (!Array.isArray(ops) || ops.some((op) => typeof op !== 'string' || !(UI_OPS as readonly string[]).includes(op))) return null
  if (new Set(ops).size !== ops.length) return null
  const ids = isObject(scope) ? scope.knowledge_base_ids : undefined
  if (!Array.isArray(ids) || ids.length === 0 || ids.some((id) => typeof id !== 'string')) return null
  if (new Set(ids).size !== ids.length || ids.some((id) => !boundKnowledgeBaseIds.includes(id as string))) return null
  return { sessionToken: token, expiresInS: expires, ops: ops as UiOp[], knowledgeBaseIds: ids as string[] }
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

  /** The user's way out of `closed` (§9.2) starts the binding afresh. */
  reset(): void {
    this.counted = []
    this.exempted = []
    this.lastRestartEventAt = null
  }
}
