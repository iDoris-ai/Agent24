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
/** U3.8: a view instance's self-check runs at most this often. */
export const UI_SELF_CHECK_INTERVAL_MS = 10_000

export interface UiViewSessionDeps {
  client: KnowledgeUiClient
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
  /** U4.5 "恢复即重载": injection was paused and is not any more. */
  onResume?: () => void
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

/** What a self-check after an injected request's 401 found (U3.8); the establish outcomes come from a renewal. */
export type UiSelfCheckOutcome =
  | UiSessionOutcome
  /** The probe accepted the JWT: nothing to do (a probe-failure pause is lifted). */
  | { kind: 'ok' }
  /** Another self-check ran less than UI_SELF_CHECK_INTERVAL_MS ago. */
  | { kind: 'skipped' }
  /** The probe said `credential_revoked`, yet issuing with C worked and the view renewed: the host counts it (UiSessionLedger.revocationMismatch). */
  | { kind: 'revoked_renewed' }
  /** The probe said `credential_invalid`: view-level handling. `jwtExchangedAt` and `at` feed UiSessionLedger's restart rule. */
  | { kind: 'session_invalid'; restarted: boolean; jwtExchangedAt: number; at: number }

export class UiViewSession {
  private session: (UiExchange & { exchangedAt: number; generation: number }) | null = null
  /** U4.7 ②: dropped on destroy; only the family id (vph) is kept for the revocation. */
  private view: ViewSecret | null
  private readonly vph: string
  /** The C the family's tickets were issued with: the revocation uses it even once the binding moved on (U4.7 ③). */
  private issuedWith: string | null = null
  private flight: Promise<UiSessionOutcome> | null = null
  private destroyed = false
  /** U3.8: injection paused after an unknown probe or exchange; lifted by any other result. */
  private paused = false
  private check: Promise<UiSelfCheckOutcome> | null = null
  private lastCheckAt: number | null = null

  constructor(private readonly deps: UiViewSessionDeps, view: ViewSecret) {
    this.view = view
    this.vph = view.vph
  }

  /** The JWT to inject, if there is one; never logged or passed on (U2.5). */
  token(): string | null {
    return this.destroyed ? null : this.session?.sessionToken ?? null
  }

