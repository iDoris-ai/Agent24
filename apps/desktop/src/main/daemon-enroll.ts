// M5 B2 (#1711): main's side of the capabilities-mode enrolment protocol v1,
// ported from `rust/apps/agent24-cli/src/enroll.rs`. A daemon the LaunchAgent
// runs (`agent24 service install`) gives this app no host token; daemon.json
// advertises an `enroll_endpoint` (a Unix socket) instead. Main connects,
// reads the daemon's `challenge`, answers `hello`, and receives a day-to-day
// FirstPartyShell token (TTL 1 h). The connection stays open: `renew` is only
// accepted on the connection the token was issued on, and the daemon closes a
// connection that sends nothing for 10 minutes (enroll_endpoint.rs
// IDLE_DEADLINE), so main renews well inside both.
//
// One JSON line per frame, at most 16 KiB, every frame `v: 1`. The handshake
// (`openEnrolConnection` + `hello`) is shared with the presence connection of
// a later slice (B3, `role: "presence"`).
//
// No token is ever logged, and no error built here carries one.

import { createHash } from 'node:crypto'
import net from 'node:net'

export const ENROL_PROTOCOL_V = 1
/** Frame size cap, both directions (CORE-2 plan §3.1). */
export const MAX_FRAME_BYTES = 16 * 1024
/** Per-step budget of the handshake and of each renew (enroll.rs HANDSHAKE_STEP). */
export const ENROL_STEP_MS = 5_000
/** How often the token connection renews: under the daemon's 10-minute idle
 * cut-off and far under the token's 1-hour TTL. */
export const RENEW_EVERY_MS = 5 * 60_000
const NONCE_HEX = /^[0-9a-fA-F]{32}$/
/** How long an `elevate` waits for the user's answer (enroll.rs ELEVATION_WAIT):
 * the daemon's challenge lives 60 s. */
export const ELEVATION_WAIT_MS = 65_000
/** The daemon refuses a longer `summary` (enroll_endpoint.rs), counted in characters. */
export const SUMMARY_MAX_CHARS = 200
/** The header an elevated retry carries its grant in (server.rs). */
export const ELEVATION_HEADER = 'X-Agent24-Elevation'

/** Lowercase hex SHA-256 of the exact body bytes sent (enroll.rs body_sha256). */
export function bodySha256(body: string): string {
  return createHash('sha256').update(body, 'utf8').digest('hex')
}

/** Codes that mean the daemon will not enrol this build as it is: an unsigned
 * or foreign binary (`identity_rejected`), or a protocol it does not speak. */
export const REJECTED_CODES: ReadonlySet<string> = new Set(['identity_rejected', 'unsupported_version'])

/** A failure while enrolling or renewing. Neither field carries a credential. */
export class EnrolError extends Error {
  constructor(readonly code: string, message: string) {
    super(message)
    this.name = 'EnrolError'
  }
}

export type Frame = Record<string, unknown>

/** One enrolment-socket connection, framed. */
export class EnrolConnection {
  private buffer = Buffer.alloc(0)
  private readonly frames: Frame[] = []
  private failure: EnrolError | null = null
  private waiter: (() => void) | null = null
  private closeListeners: Array<(why: EnrolError) => void> = []

  constructor(private readonly socket: net.Socket) {
    socket.on('data', (chunk: Buffer) => this.onData(chunk))
    socket.on('error', () => this.fail(new EnrolError('daemon_unavailable', 'the enrolment connection failed')))
    socket.on('close', () => this.fail(new EnrolError('daemon_unavailable', 'the daemon closed the enrolment connection')))
  }

