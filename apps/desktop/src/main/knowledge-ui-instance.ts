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

export interface KnowledgeUiInstanceDeps {
  client: KnowledgeUiClient
  bindingTag: string
  partition: string
  origin: string
  routes: readonly UiRoute[]
  ledger: UiSessionLedger
  credential: () => string | null
  generation: () => number
  servable: () => boolean
  knowledgeBaseIds: () => readonly string[]
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
  /** An event went out or the instance was destroyed: nothing is injected any more. */
  private stopped = false
  private destroyed = false

  constructor(private readonly deps: KnowledgeUiInstanceDeps) {
    this.session = new UiViewSession({
      client: deps.client,
      view: deps.view ?? newViewSecret(),
      tags: { bindingTag: deps.bindingTag, partitionTag: partitionTag(deps.partition) },
      credential: deps.credential,
      generation: deps.generation,
      servable: deps.servable,
      boundKnowledgeBaseIds: deps.knowledgeBaseIds,
      now: deps.now,
      sleep: deps.sleep,
    })
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
    if (!this.injected.take(id) || statusCode !== 401 || this.stopped) return
    void this.session.selfCheck().then((outcome) => { this.settle(outcome) })
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
