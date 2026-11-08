// KB-C2-04a (#1623): one view instance's UI session (UIB U2.1, U2.9–U2.12,
// U3.6–U3.8). It mints a ticket with the current C, exchanges it once with the
// view proof, keeps the JWT in memory only, renews at about 2/3 of its life,
// probes on the page's 401, and revokes its family when the instance goes.
// What a result means for the view (rebuild, close, pause) is the host's call;
// the per-binding count is UiSessionLedger's.

import type { KnowledgeUiClient, UiCallResult, UiTicket, ViewSecret } from './knowledge-ui-client'
import type { UiExchange } from './knowledge-ui-session'

/** U3.8: consecutive unknown outcomes (timeout, 5xx, unknown reason) before `closed/probe_failed`. */
export const UI_UNKNOWN_LIMIT = 3
/** Back-off between attempts after an unknown outcome or `overloaded`, unless the server names one. */
export const UI_BACKOFF_MS = 1000

export interface UiViewSessionDeps {
  client: KnowledgeUiClient
  view: ViewSecret
  tags: { bindingTag: string; partitionTag: string }
  /** The current C, or null when the binding has none. */
  credential: () => string | null
  /** The binding's generation now; read at issuance and checked again before the exchange (U2.1 ①④). */
  generation: () => number
  /** The binding is servable and admission is open (U2.1 ①, U2.12). */
  servable: () => boolean
  boundKnowledgeBaseIds: () => readonly string[]
  now: () => number
  sleep: (ms: number) => Promise<void>
}

export type UiSessionOutcome =
  | { kind: 'ready' }
  /** U2.12: generation moved between issuance and exchange; the ticket and family are revoked, binding-level teardown. */
  | { kind: 'generation_changed' }
  /** U2.12: not servable any more; only this ticket is revoked, the view stays and injection pauses. */
  | { kind: 'not_servable' }
  /** U3.8: the second consecutive `credential_invalid` from the exchange: view-level handling (counted). */
  | { kind: 'view_level' }
  /** C expired, was revoked or is about to expire: the Provider renews or re-checks C (U2.1 ③, U2.14, S3.1), then the host starts again. */
  | { kind: 'credential'; reason: 'credential_expired' | 'credential_revoked' | 'credential_near_expiry' }
  | { kind: 'closed'; code: 'empty_scope' | 'probe_failed' }
  | { kind: 'destroyed' }

export class UiViewSession {
  private session: (UiExchange & { exchangedAt: number }) | null = null
  private flight: Promise<UiSessionOutcome> | null = null
  private destroyed = false

  constructor(private readonly deps: UiViewSessionDeps) {}

  /** The JWT to inject, if there is one; never logged or passed on (U2.5). */
  token(): string | null {
    return this.destroyed ? null : this.session?.sessionToken ?? null
  }

  /** When the current JWT was exchanged (UiSessionLedger's merge rule for restarts). */
  exchangedAt(): number | null {
    return this.session?.exchangedAt ?? null
  }

  /** Remaining life is at or below a third: renew (U3.6). */
  needsRenewal(): boolean {
    const s = this.session
    if (!s) return true
    return this.deps.now() - s.exchangedAt >= (s.expiresInS * 1000 * 2) / 3
  }

  /** Mint and exchange, at most one in flight per instance (U2.9). */
  establish(): Promise<UiSessionOutcome> {
    this.flight ??= this.run().finally(() => { this.flight = null })
    return this.flight
  }

  /** Ends the instance: forget the JWT and revoke the family, bounded, never awaited by teardown (U3.7). */
  destroy(): void {
    if (this.destroyed) return
    this.destroyed = true
    this.session = null
    const credential = this.deps.credential()
    if (credential) void this.deps.client.revokeFamily(credential, this.deps.view.vph)
  }

  private async run(): Promise<UiSessionOutcome> {
    let invalidInARow = 0
    let unknownInARow = 0
    for (;;) {
      if (this.destroyed) return { kind: 'destroyed' }
      const credential = this.deps.credential()
      if (!credential || !this.deps.servable()) return { kind: 'not_servable' }
      const generation = this.deps.generation()
      const issued = await this.deps.client.issueTicket(credential, { ...this.deps.tags, vph: this.deps.view.vph }, generation)
      const failed = this.failure(issued)
      if (failed === 'retry') {
        if (++unknownInARow >= UI_UNKNOWN_LIMIT) return { kind: 'closed', code: 'probe_failed' }
        await this.deps.sleep(this.backoff(issued))
        continue
      }
      if (failed) return failed
      const ticket = (issued as { kind: 'ok'; value: UiTicket }).value
      // U2.1 ④ / U2.12, checked again right before the exchange.
      if (this.destroyed) {
        void this.deps.client.revokeTicket(credential, ticket.ticketId)
        return { kind: 'destroyed' }
      }
      if (this.deps.generation() !== generation) {
        void this.deps.client.revokeTicket(credential, ticket.ticketId)
        void this.deps.client.revokeFamily(credential, this.deps.view.vph)
        return { kind: 'generation_changed' }
      }
      if (!this.deps.servable()) {
        void this.deps.client.revokeTicket(credential, ticket.ticketId)
        return { kind: 'not_servable' }
      }
      const exchanged = await this.deps.client.exchange(ticket.ticket, this.deps.view, this.deps.boundKnowledgeBaseIds())
      if (this.destroyed) return { kind: 'destroyed' }
      if (exchanged.kind === 'ok') {
        this.session = { ...exchanged.value, exchangedAt: this.deps.now() }
        return { kind: 'ready' }
      }
      // From here the ticket is never sent again: a new one is minted (U2.9, C2-01b2 replay).
      if (exchanged.kind === 'refused' && exchanged.error.code === 'unauthorized' && exchanged.error.reason === 'credential_invalid') {
        unknownInARow = 0
        if (++invalidInARow >= 2) return { kind: 'view_level' }
        continue
      }
      invalidInARow = 0
      const other = this.failure(exchanged)
      if (other === 'retry') {
        if (++unknownInARow >= UI_UNKNOWN_LIMIT) return { kind: 'closed', code: 'probe_failed' }
        await this.deps.sleep(this.backoff(exchanged))
        continue
      }
      if (other) return other
    }
  }

  /** null for ok; 'retry' for an outcome U3.8 retries; otherwise how this ends. */
  private failure(result: UiCallResult<unknown>): UiSessionOutcome | 'retry' | null {
    if (result.kind === 'ok') return null
    if (result.kind === 'unknown') return 'retry'
    const { code, reason } = result.error
    if (code === 'unauthorized' && (reason === 'credential_expired' || reason === 'credential_revoked')) return { kind: 'credential', reason }
    if (code === 'unavailable' && reason === 'credential_near_expiry') return { kind: 'credential', reason }
    if (code === 'forbidden' && reason === 'empty_scope') return { kind: 'closed', code: 'empty_scope' }
    // overloaded, near-expiry, other 5xx and every reason this host does not know: fail closed, retry bounded.
    return 'retry'
  }

  private backoff(result: UiCallResult<unknown>): number {
    const ms = result.kind === 'refused' ? result.error.retryAfterMs : null
    return ms !== null && ms > UI_BACKOFF_MS ? ms : UI_BACKOFF_MS
  }
}
