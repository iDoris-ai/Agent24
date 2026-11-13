// Main's side of the daemon's private Knowledge host interface (KB-HOST-01
// PR-B2b; #1085 design v2 §1–2, UI-BRIDGE U1.4, U4.9–U4.11): bindings, their
// short-lived credential C, and the long-term logins. C and the logins never
// reach a log line, an error message or any renderer.
//
// Sequence numbers are wall-clock milliseconds bumped past the last one, so
// they keep growing across main restarts; a change the daemon calls stale is
// sent again under a new one. A restarted daemon has no records, so the
// bindings that should be active, and their C, are replayed on a new daemon
// connection, queued for each binding ahead of what is sent for it after that
// point. A change that did not land for a passing reason is reconciled the same
// way on every sync() until it does. Everything for one binding runs one at a
// time: a refresh and a destroy never overlap (decided 2026-10-25).
//
// In service mode the daemon requires presence confirmation. Every binding
// change, including a replay after the daemon has changed, is confirmed once
// through sendElevating. When the user refuses, the result is `refused` and
// it is not retried on its own. A binding the user refused is not asked again
// by a replay on another daemon either, until the user changes the settings or
// the app restarts.
// One not answered in time is `unanswered`: asked again on its own only after
// a backoff (10 min, doubling, at most 1 h), which the user's own change or
// another daemon resets. One that could not be asked at all is retried like
// a daemon that is not up. One the daemon itself would not ask about (no
// summary it could show, say one too long; nothing was shown) is `not_asked`:
// no refusal, it waits out the same backoff (#1852 B1).

import { daemonConnection, daemonElevation, type DaemonConnection } from './backend-manager'
import type { proxyRequest } from './backend-proxy'
import type { EnrolSession } from './daemon-enroll'
import { sendElevating } from './elevated-request'
import type { KnowledgeLogin, KnowledgeLoginStore, LoadResult } from './knowledge-logins'

const BINDINGS = '/api/v1/host/knowledge/bindings'
const MAX_BINDING_ID_BYTES = 256
const MAX_STALE_RETRIES = 3
const REQUEST_TIMEOUT_MS = 5_000
// Failures that say nothing about the change itself: it is tried again.
const PASSING: readonly Failure[] = ['daemon_unavailable', 'binding_inactive', 'stale_sequence']
// Not asked at all, or the connection went: nothing was shown; tried again
// on the next sync() like a daemon that is not up.
const PRESENCE_PASSING: readonly string[] = ['presence_unavailable', 'closed', 'busy', 'timeout', 'daemon_unavailable', 'bad_frame']
// The daemon would not ask (sendElevating's own failure is `internal`):
// nothing was shown, so no refusal; it may well not change, so backed off.
const NOT_ASKED: readonly string[] = ['not_elevated_operation', 'unknown_route', 'internal']
// Failures held for a backoff, then replayed.
const HELD: readonly Failure[] = ['unanswered', 'not_asked']

const UNANSWERED_FIRST_MS = 10 * 60_000
const UNANSWERED_MAX_MS = 60 * 60_000

/** How long after its `attempt`-th unanswered confirmation (from 1) a change
 * is asked again on its own: 10 min, doubling, at most 1 h (ruling
 * 2026-11-13-c-fix-f1-f3 question 3). */
export function unansweredRetryDelayMs(attempt: number): number {
  return Math.min(UNANSWERED_FIRST_MS * 2 ** Math.max(0, attempt - 1), UNANSWERED_MAX_MS)
}

export interface KnowledgeCredential {
  kind: 'bearer' | 'api_key'
  value: string
  /** Unix seconds. */
  expiresAt: number
}

type Failure = 'not_started' | 'invalid_binding' | 'login_unavailable' | 'credential_unavailable' | 'not_saved'
  | 'daemon_unavailable' | 'binding_inactive' | 'stale_sequence' | 'refused' | 'unanswered' | 'not_asked'