  /**
   * What the injector sends (U4.5 ⑤): the JWT and the view proof, or null while
   * paused, before the first exchange, once the JWT is past its life, or after
   * the instance is gone. Never logged or passed on (U2.5).
   */
  injection(): { token: string; proof: string } | null {
    const s = this.session
    const view = this.view
    if (this.destroyed || this.paused || !s || !view) return null
    if (this.deps.now() - s.exchangedAt >= s.expiresInS * 1000) return null
    // U4.8: not a request more once the binding is not servable or has moved on.
    if (!this.deps.servable() || this.deps.generation() !== s.generation) return null
    return { token: s.sessionToken, proof: view.proof }
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

  /** Milliseconds until needsRenewal() turns true; 0 when it already is. */
  renewalDelay(): number {
    const s = this.session
    if (!s) return 0
    return Math.max(0, s.exchangedAt + (s.expiresInS * 1000 * 2) / 3 - this.deps.now())
  }

  /** Mint and exchange, at most one in flight per instance (U2.9). */
  establish(): Promise<UiSessionOutcome> {
    this.flight ??= this.run().finally(() => { this.flight = null })
    return this.flight
  }

  /**
   * U3.8: an injected request got a 401. One self-check per instance at a time
   * and at most every UI_SELF_CHECK_INTERVAL_MS: renew if the JWT is at or near
   * its end, otherwise probe the session and act on the answer.
   */
  selfCheck(): Promise<UiSelfCheckOutcome> {
    if (this.check) return this.check
    const now = this.deps.now()
    if (this.lastCheckAt !== null && now - this.lastCheckAt < UI_SELF_CHECK_INTERVAL_MS) return Promise.resolve({ kind: 'skipped' })
    this.lastCheckAt = now
    this.check = this.runCheck().finally(() => { this.check = null })
    return this.check
  }

  /** Ends the instance: forget the JWT and revoke the family, bounded, never awaited by teardown (U3.7). */
  destroy(): void {
    if (this.destroyed) return
    this.destroyed = true
    this.session = null
    this.view = null
    const credential = this.issuedWith ?? this.deps.credential()
    this.issuedWith = null
    if (credential) void this.deps.client.revokeFamily(credential, this.vph)
  }

  private async run(): Promise<UiSessionOutcome> {
    let invalidInARow = 0
    let unknownInARow = 0
    for (;;) {
      if (this.destroyed) return { kind: 'destroyed' }
      const credential = this.deps.credential()
      if (!credential || !this.deps.servable()) return { kind: 'not_servable' }
      const generation = this.deps.generation()
      const issued = await this.deps.client.issueTicket(credential, { ...this.deps.tags, vph: this.vph }, generation)
      if (issued.kind === 'ok') this.issuedWith = credential
      const failed = this.failure(issued)
      if (failed === 'retry') {
        this.setPaused(true)
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
        void this.deps.client.revokeFamily(credential, this.vph)
        return { kind: 'generation_changed' }
      }
      if (!this.deps.servable()) {
        void this.deps.client.revokeTicket(credential, ticket.ticketId)
        return { kind: 'not_servable' }
      }
      const view = this.view
      if (!view) return { kind: 'destroyed' }
      const exchanged = await this.deps.client.exchange(ticket.ticket, view, this.deps.boundKnowledgeBaseIds())
      if (this.destroyed) return { kind: 'destroyed' }
      if (exchanged.kind === 'ok') {
        this.session = { ...exchanged.value, exchangedAt: this.deps.now(), generation }
        this.setPaused(false)
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
        // U3.8: an exchange that times out or fails unknown pauses injection too.
        this.setPaused(true)
        if (++unknownInARow >= UI_UNKNOWN_LIMIT) return { kind: 'closed', code: 'probe_failed' }
        await this.deps.sleep(this.backoff(exchanged))
        continue
      }
      if (other) return other
    }
  }

  private async runCheck(): Promise<UiSelfCheckOutcome> {
    if (this.destroyed) return { kind: 'destroyed' }
    if (this.needsRenewal()) return this.renew()
    let unknownInARow = 0
    for (;;) {
      const s = this.session
      if (!s) return this.renew()
      const view = this.view
      if (!view) return { kind: 'destroyed' }
      const probed = await this.deps.client.probe(s.sessionToken, view)
      if (this.destroyed) return { kind: 'destroyed' }
      if (probed.kind === 'ok') {
        this.setPaused(false)
        return { kind: 'ok' }
      }
      if (probed.kind === 'refused' && probed.error.code === 'unauthorized') {
        const { reason, restarted } = probed.error
        if (reason === 'credential_expired') return this.renew()
        if (reason === 'credential_revoked') {
          const renewed = await this.renew()
          return renewed.kind === 'ready' ? { kind: 'revoked_renewed' } : renewed
        }
        if (reason === 'credential_invalid') {
          // This JWT is dead: nothing more goes out with it while the host rebuilds.
          if (this.session === s) this.session = null
          return { kind: 'session_invalid', restarted, jwtExchangedAt: s.exchangedAt, at: this.deps.now() }
        }
      }
      // Timeout, 5xx, an unknown reason or an undecodable `restarted`: pause and back off.
      this.setPaused(true)
      if (++unknownInARow >= UI_UNKNOWN_LIMIT) return { kind: 'closed', code: 'probe_failed' }
      await this.deps.sleep(this.backoff(probed))
      if (this.destroyed) return { kind: 'destroyed' }
    }
  }

  /** A renewal is an establish in the same family; the pause is lifted once it brings a new JWT. */
  private renew(): Promise<UiSessionOutcome> {
    return this.establish()
  }

  /** Lifting a pause tells the host to reload the document (U4.5). */
  private setPaused(paused: boolean): void {
    if (this.paused === paused) return
    this.paused = paused
    if (!paused && !this.destroyed) this.deps.onResume?.()
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
