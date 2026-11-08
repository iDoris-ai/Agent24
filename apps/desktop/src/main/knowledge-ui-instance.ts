// KB-C2-04a (#1623): one KnowledgeView instance's side of the UI session, as
// the host sees it. It owns the instance's view secret and UiViewSession, gets
// the first JWT before the page loads (U4.5), renews at about 2/3 of the life
// (U3.6), decides injection for the partition's requests (U4.5 ①–⑤), turns a
// 401 on an injected request into a self-check (U3.8), and tells the host what
// a result means for the view through one event. The per-binding counts are
// the shared UiSessionLedger's; the host keeps one per binding.
//
// Stricter than UIB where M2 cannot know better (design §6): a binding that is
// no longer servable, a C problem and a generation change all end the
// instance (`unavailable`) instead of pausing it.

import { newUlid } from './knowledge-bridge'
import { readCitationChunk, type ChunkRead, type CitationRef } from './knowledge-readonly'
import { newViewSecret, partitionTag, type KnowledgeUiClient, type ViewSecret } from './knowledge-ui-client'
import { decideInjection, InjectedRequests, type InjectDecision, type InjectRequest } from './knowledge-ui-inject'
import type { UiRoute } from './knowledge-ui-routes'
import type { UiSessionLedger, ViewLevelDecision } from './knowledge-ui-session'
import { UiViewSession, type UiSelfCheckOutcome } from './knowledge-ui-view-session'

/** What the host does with the instance. */
export type UiInstanceEvent =
  /** View-level handling: a new instance (new partition, new vph), not counted as a crash. */
  | { kind: 'rebuild' }
  /** §9.2: stays closed until the user reloads or reopens from the settings. */
  | { kind: 'closed'; code: 'session_rejected' | 'revocation_mismatch' | 'probe_failed' | 'empty_scope' }
  /** No session can be had for now (C, servability, generation): the view steps aside with Reload. */
  | { kind: 'unavailable' }
  /** U4.5 "恢复即重载": injection was paused and is back; reload the current document. Not an end. */
  | { kind: 'reload' }

export interface KnowledgeUiInstanceDeps {
  client: KnowledgeUiClient
  bindingTag: string
  partition: string
  origin: string
  routes: readonly UiRoute[]
  ledger: UiSessionLedger
  credential: () => string | null
  /** Knowledge bases picked in settings (U7.2: the citation set is settings ∩ the exchange). */
  selected: () => readonly string[]
  /** Defaults to globalThis.fetch. */
  fetch?: typeof fetch
  generation: () => number
  servable: () => boolean
  now: () => number
  sleep: (ms: number) => Promise<void>
  setTimer: (fn: () => void, ms: number) => unknown
  clearTimer: (handle: unknown) => void
  onEvent: (event: UiInstanceEvent) => void
  /** Tests only; a new CSPRNG secret otherwise. */
  view?: ViewSecret
}

export class KnowledgeUiInstance {
  private readonly session: UiViewSession
  private readonly injected = new InjectedRequests()
  private renewal: unknown = null
  /** One self-check and its settlement at a time, however many 401s ask for it. */
  private checking: Promise<void> | null = null
  /** An event went out or the instance was destroyed: nothing is injected any more. */
  private stopped = false
  private destroyed = false

  constructor(private readonly deps: KnowledgeUiInstanceDeps) {
    this.session = new UiViewSession({
      client: deps.client,
      tags: { bindingTag: deps.bindingTag, partitionTag: partitionTag(deps.partition) },
      credential: deps.credential,
      generation: deps.generation,
      servable: deps.servable,
      now: deps.now,
      sleep: deps.sleep,
      onResume: () => { if (!this.stopped) this.deps.onEvent({ kind: 'reload' }) },
    }, deps.view ?? newViewSecret())
  }

  /** The first JWT (U4.5: the page loads only after it). False when there is none; the event has gone out then. */
  async start(): Promise<boolean> {
    return this.settle(await this.session.establish())
  }