export type HostResult = { ok: true } | { ok: false; reason: Failure }

/** Where an active binding's calls go: the Knowledge server's API base and the
 * knowledge bases they may search. From trusted settings only, never a renderer. */
export interface KnowledgeEndpoint {
  endpoint: string
  knowledgeBaseIds: readonly string[]
}

/** One request to the daemon: its status and the `error.code` of its body. */
export type HostTransport = (connection: DaemonConnection, method: 'PUT' | 'DELETE', path: string, body?: string)
  => Promise<{ status: number; code?: string }>

/** The same rule as the daemon's `valid_binding_id`: opaque, but never a path. */
export function validBindingId(id: unknown): id is string {
  if (typeof id !== 'string' || id === '' || id === '.' || id === '..') return false
  if (Buffer.byteLength(id, 'utf8') > MAX_BINDING_ID_BYTES) return false
  for (const ch of id) {
    // Rust's `char::is_control` is the Cc category.
    const code = ch.codePointAt(0) ?? 0
    if (code <= 0x1f || (code >= 0x7f && code <= 0x9f) || ch === '/' || ch === '\\') return false
  }
  try {
    encodeURIComponent(id)
    return true
  } catch {
    // A lone surrogate has no UTF-8 form.
    return false
  }
}

/** Wall-clock milliseconds, never repeating or going back within a process. */
export class SequenceClock {
  private last = 0

  constructor(private readonly now: () => number = Date.now) {}

  next(): number {
    this.last = Math.max(this.last + 1, Math.floor(this.now()))
    return this.last
  }
}

export interface KnowledgeHostDeps {
  connection?: () => DaemonConnection | null
  /** the enrolment's token-connection turn (backend-manager `daemonElevation`); null without an enrolment */
  elevation?: () => EnrolSession['turn'] | null
  transport?: HostTransport
  clock?: SequenceClock
  /** Unix seconds, to tell whether a C is still worth replaying. */
  nowSeconds?: () => number
  /** The enrolled service daemon by value (`pid:generation`), or null when the connection is not an enrolment; a renewal leaves it as it is. */
  daemonIdentity?: () => string | null
  /** Wall-clock milliseconds, for the backoff of an unanswered confirmation. */
  nowMs?: () => number
}

export class KnowledgeHost {
  private started: LoadResult | null = null
  // Bindings that should be active in the daemon, with their current C.
  private readonly wanted = new Map<string, { endpoint: KnowledgeEndpoint | null; credential: KnowledgeCredential | null }>()
  private readonly locks = new Map<string, Promise<unknown>>()
  private connectionKey: string | null = null
  // Bindings whose activation the daemon on this connection has taken.
  private readonly confirmed = new Set<string>()
  // Bindings whose daemon record may differ from `wanted`, and those queued to be fixed.
  private unsynced = new Set<string>()
  private readonly reconciling = new Set<string>()
  // Bindings whose last confirmation went unanswered: not reconciled before `until`.
  // Memory only; the user's own change of the binding or another daemon clears it.
  private readonly held = new Map<string, { until: number; attempt: number }>()
  // Bindings whose last change was refused: never reconciled, nor replayed on
  // another daemon. Memory only; only the user's own change of the binding clears it.
  private readonly refused = new Set<string>()
  // Bumped for a binding by every deactivate: an activation called before it
  // and still queued then does nothing.
  private readonly drops = new Map<string, number>()
  private readonly deps: Required<KnowledgeHostDeps>

  constructor(private readonly logins: KnowledgeLoginStore, deps: KnowledgeHostDeps = {}) {
    const nowSeconds = () => Math.floor(Date.now() / 1000)
    const connection = deps.connection ?? daemonConnection
    const elevation = deps.elevation ?? daemonElevation
    this.deps = { connection, elevation, transport: elevatingTransport(connection, elevation), clock: new SequenceClock(), nowSeconds, daemonIdentity: () => null, nowMs: Date.now, ...deps }
  }

