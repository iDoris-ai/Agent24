// Hermetic fake of the development inbound route. The real Agent24Client posts
// here. Anything else, including a run create, is 404.

import http from 'node:http'
import type { AddressInfo } from 'node:net'

export interface RecordedInbound {
  id: string
  actor: string
  at: string
  text?: string
  source?: string
}

export class FakeDaemon {
  private readonly server: http.Server
  /** Bodies the route accepted, in order. A failed status is not one of these. */
  readonly events: RecordedInbound[] = []
  /** Every inbound POST, including ones answered with `failures`. */
  readonly attempts: RecordedInbound[] = []
  /** Status codes consumed before a normal 200. `401` and `500` are typical. */
  readonly failures: number[] = []
  /** These actors are answered `ignored` / `not_allowlisted`. Everyone else is `started`. */
  readonly ignoredActors = new Set<string>()
  port = 0

  constructor() {
    this.server = http.createServer((req, res) => void this.handle(req, res))
  }

  async listen(): Promise<this> {
    await new Promise<void>((resolve) => this.server.listen(0, '127.0.0.1', resolve))
    this.port = (this.server.address() as AddressInfo).port
    return this
  }

  get baseUrl(): string {
    return `http://127.0.0.1:${this.port}`
  }

  async close(): Promise<void> {
    await new Promise<void>((resolve, reject) =>
      this.server.close((err) => (err ? reject(err) : resolve())),
    )
  }

  private async handle(req: http.IncomingMessage, res: http.ServerResponse): Promise<void> {
    const method = req.method ?? 'GET'
    const path = (req.url ?? '').split('?')[0] ?? ''
    const body = (await readJson(req)) as RecordedInbound

    if (method === 'POST' && path === '/api/v1/inbound/events') {
      this.attempts.push(body)
      const failed = this.failures.shift()
      if (failed !== undefined) {
        send(res, { error: { code: 'failed', message: 'nope' } }, failed)
        return
      }
      this.events.push(body)
      if (this.ignoredActors.has(body.actor)) {
        send(res, { id: body.id, outcome: 'ignored', reason: 'not_allowlisted' })
      } else {
        send(res, { id: body.id, outcome: 'started', run_id: 'run_fake' })
      }
      return
    }
    send(res, {}, 404)
  }
}

function readJson(req: http.IncomingMessage): Promise<unknown> {
  return new Promise((resolve) => {
    let raw = ''
    req.on('data', (c) => (raw += c))
    req.on('end', () => {
      try {
        resolve(raw ? JSON.parse(raw) : {})
      } catch {
        resolve({})
      }
    })
  })
}

function send(res: http.ServerResponse, payload: unknown, status = 200): void {
  res.writeHead(status, { 'Content-Type': 'application/json' })
  res.end(JSON.stringify(payload))
}
