// M5 B3 (#1717): main's presence connection to the daemon the LaunchAgent
// runs. In service mode main holds no host token, so the daemon cannot ask it
// for a presence proof over HTTP; it asks over a second enrolment connection
// instead (K-B3 #1715, R0 A): `hello` with `role: "presence"`, answered by
// `presence_ready` and no token. On it the daemon sends `presence_challenge`,
// main shows the native box and answers `presence_answer`; the daemon pings
// every minute. The token connection (B2) is untouched by any of this.
//
// Only main answers: the box is the only source of a decision, nothing here
// is reachable from the renderer, and an answer goes only on the connection
// its challenge came on. Frame and field names are K-B3's (#1715,
// issuecomment-6732349252), all in `PRESENCE` so a change there is one edit.
//
// No credential is involved: the connection carries no token, and nothing
// from a challenge but its id and code is logged.

import type net from 'node:net'
import { EnrolError, hello, openEnrolConnection, type EnrolConnection, type Frame } from './daemon-enroll'
import { parseChallenge, type PresenceChallenge } from './presence-poller'

/** K-B3's frame and field names (#1715). */
export const PRESENCE = {
  role: 'presence',
  ready: 'presence_ready',
  challenge: 'presence_challenge',
  answer: 'presence_answer',
  answered: 'presence_answered',
  cancel: 'presence_cancel',
  ping: 'presence_ping',
  pong: 'presence_pong',
} as const

/** The daemon pings every 60 s; two missed pings and a little more is a dead connection. */
export const PRESENCE_SILENCE_MS = 2 * 60_000 + 10_000
export const RECONNECT_MIN_MS = 1_000
export const RECONNECT_MAX_MS = 30_000
/** How often to look again when there is no service daemon to connect to. */
export const IDLE_MS = 5_000
const MAX_ID = 200

/** A challenge as it comes on the presence connection. */
export interface SocketChallenge extends PresenceChallenge {
  /** Echoed in the answer: binds it to this challenge. */
  nonce: string
  method: string
  path: string
}

export type PresenceDecision = 'allow' | 'deny'

export interface PresenceTarget {
  socketPath: string
  generation: string
}

export interface PresenceConnectionDeps {
  /** The service daemon's enrolment socket while this app is enrolled with it; else null. */
  target: () => PresenceTarget | null
  /** The native box. Aborting `signal` (expiry, cancel, the connection ending) closes it. */
  ask: (challenge: SocketChallenge, signal: AbortSignal) => Promise<PresenceDecision>
  connect?: (socketPath: string) => net.Socket
  nowMs?: () => number
  /** Resolves after `ms`, or early when `signal` aborts. */
  sleep?: (ms: number, signal: AbortSignal) => Promise<void>
  log?: (message: string) => void
}

const shortString = (v: unknown, max = MAX_ID): v is string => typeof v === 'string' && v !== '' && v.length <= max

/** A `presence_challenge` frame, or null when it is not one main may answer. */
export function parseSocketChallenge(frame: Frame): SocketChallenge | null {
  const base = parseChallenge(frame)
  const { nonce, method, path } = frame as Record<string, unknown>
  if (!base || !shortString(base.challenge_id) || !shortString(nonce) || !shortString(method, 16) || !shortString(path, 2_048)) return null
  return { ...base, nonce, method, path }
}

function sleepFor(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) return resolve()
    const done = (): void => { clearTimeout(timer); signal.removeEventListener('abort', done); resolve() }
    const timer = setTimeout(done, ms)
    signal.addEventListener('abort', done)
  })
}

export class PresenceConnection {
  private readonly deps: Required<Omit<PresenceConnectionDeps, 'connect'>> & Pick<PresenceConnectionDeps, 'connect'>
  private abort: AbortController | null = null
  private loop: Promise<void> | null = null
  private connected = false

  constructor(deps: PresenceConnectionDeps) {
    this.deps = {
      nowMs: () => Date.now(),
      sleep: sleepFor,
      log: (message) => console.warn(`[presence] ${message}`),
      ...deps,
    }
  }

  /** True while a presence connection is up: an elevated operation can be confirmed. */
  get ready(): boolean {
    return this.connected
  }

  start(): void {
    if (this.abort) return
    const abort = new AbortController()
    this.abort = abort
    this.loop = this.run(abort.signal)
  }