  /** Loads the logins, once, before anything may change them (G4). A load
   * that throws counts as unreadable. */
  start(): LoadResult {
    if (this.started) return this.started
    try {
      this.started = this.logins.load()
    } catch {
      this.started = { ok: false, reason: 'unreadable' }
    }
    return this.started
  }

  /** Queues a reconcile for every binding that may be out of step: all wanted
   * ones on a new daemon connection (it has no records, so nothing is left to
   * remove), else those whose last change did not land. Cheap to call often. */
  sync(): void {
    const conn = this.deps.connection()
    if (!conn) return
    const identity = this.deps.daemonIdentity()
    // The service daemon by value: a renewal (a new token, same daemon) is no
    // new daemon and replays nothing; the daemon keeps bindings by host key,
    // not by connection. Otherwise the connection itself, as before.
    const key = identity !== null ? `svc:${identity}` : `conn:${conn.port} ${conn.authHeaders['Authorization'] ?? ''}`
    if (key !== this.connectionKey) {
      this.connectionKey = key
      this.unsynced = new Set(this.wanted.keys())
      this.confirmed.clear()
      this.held.clear()
    }
    for (const bindingId of this.unsynced) {
      // A refused or held binding is queued too: reconcile() itself skips it,
      // also for one queued before its refusal or backoff.
      if (this.reconciling.has(bindingId)) continue
      this.reconciling.add(bindingId)
      void this.exclusive(bindingId, () => {
        this.reconciling.delete(bindingId)
        return this.reconcile(bindingId)
      })
    }
  }

  /** Activates `bindingId`, with the endpoint its calls use; without one the
   * daemon refuses every call of the binding. */
  activate(bindingId: string, endpoint: KnowledgeEndpoint | null = null): Promise<HostResult> {
    const copy = endpoint && { endpoint: endpoint.endpoint, knowledgeBaseIds: [...endpoint.knowledgeBaseIds] }
    const drops = this.drops.get(bindingId) ?? 0
    // This change is the reconcile: a replay queued for the binding, by this
    // sync (a daemon not noticed yet) or an earlier one, finds nothing to do.
    if (validBindingId(bindingId) && this.started) {
      this.sync()
      this.unsynced.delete(bindingId)
    }
    return this.operate(bindingId, async () => {
      // An Off since the call wins, even over an activation queued behind a refresh.
      if ((this.drops.get(bindingId) ?? 0) !== drops) return failure('binding_inactive')
      // The user's own change: asked now, and a backoff starts again from the first step.
      this.held.delete(bindingId)
      this.refused.delete(bindingId)
      // Activating drops C in the daemon; it is pushed again afterwards.
      this.wanted.set(bindingId, { endpoint: copy, credential: null })
      this.confirmed.delete(bindingId)
      // This change is the reconcile: a sync while it waits for the user's
      // answer queues no second one; track() puts it back when it must be retried.
      this.unsynced.delete(bindingId)
      return this.track(bindingId, await this.activation(bindingId))
    })
  }

  /** Whether `bindingId` is in effect: active in the daemon on this connection,
   * as wanted, and its last change not refused (a refused C push leaves the
   * old C to run out; fail-closed, ruling 2026-11-13-c2-stale-active). Checks
   * the connection first: a restarted daemon has nothing active yet, even
   * before the timer's `sync()` notices it. */
  isActive(bindingId: string): boolean {
    this.sync()
    return this.wanted.has(bindingId) && this.confirmed.has(bindingId) && !this.refused.has(bindingId)
  }