  private onData(chunk: Buffer): void {
    if (this.failure) return
    this.buffer = Buffer.concat([this.buffer, chunk])
    for (;;) {
      const nl = this.buffer.indexOf(0x0a)
      if (nl < 0) break
      // As the Rust readers count it: with its newline, at most 16 KiB (Codex B2 C3).
      if (nl + 1 > MAX_FRAME_BYTES) return this.fail(new EnrolError('bad_frame', 'daemon frame exceeds 16 KiB'))
      const line = this.buffer.subarray(0, nl).toString('utf8')
      this.buffer = this.buffer.subarray(nl + 1)
      let frame: unknown
      try {
        frame = JSON.parse(line)
      } catch {
        return this.fail(new EnrolError('bad_frame', 'daemon sent a malformed frame'))
      }
      if (typeof frame !== 'object' || frame === null || Array.isArray(frame)) {
        return this.fail(new EnrolError('bad_frame', 'daemon sent a malformed frame'))
      }
      if ((frame as Frame)['v'] !== ENROL_PROTOCOL_V) {
        return this.fail(new EnrolError('unsupported_version', 'daemon speaks another enrolment protocol version'))
      }
      this.frames.push(frame as Frame)
    }
    if (this.buffer.length >= MAX_FRAME_BYTES) this.fail(new EnrolError('bad_frame', 'daemon frame exceeds 16 KiB'))
    this.wake()
  }

  private wake(): void {
    const waiter = this.waiter
    this.waiter = null
    waiter?.()
  }

  private fail(why: EnrolError): void {
    if (this.failure) return
    this.failure = why
    this.socket.destroy()
    this.wake()
    for (const listener of this.closeListeners.splice(0)) listener(why)
  }

  /** Called once, when the connection ends for any reason (close() included). */
  onClose(listener: (why: EnrolError) => void): void {
    if (this.failure) listener(this.failure)
    else this.closeListeners.push(listener)
  }

  get closed(): boolean {
    return this.failure !== null
  }

  /** The next frame, within `timeoutMs`; a daemon `error` frame rejects with its code. */
  async next(timeoutMs: number, onTimeout = new EnrolError('timeout', 'timed out during the enrolment handshake')): Promise<Frame> {
    const deadline = Date.now() + timeoutMs
    for (;;) {
      const frame = this.frames.shift()
      if (frame) {
        if (frame['op'] === 'error') throw errorFromFrame(frame)
        return frame
      }
      if (this.failure) throw this.failure
      const left = deadline - Date.now()
      if (left <= 0) {
        this.fail(onTimeout)
        throw onTimeout
      }
      await new Promise<void>((resolve) => {
        const timer = setTimeout(resolve, left)
        this.waiter = () => { clearTimeout(timer); resolve() }
      })
    }
  }

  send(frame: Frame): void {
    if (this.failure) throw this.failure
    const bytes = Buffer.from(JSON.stringify({ v: ENROL_PROTOCOL_V, ...frame }) + '\n', 'utf8')
    if (bytes.length > MAX_FRAME_BYTES) throw new EnrolError('bad_frame', 'frame exceeds 16 KiB')
    this.socket.write(bytes)
  }

  /** Close, naming why (default: closed by this app). */
  close(why: unknown = null): void {
    this.fail(why instanceof EnrolError ? why : new EnrolError('closed', 'the enrolment connection was closed'))
  }
}

/** A daemon `error` frame as an [`EnrolError`]; the daemon's message is for
 * humans and never holds a secret. */
function errorFromFrame(frame: Frame): EnrolError {
  const code = typeof frame['code'] === 'string' ? frame['code'] : 'bad_frame'
  const message = typeof frame['message'] === 'string' && frame['message'] ? frame['message'] : `enrolment failed (${code})`
  return new EnrolError(code, message)
}

export type Connect = (socketPath: string) => net.Socket

const defaultConnect: Connect = (socketPath) => net.createConnection({ path: socketPath })

/** Connect and wait for the daemon's `challenge`; nothing is written before it
 * arrives (bytes queued before the daemon's accept are refused). Returns the
 * connection and the challenge's nonce. */
