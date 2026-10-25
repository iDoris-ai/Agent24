// Main's side of the daemon's private Knowledge host interface (KB-HOST-01
// PR-B2b; #1085 design v2 §1–2, UI-BRIDGE U1.4, U4.9–U4.11). Main activates
// and deactivates bindings in the daemon, hands it the short-lived credential
// C of an active binding or takes it back, and owns the long-term logins.
// C and the logins never reach a log line, an error message or any renderer.
//
// Every request carries a sequence number the daemon requires to grow per
// binding: here it is the wall clock in milliseconds, bumped past the last one
// handed out, so it keeps growing across main restarts (decided 2026-10-25).
// A request the daemon calls stale did not land; it is sent again under a new
// number.
//
// The daemon keeps its records per host capability, so a restarted daemon has
// none. This module remembers which bindings should be active and their C, and
// replays that state once it sees a new daemon connection, before anything
// else is sent for those bindings. Everything for one binding runs one at a
// time, so a refresh and a destroy never overlap (decided 2026-10-25).

import http from 'node:http'
import { daemonConnection, type DaemonConnection } from './backend-manager'
import type { KnowledgeLogin, KnowledgeLoginStore, LoadResult } from './knowledge-logins'

const BINDINGS = '/api/v1/host/knowledge/bindings'
const MAX_BINDING_ID_BYTES = 256
const MAX_STALE_RETRIES = 3
const REQUEST_TIMEOUT_MS = 5_000

export interface KnowledgeCredential {
  kind: 'bearer' | 'api_key'
  value: string
  /** Unix seconds. */
  expiresAt: number
}

export type HostResult =
  | { ok: true }
  | {
      ok: false
      reason:
        | 'not_started'
        | 'invalid_binding'
        | 'login_unavailable'
        | 'credential_unavailable'
        | 'not_saved'
        | 'daemon_unavailable'
        | 'binding_inactive'
        | 'stale_sequence'
        | 'refused'
    }

type Failure = Extract<HostResult, { ok: false }>['reason']

/** One request to the daemon: its status and the `error.code` of its body. */
export type HostTransport = (
  connection: DaemonConnection,
  method: 'PUT' | 'DELETE',
  path: string,
  body?: string,
) => Promise<{ status: number; code?: string }>

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

interface Wanted {
  credential: KnowledgeCredential | null
}

export interface KnowledgeHostDeps {
  connection?: () => DaemonConnection | null
  transport?: HostTransport
  clock?: SequenceClock
  /** Unix seconds, to tell whether a C is still worth replaying. */
  nowSeconds?: () => number
}

export class KnowledgeHost {
  private started: LoadResult | null = null
  // Bindings that should be active in the daemon, with their current C.
  private readonly wanted = new Map<string, Wanted>()
  private readonly locks = new Map<string, Promise<unknown>>()
  private connectionKey: string | null = null
  private readonly connection: () => DaemonConnection | null
  private readonly transport: HostTransport
  private readonly clock: SequenceClock
  private readonly nowSeconds: () => number

