// KB-C2-04a (#1623): main's calls to the UI-session endpoints of WeKnora
// (KB-C2-01b, WeKnora#139/#140): ticket issuance and revocation with C, the
// exchange and the session probe with the view proof. Shapes follow #140:
//   POST {origin}/api/v1/agent24/knowledge/ui/tickets      X-API-Key: C
//        {binding_tag, partition_tag, vph, generation} → {ticket, ticket_id, expires_in}
//   POST …/ui/revocations                                   X-API-Key: C
//        {ticket_id} or {vph} → 204
//   POST …/ui/exchange      X-Agent24-View-Proof            {ticket} → UiExchange
//   GET  …/ui/session       Authorization: Bearer, proof    → {expires_in}
//
// Tickets, JWTs, C and the view secret never reach a log, an error message or
// IPC (U2.5): what comes back out of here is a reason, never a request.

import { createHash, randomBytes } from 'node:crypto'
import { readUiError, readUiExchange, readUiProbe, type UiErrorRead, type UiExchange } from './knowledge-ui-session'

const PREFIX = '/api/v1/agent24/knowledge/ui'
const TAG = /^[A-Za-z0-9_-]{43}$/
const TICKET_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/

/** Request limits: issuance and exchange are short; a revocation never holds up a teardown (U3.7). */
export const UI_CALL_TIMEOUT_MS = 10_000
export const UI_REVOKE_TIMEOUT_MS = 2_000

/** UIB §1: SHA-256 over be32(len(tag)) ‖ tag ‖ be32(len(x)) ‖ x …, unpadded base64url. */
export function uiHash(tag: string, ...parts: Uint8Array[]): string {
  const hash = createHash('sha256')
  const piece = (bytes: Uint8Array) => {
    const len = Buffer.alloc(4)
    len.writeUInt32BE(bytes.length)
    hash.update(len)
    hash.update(bytes)
  }
  piece(Buffer.from(tag, 'utf8'))
  for (const part of parts) piece(part)
  return hash.digest('base64url')
}

export interface ViewSecret {
  /** The X-Agent24-View-Proof value: the raw secret, unpadded base64url. */
  proof: string
  vph: string
}

/** A new view instance's secret (U1, 32 bytes CSPRNG) and its family id. */
export function newViewSecret(random: (n: number) => Uint8Array = randomBytes): ViewSecret {
  const secret = random(32)
  return { proof: Buffer.from(secret).toString('base64url'), vph: uiHash('kb-vph-v1', secret) }
}

export const partitionTag = (partition: string) => uiHash('kb-partition-v1', Buffer.from(partition, 'utf8'))
export const bindingTag = (appInstanceId: string, bindingId: string) =>
  uiHash('kb-binding-v1', Buffer.from(appInstanceId, 'utf8'), Buffer.from(bindingId, 'utf8'))

/** The fetch main uses: Electron's `net.fetch` in the app, a fake in tests. */
export type UiFetch = (url: string, init: {
  method: 'GET' | 'POST'
  headers: Record<string, string>
  body?: string
  redirect: 'error'
  cache: 'no-store'
  signal: AbortSignal
}) => Promise<{ status: number; json(): Promise<unknown> }>

/** Every call ends in one of these; `unknown` is a timeout, a dropped connection or a body that could not be read (U2.9). */
export type UiCallResult<T> = { kind: 'ok'; value: T } | { kind: 'refused'; error: Extract<UiErrorRead, { kind: 'error' }> } | { kind: 'unknown' }

export interface UiTicket {
  ticket: string
  ticketId: string
  expiresInS: number
}

export class KnowledgeUiClient {
  constructor(private readonly fetch: UiFetch, private readonly origin: string) {}

  issueTicket(credential: string, tags: { bindingTag: string; partitionTag: string; vph: string }, generation: number): Promise<UiCallResult<UiTicket>> {
    if (![tags.bindingTag, tags.partitionTag, tags.vph].every((t) => TAG.test(t)) || !Number.isSafeInteger(generation) || generation < 0) {
      return Promise.resolve({ kind: 'unknown' })
    }
    const body = JSON.stringify({ binding_tag: tags.bindingTag, partition_tag: tags.partitionTag, vph: tags.vph, generation })
    return this.call('POST', '/tickets', { 'X-API-Key': credential }, body, UI_CALL_TIMEOUT_MS, 'issue', (b) => {
      if (typeof b !== 'object' || b === null) return null
      const { ticket, ticket_id: id, expires_in: expires } = b as Record<string, unknown>
      if (typeof ticket !== 'string' || ticket === '' || typeof id !== 'string' || !TICKET_ID.test(id)) return null
      if (typeof expires !== 'number' || !Number.isInteger(expires) || expires <= 0) return null
      return { ticket, ticketId: id, expiresInS: expires }
    })
  }

  /** One attempt only: a ticket is never sent twice (U2.9, C2-01b2 replay). */
  exchange(ticket: string, view: ViewSecret): Promise<UiCallResult<UiExchange>> {
    return this.call('POST', '/exchange', { 'X-Agent24-View-Proof': view.proof }, JSON.stringify({ ticket }), UI_CALL_TIMEOUT_MS, 'exchange',
      readUiExchange)
  }

  probe(sessionToken: string, view: ViewSecret): Promise<UiCallResult<{ expiresInS: number }>> {
    return this.call('GET', '/session', { Authorization: `Bearer ${sessionToken}`, 'X-Agent24-View-Proof': view.proof }, undefined, UI_CALL_TIMEOUT_MS, 'probe', readUiProbe)
  }

  revokeFamily(credential: string, vph: string): Promise<UiCallResult<null>> {
    if (!TAG.test(vph)) return Promise.resolve({ kind: 'unknown' })
    return this.call('POST', '/revocations', { 'X-API-Key': credential }, JSON.stringify({ vph }), UI_REVOKE_TIMEOUT_MS, 'issue', () => null, 204)
  }

  revokeTicket(credential: string, ticketId: string): Promise<UiCallResult<null>> {
    if (!TICKET_ID.test(ticketId)) return Promise.resolve({ kind: 'unknown' })
    return this.call('POST', '/revocations', { 'X-API-Key': credential }, JSON.stringify({ ticket_id: ticketId }), UI_REVOKE_TIMEOUT_MS, 'issue', () => null, 204)
  }

  private async call<T>(
    method: 'GET' | 'POST', path: string, headers: Record<string, string>, body: string | undefined, timeoutMs: number,
    from: 'issue' | 'exchange' | 'probe', read: (body: unknown) => T | null, okStatus = 200,
  ): Promise<UiCallResult<T>> {
    const abort = new AbortController()
    const timer = setTimeout(() => abort.abort(), timeoutMs)
    try {
      const response = await this.fetch(`${this.origin}${PREFIX}${path}`, {
        method,
        headers: { ...headers, ...(body === undefined ? {} : { 'Content-Type': 'application/json' }) },
        ...(body === undefined ? {} : { body }),
        redirect: 'error',
        cache: 'no-store',
        signal: abort.signal,
      })
      if (response.status === okStatus) {
        if (okStatus === 204) return { kind: 'ok', value: read(null) as T }
        const value = read(await response.json())
        return value === null ? { kind: 'unknown' } : { kind: 'ok', value }
      }
      const error = readUiError(await response.json().catch(() => null), from === 'issue' ? 'exchange' : from)
      return error.kind === 'error' ? { kind: 'refused', error } : { kind: 'unknown' }
    } catch {
      return { kind: 'unknown' }
    } finally {
      clearTimeout(timer)
    }
  }
}
