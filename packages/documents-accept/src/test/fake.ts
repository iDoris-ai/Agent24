// A stand-in for the daemon in unit tests: a `Client` whose requests go to a
// function, which sees each one as the routes would.

import { Client, type ClientOptions } from '../client'

export interface Call {
  method: string
  /** The path under /api/v1/documents, with its query. */
  route: string
  headers: Record<string, string>
  json?: unknown
  bytes?: Uint8Array
}

export const answer = (body: unknown, status = 200, headers: Record<string, string> = {}): Response =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json', ...headers } })

export const refuse = (status: number, code: string, retryable = false): Response =>
  answer({ error: { code, message: `it is ${code}`, details: { retryable } } }, status)

const BASE = 'http://127.0.0.1:1/api/v1/documents'

/** A client answered by `daemon`, and every call it made, in order. */
export function fakeClient(daemon: (call: Call, calls: Call[]) => Response, options: ClientOptions = {}): { client: Client; calls: Call[] } {
  const calls: Call[] = []
  const fetch = (async (url: string, init: RequestInit = {}) => {
    const body = init.body
    const call: Call = {
      method: init.method ?? 'GET',
      route: String(url).slice(BASE.length),
      headers: init.headers as Record<string, string>,
      ...(typeof body === 'string' ? { json: JSON.parse(body) as unknown } : {}),
      ...(body instanceof Uint8Array ? { bytes: body } : {}),
    }
    calls.push(call)
    return daemon(call, calls)
  }) as unknown as typeof globalThis.fetch
  const client = new Client({ base: BASE, token: 'secret' }, { fetch, sleep: async () => {}, ...options })
  return { client, calls }
}