  /** Drops `bindingId` at once, ahead of anything queued for it (Off): it
   * is no longer wanted, so a queued push, replay or activation sends
   * nothing, and an activation still being retried stops. */
  async deactivate(bindingId: string): Promise<HostResult> {
    if (!validBindingId(bindingId)) return failure('invalid_binding')
    if (!this.started) return failure('not_started')
    this.drops.set(bindingId, (this.drops.get(bindingId) ?? 0) + 1)
    this.wanted.delete(bindingId)
    this.confirmed.delete(bindingId)
    this.sync()
    // The user's own change: asked now, and a backoff starts again from the first step.
    this.held.delete(bindingId)
    this.refused.delete(bindingId)
    // As in activate(): no reconcile beside this change while it waits for the answer.
    this.unsynced.delete(bindingId)
    return this.track(bindingId, await this.send(bindingId, 'DELETE', ''))
  }

  pushCredential(bindingId: string, credential: KnowledgeCredential): Promise<HostResult> {
    return this.operate(bindingId, () => this.push(bindingId, credential))
  }

  revokeCredential(bindingId: string): Promise<HostResult> {
    return this.operate(bindingId, async () => {
      const wanted = this.wanted.get(bindingId)
      if (wanted) wanted.credential = null
      return this.track(bindingId, await this.send(bindingId, 'DELETE', '/credential'))
    })
  }

  /** Gets a new C from the binding's login and pushes it, never alongside a
   * destroy of the same binding (G5). */
  refresh(bindingId: string, obtain: (login: Readonly<KnowledgeLogin>) => Promise<KnowledgeCredential>): Promise<HostResult> {
    return this.operate(bindingId, async () => {
      const login = this.logins.list().get(bindingId)
      if (!login || login.unusable) return failure('login_unavailable')
      let credential: KnowledgeCredential
      try {
        credential = await obtain(login)
      } catch {
        return failure('credential_unavailable')
      }
      return this.push(bindingId, credential)
    })
  }

  /** Destroys the login of `bindingId` (U4.11) as far as the host can today:
   * marks it unusable, drops C and the binding in the daemon, then removes
   * the login. Remote revocation needs a server contract that does not exist
   * yet. When the unusable mark or the removal is not saved, the destroy did
   * not finish and is reported so (G2'); the tightening stays in memory. When
   * the daemon did not drop the binding, that is reported, and retried. */
  destroy(bindingId: string): Promise<HostResult> {
    return this.operate(bindingId, async () => {
      // Never replayed again, whatever happens below.
      this.wanted.delete(bindingId)
      // Handled at once: the daemon call below must not leave a rejection unobserved.
      const marked = this.logins.markUnusable(bindingId).then(() => true, () => false)
      const dropped = this.track(bindingId, await this.send(bindingId, 'DELETE', ''))
      if (!(await marked)) return failure('not_saved')
      try {
        await this.logins.remove(bindingId)
      } catch {
        return failure('not_saved')
      }
      return dropped
    })
  }

  private operate(bindingId: string, task: () => Promise<HostResult>): Promise<HostResult> {
    if (!validBindingId(bindingId)) return Promise.resolve(failure('invalid_binding'))
    if (!this.started) return Promise.resolve(failure('not_started'))
    this.sync()
    return this.exclusive(bindingId, task)
  }

  /** Runs `task` after every earlier task for the same binding. */
  private exclusive<T>(bindingId: string, task: () => Promise<T>): Promise<T> {
    const previous = this.locks.get(bindingId) ?? Promise.resolve()
    const run = previous.then(task)
    const settled = run.then(() => {}, () => {})
    this.locks.set(bindingId, settled)
    void settled.then(() => {
      if (this.locks.get(bindingId) === settled) this.locks.delete(bindingId)
    })
    return run
  }

