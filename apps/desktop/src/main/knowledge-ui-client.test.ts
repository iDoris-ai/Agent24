import { describe, expect, it, vi } from 'vitest'
import { bindingTag, KnowledgeUiClient, newViewSecret, partitionTag, uiHash, UI_CALL_TIMEOUT_MS, type UiFetch } from './knowledge-ui-client'

const ORIGIN = 'https://kb.example.org'
const SECRET = Uint8Array.from({ length: 32 }, (_, i) => i)
const VIEW = newViewSecret(() => SECRET)
const TAGS = { bindingTag: bindingTag('app-1', 'binding-1'), partitionTag: partitionTag('agent24-knowledge-v1:TDIzDN1Zhf2uSO3A6KrFSA'), vph: VIEW.vph }
const TICKET_ID = '0b8f3a64-3c2d-4e1f-9a7b-1c2d3e4f5a6b'

type Call = Parameters<UiFetch>
function fake(status: number, body: unknown = {}) {
  const calls: Call[] = []
  const fetch: UiFetch = async (...args) => { calls.push(args); return { status, json: async () => body } }
  return { fetch, calls }
}

describe('uiHash (UIB §1), against vectors from the server code (agent24uisession.H)', () => {
  it('matches the server byte for byte', () => {
    expect(VIEW.vph).toBe('oXHV1-znyZXXSq0mo5zki6p1m5e0ys3r2VGaXV59vPQ')
    expect(TAGS.partitionTag).toBe('nCvDqd7EhSjtoP2vmHu92nIo14bCowlzmjceShMY0L4')
    expect(TAGS.bindingTag).toBe('s7ifZtDVJiAScBkrPBp6iGpX8CjvWIgYAD9S_eZ1S2U')
    expect(VIEW.proof).toBe('AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8')
  })

  it('length-prefixes every part, so moving bytes between parts changes the hash', () => {
    expect(uiHash('t', Buffer.from('ab'), Buffer.from('c'))).not.toBe(uiHash('t', Buffer.from('a'), Buffer.from('bc')))
  })

  it('makes a fresh 32-byte secret by default', () => {
    const a = newViewSecret()
    expect(Buffer.from(a.proof, 'base64url')).toHaveLength(32)
    expect(newViewSecret().proof).not.toBe(a.proof)
  })
})