  /** Close the connection and any open box; resolves once it has ended. */
  async stop(): Promise<void> {
    this.abort?.abort()
    await this.loop
    this.abort = null
    this.loop = null
  }

  private async run(signal: AbortSignal): Promise<void> {
    let backoff = RECONNECT_MIN_MS
    while (!signal.aborted) {
      const target = this.deps.target()
      if (!target) {
        await this.deps.sleep(IDLE_MS, signal)
        continue
      }
      try {
        await this.session(target, signal)
        backoff = RECONNECT_MIN_MS
      } catch (error) {
        if (signal.aborted) break
        // presence_conflict: the daemon still holds an old connection of ours;
        // it clears a dead one, so trying again shortly gets through.
        this.deps.log(`presence connection ended (${error instanceof EnrolError ? error.code : 'internal'})`)
        await this.deps.sleep(backoff, signal)
        backoff = Math.min(backoff * 2, RECONNECT_MAX_MS)
      }
    }
  }

  /** One presence connection, until it ends. */
  private async session(target: PresenceTarget, signal: AbortSignal): Promise<void> {
    const { conn, nonce } = await openEnrolConnection(target.socketPath, this.deps.connect)
    const onAbort = (): void => conn.close()
    signal.addEventListener('abort', onAbort)
    let open: { id: string; abort: AbortController } | null = null
    try {
      const ready = await hello(conn, nonce, target.generation, { role: PRESENCE.role })
      // Never a token here: an `enrolled` frame is as wrong as any other.
      if (ready['op'] !== PRESENCE.ready) throw new EnrolError('bad_frame', 'expected presence_ready')
      this.connected = true
      for (;;) {
        const frame = await conn.next(PRESENCE_SILENCE_MS, new EnrolError('timeout', 'the daemon stopped pinging the presence connection'))
        switch (frame['op']) {
          case PRESENCE.ping: {
            if (!shortString(frame['nonce'])) throw new EnrolError('bad_frame', 'ping carries no nonce')
            conn.send({ op: PRESENCE.pong, nonce: frame['nonce'] })
            break
          }
          case PRESENCE.challenge: {
            const challenge = parseSocketChallenge(frame)
            if (!challenge) throw new EnrolError('bad_frame', 'malformed presence challenge')
            // The daemon sends one at a time; a second while one is open is a fault.
            if (open) throw new EnrolError('bad_frame', 'a second challenge while one is open')
            const box = { id: challenge.challenge_id, abort: new AbortController() }
            open = box
            void this.answer(conn, challenge, box.abort.signal).finally(() => { if (open === box) open = null })
            break
          }
          case PRESENCE.cancel: {
            if (open && frame['challenge_id'] === open.id) open.abort.abort()
            break
          }
          case PRESENCE.answered: {
            if (frame['accepted'] === false) this.deps.log('the daemon did not take an answer (late or settled)')
            break
          }
          default:
            throw new EnrolError('bad_frame', 'unexpected frame on the presence connection')
        }
      }
    } finally {
      this.connected = false
      // The daemon settles an unanswered challenge as Unavailable: close its box.
      ;(open as { abort: AbortController } | null)?.abort.abort()
      signal.removeEventListener('abort', onAbort)
      conn.close()
    }
  }

  /** Ask, then answer on the connection the challenge came on, unless it has
   * expired, been cancelled or the connection ended meanwhile. */
  private async answer(conn: EnrolConnection, challenge: SocketChallenge, cancelled: AbortSignal): Promise<void> {
    const expiry = new AbortController()
    const remaining = challenge.expires_at * 1000 - this.deps.nowMs()
    if (remaining <= 0) return
    const timer = setTimeout(() => expiry.abort(), remaining)
    const onCancel = (): void => expiry.abort()
    cancelled.addEventListener('abort', onCancel)
    let decision: PresenceDecision
    try {
      decision = await this.deps.ask(challenge, expiry.signal)
    } catch {
      decision = 'deny'
    } finally {
      clearTimeout(timer)
      cancelled.removeEventListener('abort', onCancel)
    }
    if (expiry.signal.aborted || conn.closed || this.deps.nowMs() >= challenge.expires_at * 1000) return
    try {
      conn.send({ op: PRESENCE.answer, challenge_id: challenge.challenge_id, nonce: challenge.nonce, decision })
    } catch {
      // The connection ended: the daemon settles it as Unavailable.
    }
  }
}