export async function openEnrolConnection(
  socketPath: string,
  connect: Connect = defaultConnect,
): Promise<{ conn: EnrolConnection; nonce: string }> {
  const conn = new EnrolConnection(connect(socketPath))
  try {
    const challenge = await conn.next(ENROL_STEP_MS)
    if (challenge['op'] !== 'challenge') throw new EnrolError('bad_frame', 'expected a challenge frame')
    const nonce = challenge['nonce']
    if (typeof nonce !== 'string' || !NONCE_HEX.test(nonce)) {
      throw new EnrolError('bad_frame', 'challenge frame carries no valid nonce')
    }
    return { conn, nonce }
  } catch (error) {
    conn.close(error)
    throw error
  }
}

/** Send `hello` echoing the nonce verbatim, with `generation` from daemon.json
 * (the daemon refuses a mismatch) and any extra fields (B3: `role`), and
 * return the daemon's answer. */
export async function hello(conn: EnrolConnection, nonce: string, generation: string, extra: Frame = {}): Promise<Frame> {
  conn.send({ ...extra, op: 'hello', generation, nonce })
  return conn.next(ENROL_STEP_MS)
}

/** The token of an `enrolled` frame, checked against daemon.json's port: the
 * daemon names the port it listens on, so a daemon.json pointing elsewhere
 * never receives the token (#1446 F1). */
function enrolledToken(frame: Frame, httpPort: number): { token: string; expiresIn: number } {
  if (frame['op'] !== 'enrolled') throw new EnrolError('bad_frame', 'expected an enrolled frame')
  const token = frame['token']
  if (typeof token !== 'string' || token === '') throw new EnrolError('bad_frame', 'enrolled frame carries no token')
  if (frame['http_port'] !== httpPort) {
    throw new EnrolError('endpoint_mismatch', "daemon.json's port is not the port of the daemon that enrolled this app")
  }
  const expiresIn = typeof frame['expires_in'] === 'number' ? frame['expires_in'] : 0
  return { token, expiresIn }
}

/** One request to elevate: exactly what will be sent (the grant binds method,
 * path and body), and a line for the confirmation box. */
export interface ElevateRequest {
  method: string
  /** No query string: the daemon refuses a grant on a request that has one. */
  path: string
  /** The exact body bytes sent, '' for none. */
  body: string
  summary: string
}

/** The token connection of an enrolled app. */
export interface EnrolSession {
  /** The current day-to-day token. Replaced by each renew; the old one is void. */
  readonly token: string
  /** Renew on this connection; resolves with the new token. */
  renew(): Promise<string>
  /** A turn on this connection (M5 B3): no renew comes until `fn` has
   * finished, so `token` (passed to `fn`) is the one throughout: the daemon
   * binds both the request it remembers on a 403 and the grant to it, so
   * whatever `fn` sends must carry it. Refused at once on a connection that
   * has ended (Codex B3b r2 C1). In it, `elevate`
   * asks for a one-shot grant for `request`, waiting for the user up to
   * `ELEVATION_WAIT_MS`; it rejects with an [`EnrolError`], and a daemon
   * `error` frame (denied, timed out, unavailable) leaves the connection open.
   * One `elevate` at a time, and the turn ends only once it has settled
   * (PR-Daemon #1736 C1/C2). `fn` must settle: renews wait for it, so callers
   * bound it (sendElevating: each request has its deadline, elevate its wait; C3). */
  turn<T>(fn: (elevate: (request: ElevateRequest) => Promise<string>, token: string) => Promise<T>): Promise<T>
  /** Called once when the connection ends (the daemon went away, a renew failed, close()). */
  onClose(listener: (why: EnrolError) => void): void
  close(): void
}

export interface EnrolTarget {
  socketPath: string
  generation: string
  /** daemon.json's port, where the token will be sent. */
  httpPort: number
}

/** Enrol as this app's first-party shell. Rejects with an [`EnrolError`].
 * `elevationWaitMs` is for tests only. */