  private async push(bindingId: string, credential: KnowledgeCredential): Promise<HostResult> {
    const wanted = this.wanted.get(bindingId)
    if (!wanted) return failure('binding_inactive')
    const copy: KnowledgeCredential = { kind: credential.kind, value: credential.value, expiresAt: credential.expiresAt }
    wanted.credential = copy
    const result = this.track(bindingId, await this.send(bindingId, 'PUT', '/credential', credentialBody(copy)))
    // Taken by the daemon whose activation is confirmed: its record is as wanted,
    // so a reconcile or a backoff still pending for it has nothing left to do.
    if (result.ok && this.confirmed.has(bindingId)) {
      this.unsynced.delete(bindingId)
      this.held.delete(bindingId)
    }
    return result
  }

  /** Notes what a change that did not land means for later: refused (not asked
   * again on its own), unanswered (held for a backoff), or due for a reconcile. */
  private track(bindingId: string, result: HostResult): HostResult {
    // A backoff ends with the reconcile that lands, or the user's own change.
    if (result.ok) return result
    if (result.reason === 'refused') {
      this.refused.add(bindingId)
    } else if (HELD.includes(result.reason)) {
      // Back into unsynced, so the activation is replayed when the backoff is over.
      const attempt = (this.held.get(bindingId)?.attempt ?? 0) + 1
      this.held.set(bindingId, { until: this.deps.nowMs() + unansweredRetryDelayMs(attempt), attempt })
      this.unsynced.add(bindingId)
    } else if (PASSING.includes(result.reason)) {
      this.unsynced.add(bindingId)
    }
    return result
  }

  /** Brings the daemon's record in line with `wanted`: activated with its
   * unexpired C, or dropped. Runs under the binding's turn. */
  private async reconcile(bindingId: string): Promise<void> {
    // A new connection replaces the set; this run then only answers to the old one.
    const unsynced = this.unsynced
    if (!unsynced.has(bindingId)) return
    // Refused, or not answered, while this run was queued behind that change.
    if (this.refused.has(bindingId)) return
    const heldUntil = this.held.get(bindingId)?.until
    if (heldUntil !== undefined && heldUntil > this.deps.nowMs()) return
    const wanted = this.wanted.get(bindingId)
    // The user changed the binding while this replay waited for its answer:
    // the answer is about the old change and notes nothing; the user's change
    // tracks itself.
    const superseded = () => this.wanted.get(bindingId) !== wanted
    const result = wanted ? await this.activation(bindingId) : await this.send(bindingId, 'DELETE', '')
    if (superseded()) return
    if (!result.ok) {
      // Not answered, on whichever daemon it was asked: the user is away, so
      // the replay waits out the backoff. So does one the daemon would not ask about.
      if (HELD.includes(result.reason)) {
        this.track(bindingId, result)
        return
      }
      // A refusal will not change by asking again.
      if (result.reason === 'refused') this.refused.add(bindingId)
      if (!PASSING.includes(result.reason)) unsynced.delete(bindingId)
      return
    }
    const credential = wanted?.credential
    if (credential && credential.expiresAt > this.deps.nowSeconds()) {
      const pushed = await this.send(bindingId, 'PUT', '/credential', credentialBody(credential))
      if (superseded()) return
      if (!pushed.ok && HELD.includes(pushed.reason)) {
        this.track(bindingId, pushed)
        return
      }
      if (!pushed.ok && PASSING.includes(pushed.reason)) return
      if (!pushed.ok && pushed.reason === 'refused') this.refused.add(bindingId)
    }
    unsynced.delete(bindingId)
    this.held.delete(bindingId)
  }

  /** Sends the wanted activation, and records it as taken by this connection. */
  private async activation(bindingId: string): Promise<HostResult> {
    const key = this.connectionKey
    const result = await this.send(bindingId, 'PUT', '', activationBody(this.wanted.get(bindingId)?.endpoint ?? null))
    if (result.ok && key === this.connectionKey && this.wanted.has(bindingId)) this.confirmed.add(bindingId)
    return result
  }