  /** The partition's `onBeforeSendHeaders` for one request of this instance's webContents. */
  inject(request: InjectRequest, webContentsId: number): InjectDecision {
    const decision = decideInjection(request, {
      webContentsId,
      origin: this.deps.origin,
      routes: this.deps.routes,
      credentials: () => (this.stopped ? null : this.session.injection()),
    })
    if (!decision.cancel && decision.injected) this.injected.note(request.id)
    return decision
  }

  /** A request ended (`onCompleted` with its status, `onErrorOccurred` without): a 401 on an injected one starts the self-check. */
  responded(id: number, statusCode: number | undefined): void {
    if (!this.injected.take(id) || statusCode !== 401 || this.stopped || this.checking) return
    this.checking = this.session.selfCheck()
      .then((outcome) => { this.settle(outcome) })
      .finally(() => { this.checking = null })
  }

  /**
   * The bridge's U6.4 ⑬⑭ in M2 (design §6): admission is open while this
   * session holds a JWT it may inject now (servable, same generation, not
   * paused, not past its life); the generation is the binding's.
   */
  bridgeAdmission(): { open: boolean; generation: number } {
    return { open: !this.stopped && this.session.injection() !== null, generation: this.deps.generation() }
  }

  /**
   * U7.2: null when there is no exchange or admission is closed. Otherwise
   * settings ∩ the exchange's knowledge bases, in the settings' order (design §4).
   */
  citationScope(): readonly string[] | null {
    if (!this.bridgeAdmission().open) return null
    const ids = this.session.knowledgeBaseIds()
    if (ids === null) return null
    const allowed = new Set(ids)
    return this.deps.selected().filter((id) => allowed.has(id))
  }

  /** One citation's chunk (U7.2). The read uses C and the allowed origin; it logs neither. */
  readCitation(generation: number, ref: CitationRef, signal: AbortSignal, visible?: () => boolean): Promise<ChunkRead> {
    return readCitationChunk({
      origin: this.deps.origin,
      credential: () => this.deps.credential(),
      fetch: this.deps.fetch ?? globalThis.fetch,
      newRequestId: () => newUlid(),
    }, generation, ref, signal, visible)
  }

  /** U4.7: stop injecting, forget the JWT, revoke the family (bounded, not awaited). */
  destroy(): void {
    if (this.destroyed) return
    this.destroyed = true
    this.stopped = true
    this.cancelRenewal()
    this.injected.clear()
    this.session.destroy()
  }

  private settle(outcome: UiSelfCheckOutcome): boolean {
    if (this.stopped) return false
    switch (outcome.kind) {
      case 'ready':
        this.scheduleRenewal()
        return true
      case 'ok':
      case 'skipped':
        return true
      case 'revoked_renewed':
        if (this.deps.ledger.revocationMismatch(this.deps.now()) === 'closed') return this.end({ kind: 'closed', code: 'revocation_mismatch' })
        this.scheduleRenewal()
        return true
      case 'view_level':
        return this.viewLevel(this.deps.ledger.handle({ kind: 'invalid' }, this.deps.now()))
      case 'session_invalid':
        return this.viewLevel(this.deps.ledger.handle(
          outcome.restarted ? { kind: 'restarted', jwtExchangedAt: outcome.jwtExchangedAt } : { kind: 'invalid' }, outcome.at))
      case 'closed':
        return this.end({ kind: 'closed', code: outcome.code })
      case 'credential':
      case 'not_servable':
      case 'generation_changed':
        return this.end({ kind: 'unavailable' })
      case 'destroyed':
        return false
    }
  }

  private viewLevel(decision: ViewLevelDecision): boolean {
    return this.end(decision.action === 'closed' ? { kind: 'closed', code: 'session_rejected' } : { kind: 'rebuild' })
  }

  private end(event: UiInstanceEvent): false {
    this.stopped = true
    this.cancelRenewal()
    this.deps.onEvent(event)
    return false
  }

  private scheduleRenewal(): void {
    this.cancelRenewal()
    this.renewal = this.deps.setTimer(() => {
      this.renewal = null
      if (this.stopped) return
      void this.session.establish().then((outcome) => { this.settle(outcome) })
    }, this.session.renewalDelay())
  }

  private cancelRenewal(): void {
    if (this.renewal === null) return
    this.deps.clearTimer(this.renewal)
    this.renewal = null
  }
}