  constructor(
    private readonly logins: KnowledgeLoginStore,
    deps: KnowledgeHostDeps = {},
  ) {
    this.connection = deps.connection ?? daemonConnection
    this.transport = deps.transport ?? httpTransport
    this.clock = deps.clock ?? new SequenceClock()
    this.nowSeconds = deps.nowSeconds ?? (() => Math.floor(Date.now() / 1000))
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

  /** On a new daemon connection, queues the replay of every wanted binding
   * ahead of whatever is sent for it next. Cheap to call often. */
  sync(): void {
    const conn = this.connection()
    if (!conn) return
    const key = `${conn.port} ${conn.authHeaders['Authorization'] ?? ''}`
    if (key === this.connectionKey) return
    this.connectionKey = key
    for (const bindingId of this.wanted.keys()) {
      void this.exclusive(bindingId, () => this.replay(bindingId))
    }
  }

  activate(bindingId: string): Promise<HostResult> {
    return this.operate(bindingId, async () => {
      // Activating drops C in the daemon; it is pushed again afterwards.
      this.wanted.set(bindingId, { credential: null })
      return this.send(bindingId, 'PUT', '')
    })
  }

  deactivate(bindingId: string): Promise<HostResult> {
    return this.operate(bindingId, async () => {
      this.wanted.delete(bindingId)
      return this.send(bindingId, 'DELETE', '')
    })
  }

  pushCredential(bindingId: string, credential: KnowledgeCredential): Promise<HostResult> {
    return this.operate(bindingId, () => this.push(bindingId, credential))
  }

  revokeCredential(bindingId: string): Promise<HostResult> {
    return this.operate(bindingId, async () => {
      const wanted = this.wanted.get(bindingId)
      if (wanted) wanted.credential = null
      return this.send(bindingId, 'DELETE', '/credential')
    })
  }

  /** Gets a new C from the binding's login and pushes it, never alongside a
   * destroy of the same binding (G5). */
  refresh(
    bindingId: string,
    obtain: (login: Readonly<KnowledgeLogin>) => Promise<KnowledgeCredential>,
  ): Promise<HostResult> {
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
   * not finish and is reported so (G2'); the tightening stays in memory. */
  destroy(bindingId: string): Promise<HostResult> {
    return this.operate(bindingId, async () => {
      // Never replayed again, whatever happens below.
      this.wanted.delete(bindingId)
      // Handled at once: the daemon call below must not leave a rejection unobserved.
      const marked = this.logins.markUnusable(bindingId).then(
        () => true,
        () => false,
      )
      await this.send(bindingId, 'DELETE', '')
      if (!(await marked)) return failure('not_saved')
      try {
        await this.logins.remove(bindingId)
      } catch {
        return failure('not_saved')
      }
      return { ok: true }
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
    const settled = run.then(
      () => {},
      () => {},
    )
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
    return this.send(bindingId, 'PUT', '/credential', credentialBody(copy))
  }

  private async replay(bindingId: string): Promise<void> {
    const wanted = this.wanted.get(bindingId)
    if (!wanted) return
    const credential = wanted.credential
    const activated = await this.send(bindingId, 'PUT', '')
    if (!activated.ok || !credential || credential.expiresAt <= this.nowSeconds()) return
    // A push queued after this replay may have replaced C meanwhile; it sends its own.
    if (this.wanted.get(bindingId)?.credential === credential) {
      await this.send(bindingId, 'PUT', '/credential', credentialBody(credential))
    }
  }

  /** Sends one change, again under a new number while the daemon calls it stale. */
  private async send(bindingId: string, method: 'PUT' | 'DELETE', suffix: string, body?: string): Promise<HostResult> {
    let last: Failure = 'stale_sequence'
    for (let attempt = 0; attempt < MAX_STALE_RETRIES; attempt++) {
      const conn = this.connection()
      if (!conn) return failure('daemon_unavailable')
      const path = `${BINDINGS}/${encodeURIComponent(bindingId)}${suffix}?seq=${this.clock.next()}`
      let response: { status: number; code?: string }
      try {
        response = await this.transport(conn, method, path, body)
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
  if (response.code === 'stale_sequence') return 'stale_sequence'
  if (response.code === 'binding_inactive') return 'binding_inactive'
  if (response.status === 503) return 'daemon_unavailable'
  return 'refused'
}

function credentialBody(credential: KnowledgeCredential): string {
  return JSON.stringify({ kind: credential.kind, value: credential.value, expires_at: credential.expiresAt })
}

/** Loopback HTTP to the daemon; reads nothing from a response but its error code. */
const httpTransport: HostTransport = (connection, method, path, body) =>
  new Promise((resolve, reject) => {
    const headers: Record<string, string> = { ...connection.authHeaders }
    if (body !== undefined) {
      headers['Content-Type'] = 'application/json'
      headers['Content-Length'] = String(Buffer.byteLength(body))
    }
    const req = http.request(
      { host: '127.0.0.1', port: connection.port, method, path, headers, timeout: REQUEST_TIMEOUT_MS },
      (res) => {
        const chunks: Buffer[] = []
        res.on('data', (chunk: Buffer) => chunks.push(chunk))
        res.on('end', () => {
          let code: string | undefined
          try {
            const parsed = JSON.parse(Buffer.concat(chunks).toString()) as { error?: { code?: unknown } }
            if (typeof parsed.error?.code === 'string') code = parsed.error.code
          } catch {
            // No JSON body: a success, or an error without the envelope.
          }
          resolve({ status: res.statusCode ?? 0, code })
        })
      },
    )
    req.on('timeout', () => req.destroy(new Error('timeout')))
    req.on('error', () => reject(new Error('Knowledge host request failed')))
    req.end(body)
  })