  /** Sends one change, again under a new number while the daemon calls it
   * stale. A PUT stops once its binding is no longer the one wanted. */
  private async send(bindingId: string, method: 'PUT' | 'DELETE', suffix: string, fields: Record<string, unknown> = {}): Promise<HostResult> {
    let last: Failure = 'stale_sequence'
    const entry = this.wanted.get(bindingId)
    for (let attempt = 0; attempt < MAX_STALE_RETRIES; attempt++) {
      // Overtaken: a later change of the binding wins, so a stale retry of
      // an Off never drops an activation that came after it.
      if (this.wanted.get(bindingId) !== entry || (method === 'PUT' && entry === undefined)) return failure('binding_inactive')
      const conn = this.deps.connection()
      if (!conn) return failure('daemon_unavailable')
      const path = `${BINDINGS}/${encodeURIComponent(bindingId)}${suffix}`
      const payload = JSON.stringify({ seq: this.deps.clock.next(), ...fields })
      let response: { status: number; code?: string }
      try {
        response = await this.deps.transport(conn, method, path, payload)
      } catch {
        return failure('daemon_unavailable')
      }
      if (response.status >= 200 && response.status < 300) return { ok: true }
      last = classify(response)
      if (last !== 'stale_sequence') return failure(last)
    }
    return failure(last)
  }
}

function failure(reason: Failure): HostResult {
  return { ok: false, reason }
}

function classify(response: { status: number; code?: string }): Failure {
  // Elevation outcomes (elevated-request.ts answers 403 with the EnrolError code).
  if (response.status === 403 && response.code !== undefined) {
    if (response.code === 'presence_timeout') return 'unanswered'
    if (PRESENCE_PASSING.includes(response.code)) return 'daemon_unavailable'
    if (NOT_ASKED.includes(response.code)) return 'not_asked'
  }
  if (response.code === 'stale_sequence') return 'stale_sequence'
  if (response.code === 'binding_inactive') return 'binding_inactive'
  if (response.status === 503) return 'daemon_unavailable'
  return 'refused'
}

function activationBody(endpoint: KnowledgeEndpoint | null): Record<string, unknown> {
  return endpoint ? { endpoint: endpoint.endpoint, knowledge_base_ids: endpoint.knowledgeBaseIds } : {}
}

function credentialBody(credential: KnowledgeCredential): Record<string, unknown> {
  return { kind: credential.kind, value: credential.value, expires_at: credential.expiresAt }
}

/** M5 B6. After K-B6 a binding change is Elevated in service mode. With an
 * enrolment, confirm once and retry once with the grant. Without an
 * enrolment, this is one ordinary request. */
export function elevatingTransport(
  connection: () => DaemonConnection | null,
  elevation: () => EnrolSession['turn'] | null,
  send?: typeof proxyRequest,
): HostTransport {
  return async (_connection, method, path, body) => {
    // Live `connection`, not `_connection`. sendElevating reads the getter again
    // inside the turn to check the token is still this one. send() just read
    // that same getter before calling the transport, so the first request
    // uses the same connection.
    let request: { method: 'PUT' | 'DELETE'; path: string; body?: unknown }
    try {
      request = body === undefined ? { method, path } : { method, path, body: JSON.parse(body) }
    } catch {
      throw new Error('Knowledge host request failed')
    }
    const res = await sendElevating(
      request,
      { timeoutMs: REQUEST_TIMEOUT_MS },
      send ? { connection, elevation, send } : { connection, elevation },
    )
    const raw = (res.data as { error?: { code?: unknown } } | null)?.error?.code
    const code = typeof raw === 'string' ? raw : undefined
    // proxyRequest's own failure (cannot connect, timed out, response cut short):
    // send() still records the throw as daemon_unavailable.
    if (
      (code === 'timeout' && res.status === 504)
      || (code === 'proxy_failed' && res.status === 502)
      || (code === 'payload_too_large' && res.status === 502)
      || (code === 'backend_not_ready' && res.status === 503)
    ) {
      throw new Error('Knowledge host request failed')
    }
    return { status: res.status, code }
  }
}
