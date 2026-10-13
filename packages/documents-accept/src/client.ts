// The harness's line to a running daemon: the documents routes it serves
// under /api/v1/documents, with the bearer it wrote to its state file
// (acceptance README §1). Nothing here reads the data directory or an engine.

import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

export interface Connection {
  /** `http://127.0.0.1:<port>/api/v1/documents` */
  base: string
  token: string
}

/** Reads the daemon's state file: `~/.agent24/daemon.json`, or the one named. */
export function connect(daemonJson = path.join(os.homedir(), '.agent24', 'daemon.json')): Connection {
  const state = JSON.parse(fs.readFileSync(daemonJson, 'utf8')) as { port?: unknown; token?: unknown }
  if (!Number.isInteger(state?.port) || typeof state.token !== 'string' || state.token === '') {
    throw new Error(`${daemonJson}: no port and bearer token (a daemon in capability mode has no token)`)
  }
  return { base: `http://127.0.0.1:${String(state.port)}/api/v1/documents`, token: state.token }
}

/** A route that did not answer 2xx: its status, and the error code of its envelope. */
export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    readonly retryable: boolean,
    message: string,
  ) {
    super(`${status} ${code}: ${message}`)
  }
}

/** What the kernel answers with a 503 before a request reached the module:
 *  nothing happened, so asking again is safe (ADR-DOC-02 §6). */
const NOT_DISPATCHED = ['module_overloaded', 'module_stopping', 'module_not_ready', 'module_draining', 'duplicate_request_id']

export interface Request {
  /** Sent as JSON. */
  json?: unknown
  /** Sent as it is, as `application/octet-stream`. */
  bytes?: Uint8Array
  headers?: Record<string, string>
}

export interface ClientOptions {
  fetch?: typeof fetch
  sleep?: (ms: number) => Promise<void>
  /** How often a retryable 503 is asked again, and how long to wait in between. */
  retries?: number
  retryMs?: number
}

export class Client {
  private readonly fetch: typeof fetch
  private readonly sleep: (ms: number) => Promise<void>
  private readonly retries: number
  private readonly retryMs: number

  constructor(
    private readonly connection: Connection,
    options: ClientOptions = {},
  ) {
    this.fetch = options.fetch ?? fetch
    this.sleep = options.sleep ?? ((ms) => new Promise((done) => setTimeout(done, ms)))
    this.retries = options.retries ?? 60
    this.retryMs = options.retryMs ?? 2000
  }

  /** One request, answered 2xx or thrown as an `ApiError`. A 503 is asked
   *  again as it was when the route calls it retryable (the text layer may
   *  still be parsing) or the kernel never passed the request on: every route
   *  that writes replays an identical request (§1). */
  async send(method: string, route: string, request: Request = {}): Promise<Response> {
    const headers: Record<string, string> = { authorization: `Bearer ${this.connection.token}`, ...request.headers }
    if (request.json !== undefined) headers['content-type'] = 'application/json'
    if (request.bytes !== undefined) headers['content-type'] = 'application/octet-stream'
    const body = request.json !== undefined ? JSON.stringify(request.json) : request.bytes
    for (let attempt = 0; ; attempt += 1) {
      const response = await this.fetch(this.connection.base + route, { method, headers, body })
      if (response.ok) return response
      const error = await failure(response)
      const again = error.status === 503 && (error.retryable || NOT_DISPATCHED.includes(error.code))
      if (!again || attempt >= this.retries) throw error
      await this.sleep(this.retryMs)
    }
  }

  async json<T>(method: string, route: string, request: Request = {}): Promise<T> {
    return (await (await this.send(method, route, request)).json()) as T
  }
}

async function failure(response: Response): Promise<ApiError> {
  const text = await response.text()
  let error: { code?: unknown; message?: unknown; details?: { retryable?: unknown } } | undefined
  try {
    error = (JSON.parse(text) as { error?: typeof error })?.error
  } catch {
    // Not the envelope: the text itself is all there is to say.
  }
  const code = typeof error?.code === 'string' ? error.code : 'unknown'
  const message = typeof error?.message === 'string' ? error.message : text.slice(0, 200)
  return new ApiError(response.status, code, error?.details?.retryable === true, message)
}