describe('KnowledgeUiClient (WeKnora#140 shapes)', () => {
  it('issues a ticket with C, the three tags and the generation', async () => {
    const f = fake(200, { ticket: 'tkt', ticket_id: TICKET_ID, expires_in: 30, extra: 1 })
    const result = await new KnowledgeUiClient(f.fetch, ORIGIN).issueTicket('C', TAGS, 3)
    expect(result).toEqual({ kind: 'ok', value: { ticket: 'tkt', ticketId: TICKET_ID, expiresInS: 30 } })
    const [url, init] = f.calls[0]
    expect(url).toBe(`${ORIGIN}/api/v1/agent24/knowledge/ui/tickets`)
    expect(init).toMatchObject({ method: 'POST', redirect: 'error', cache: 'no-store', headers: { 'X-API-Key': 'C', 'Content-Type': 'application/json' } })
    expect(JSON.parse(init.body!)).toEqual({ binding_tag: TAGS.bindingTag, partition_tag: TAGS.partitionTag, vph: TAGS.vph, generation: 3 })
  })

  it('does not send a malformed issuance at all', async () => {
    const f = fake(200)
    const client = new KnowledgeUiClient(f.fetch, ORIGIN)
    expect(await client.issueTicket('C', { ...TAGS, vph: 'short' }, 1)).toEqual({ kind: 'unknown' })
    expect(await client.issueTicket('C', TAGS, -1)).toEqual({ kind: 'unknown' })
    expect(await client.issueTicket('C', TAGS, 1.5)).toEqual({ kind: 'unknown' })
    expect(f.calls).toHaveLength(0)
  })

  it('refuses an issuance answer it cannot trust', async () => {
    for (const body of [{ ticket: '', ticket_id: TICKET_ID, expires_in: 30 }, { ticket: 't', ticket_id: TICKET_ID.toUpperCase(), expires_in: 30 },
      { ticket: 't', ticket_id: TICKET_ID, expires_in: 0 }, null]) {
      expect(await new KnowledgeUiClient(fake(200, body).fetch, ORIGIN).issueTicket('C', TAGS, 1)).toEqual({ kind: 'unknown' })
    }
  })

  it('exchanges with the view proof and the ticket in the body only', async () => {
    const f = fake(200, { session_token: 'jwt', expires_in: 600, ops: ['kb.list'], effective_scope: { knowledge_base_ids: ['kb-a'] } })
    const result = await new KnowledgeUiClient(f.fetch, ORIGIN).exchange('tkt', VIEW, ['kb-a'])
    expect(result).toEqual({ kind: 'ok', value: { sessionToken: 'jwt', expiresInS: 600, ops: ['kb.list'], knowledgeBaseIds: ['kb-a'] } })
    const [url, init] = f.calls[0]
    expect(url).toBe(`${ORIGIN}/api/v1/agent24/knowledge/ui/exchange`)
    expect(init.headers).toEqual({ 'X-Agent24-View-Proof': VIEW.proof, 'Content-Type': 'application/json' })
    expect(JSON.parse(init.body!)).toEqual({ ticket: 'tkt' })
    expect(url).not.toContain('tkt')
  })

  it('probes with the JWT and the proof, no body', async () => {
    const f = fake(200, { expires_in: 120 })
    expect(await new KnowledgeUiClient(f.fetch, ORIGIN).probe('jwt', VIEW)).toEqual({ kind: 'ok', value: { expiresInS: 120 } })
    const [url, init] = f.calls[0]
    expect(url).toBe(`${ORIGIN}/api/v1/agent24/knowledge/ui/session`)
    expect(init).toMatchObject({ method: 'GET', headers: { Authorization: 'Bearer jwt', 'X-Agent24-View-Proof': VIEW.proof } })
    expect(init.body).toBeUndefined()
  })

  it('passes refusals through, restarted only from the probe', async () => {
    const body = { error: { code: 'unauthorized', message: 'm', details: { retryable: false, reason: 'credential_invalid', restarted: true } } }
    expect(await new KnowledgeUiClient(fake(401, body).fetch, ORIGIN).probe('jwt', VIEW))
      .toEqual({ kind: 'refused', error: { kind: 'error', code: 'unauthorized', reason: 'credential_invalid', restarted: true, retryAfterMs: null } })
    expect(await new KnowledgeUiClient(fake(401, body).fetch, ORIGIN).exchange('t', VIEW, ['kb-a']))
      .toEqual({ kind: 'refused', error: { kind: 'error', code: 'unauthorized', reason: 'credential_invalid', restarted: false, retryAfterMs: null } })
    expect(await new KnowledgeUiClient(fake(503, { error: { code: 'unavailable', message: 'm', details: { reason: 'overloaded' } } }).fetch, ORIGIN).issueTicket('C', TAGS, 1))
      .toMatchObject({ kind: 'refused', error: { code: 'unavailable', reason: 'overloaded' } })
  })

  it('gives unknown for a dropped connection, an unreadable body or a timeout', async () => {
    const thrown: UiFetch = async () => { throw new Error('ECONNRESET') }
    expect(await new KnowledgeUiClient(thrown, ORIGIN).exchange('t', VIEW, ['kb-a'])).toEqual({ kind: 'unknown' })
    const notJson: UiFetch = async () => ({ status: 502, json: async () => { throw new Error('html') } })
    expect(await new KnowledgeUiClient(notJson, ORIGIN).probe('jwt', VIEW)).toEqual({ kind: 'unknown' })
    vi.useFakeTimers()
    try {
      let signal: AbortSignal | undefined
      const hang: UiFetch = (_url, init) => { signal = init.signal; return new Promise((_r, reject) => init.signal.addEventListener('abort', () => reject(new Error('aborted')))) }
      const pending = new KnowledgeUiClient(hang, ORIGIN).exchange('t', VIEW, ['kb-a'])
      await vi.advanceTimersByTimeAsync(UI_CALL_TIMEOUT_MS)
      expect(signal?.aborted).toBe(true)
      expect(await pending).toEqual({ kind: 'unknown' })
    } finally {
      vi.useRealTimers()
    }
  })

  it('revokes a family or one ticket with C, 204 is success', async () => {
    const f = fake(204)
    const client = new KnowledgeUiClient(f.fetch, ORIGIN)
    expect(await client.revokeFamily('C', VIEW.vph)).toEqual({ kind: 'ok', value: null })
    expect(await client.revokeTicket('C', TICKET_ID)).toEqual({ kind: 'ok', value: null })
    expect(f.calls.map(([, init]) => JSON.parse(init.body!))).toEqual([{ vph: VIEW.vph }, { ticket_id: TICKET_ID }])
    expect(f.calls.every(([url, init]) => url.endsWith('/ui/revocations') && init.headers['X-API-Key'] === 'C')).toBe(true)
    expect(await client.revokeTicket('C', TICKET_ID.toUpperCase())).toEqual({ kind: 'unknown' })
    expect(f.calls).toHaveLength(2)
  })
})