export async function enrol(
  target: EnrolTarget, connect: Connect = defaultConnect, { elevationWaitMs = ELEVATION_WAIT_MS } = {},
): Promise<EnrolSession> {
  const { conn, nonce } = await openEnrolConnection(target.socketPath, connect)
  let current: string
  try {
    current = enrolledToken(await hello(conn, nonce, target.generation), target.httpPort).token
  } catch (error) {
    conn.close(error)
    throw error
  }
  let renewing: Promise<string> | null = null
  // One exchange at a time: frames are read in order, and the daemon drops
  // the connection on any input while it waits for the user (enroll_endpoint.rs).
  let tail: Promise<unknown> = Promise.resolve()
  const exclusive = <T>(fn: () => Promise<T>): Promise<T> => {
    const run = tail.then(fn, fn)
    tail = run.catch(() => {})
    return run
  }
  const broken = (message: string): never => {
    const error = new EnrolError('bad_frame', message)
    conn.close(error)
    throw error
  }
  return {
    get token() { return current },
    renew() {
      if (renewing) return renewing
      const attempt = exclusive(async () => {
        try {
          conn.send({ op: 'renew' })
          const next = enrolledToken(
            await conn.next(ENROL_STEP_MS, new EnrolError('timeout', 'timed out renewing the enrolment')),
            target.httpPort,
          ).token
          current = next
          return next
        } catch (error) {
          conn.close(error)
          throw error
        }
      })
      // Cleared once this attempt settles, and only if it is still the one
      // held: a send that throws at once settles it before it is held
      // (PR-Daemon #1725).
      renewing = attempt
      attempt.then(() => { if (renewing === attempt) renewing = null }, () => { if (renewing === attempt) renewing = null })
      return attempt
    },
    turn(fn) {
      return exclusive(async () => {
        if (conn.closed) throw new EnrolError('closed', 'the enrolment connection has ended')
        let inTurn = true
        let outstanding: Promise<string> | null = null
        const once = (request: ElevateRequest): Promise<string> => {
          if (!inTurn) return Promise.reject(new EnrolError('closed', 'elevate outside its turn'))
          if (outstanding) return Promise.reject(new EnrolError('busy', 'one elevate at a time'))
          const asked = elevate(request)
          outstanding = asked
          asked.then(() => { outstanding = null }, () => { outstanding = null })
          return asked
        }
        try {
          return await fn(once, current)
        } finally {
          inTurn = false
          // Nothing else may go on the connection while the daemon waits for the user.
          await (outstanding as Promise<string> | null)?.catch(() => {})
        }
      })
    },
    onClose: (listener) => conn.onClose(listener),
    close: () => { conn.close() },
  }

  async function elevate(request: ElevateRequest): Promise<string> {
    // One exchange: the caller's turn keeps everything else off the connection.
    conn.send({
      op: 'elevate',
      method: request.method,
      path: request.path,
      body_sha256: bodySha256(request.body),
      summary: Array.from(request.summary).slice(0, SUMMARY_MAX_CHARS).join(''),
    })
    const deadline = Date.now() + elevationWaitMs
    let pending: string | null = null
    for (;;) {
      // Timing out closes the connection; an `error` frame rejects and leaves it open.
      const frame = await conn.next(
        Math.max(0, deadline - Date.now()),
        new EnrolError('presence_timeout', 'timed out waiting for the confirmation'),
      )
      // A frame read from what was queued once the deadline passed or the
      // connection failed comes too late (PR-Daemon #1736 C4/C5).
      if (Date.now() >= deadline) {
        const late = new EnrolError('presence_timeout', 'timed out waiting for the confirmation')
        conn.close(late)
        throw late
      }
      if (conn.closed) throw new EnrolError('closed', 'the enrolment connection has ended')
      const id = frame['challenge_id']
      if (frame['op'] === 'elevation_pending') {
        if (pending !== null) broken('duplicate elevation_pending frame')
        if (typeof id !== 'string' || id === '') broken('pending frame carries no challenge id')
        pending = id as string
        continue
      }
      if (frame['op'] !== 'elevated') broken('unexpected frame while waiting for the confirmation')
      if (pending === null || id !== pending) broken('elevated frame does not match the pending challenge')
      const grant = frame['grant']
      if (typeof grant !== 'string' || grant === '') broken('elevated frame carries no grant')
      return grant as string
    }
  }
}
