import { describe, expect, it } from 'vitest'
import {
  candidateAdmission,
  getCandidate,
  submitCandidate,
  withdrawCandidate,
  type CandidateDeps,
  type SubmitBody,
} from './knowledge-candidates'

const ORIGIN = 'https://kb.example'
const KEY = 'C-secret-key'
const AUTHZ = `az1_${'0'.repeat(32)}`
const SHA = 'a'.repeat(64)

interface Call { url: string; init: RequestInit; body: Record<string, unknown> }

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } })
}

/** A fake server: `reply` gets the parsed request and answers. */
function deps(reply: (call: Call) => Response | Promise<Response>, credential: string | null = KEY): { deps: CandidateDeps; calls: Call[] } {
  const calls: Call[] = []
  let n = 0
  return {
    calls,
    deps: {
      origin: ORIGIN,
      credential: () => credential,
      newRequestId: () => `req-${++n}`,
      fetch: async (url, init) => {
        const call = { url, init, body: JSON.parse(String(init.body)) as Record<string, unknown> }
        calls.push(call)
        return reply(call)
      },
    },
  }
}

const body: SubmitBody = {
  idempotency_key: 'k'.repeat(22),
  target_knowledge_base_id: 'kb-2',
  mode: 'create',
  kind: 'sop',
  title: 'Leave policy',
  body: 'Ask your lead.',
  content_sha256: SHA,
  provenance: { source: 'agent24_conversation', session_ref: 'b'.repeat(64), message_range: [0, 3], skill_version: 'entry-v1', redaction_rules_version: 1 },
}

const signal = (): AbortSignal => new AbortController().signal

function envelope(call: Call, code: string, reason?: string, extra: Record<string, unknown> = {}): Response {
  return json(code === 'not_found' ? 404 : 400, {
    error: { code, message: 'x', details: { retryable: false, request_id: call.body.request_id, generation: call.body.generation, ...(reason ? { reason } : {}), ...extra } },
  })
}

describe('candidateAdmission (W1.4, HS1)', () => {
  const ready = (state: unknown) => deps((call) => json(200, { request_id: call.body.request_id, generation: call.body.generation, live: true, operations: { candidate: { state }, search: { state: 'ready' } } }))

  it('lets the write through only when operations.candidate is ready', async () => {
    const d = ready('ready')
    expect(await candidateAdmission(d.deps, 7, signal())).toEqual({ ok: true })
    expect(d.calls[0].url).toBe(`${ORIGIN}/api/v1/agent24/knowledge/ready`)
    expect(d.calls[0].body).toEqual({ protocol_version: 1, request_id: 'req-1', generation: 7 })
  })

  it('disabled is a local unavailable/operation_disabled; not_ready, unknown values and no answer are a local not_ready', async () => {
    expect(await candidateAdmission(ready('disabled').deps, 1, signal())).toMatchObject({ ok: false, code: 'unavailable', reason: 'operation_disabled', unknown: false })
    for (const state of ['not_ready', 'readyish', undefined, 1]) {
      expect(await candidateAdmission(ready(state).deps, 1, signal())).toMatchObject({ ok: false, code: 'not_ready', unknown: false })
    }
    const down = deps(() => { throw new Error('ECONNREFUSED') })
    expect(await candidateAdmission(down.deps, 1, signal())).toMatchObject({ ok: false, code: 'not_ready' })
    const wrongEcho = deps(() => json(200, { request_id: 'someone-else', live: true, operations: { candidate: { state: 'ready' } } }))
    expect(await candidateAdmission(wrongEcho.deps, 1, signal())).toMatchObject({ ok: false, code: 'not_ready' })
    // R7.2/R7.3 (Codex H1 C8): the generation is echoed and checked too.
    for (const generation of [undefined, 2, null]) {
      const wrong = deps((call) => json(200, { request_id: call.body.request_id, generation, live: true, operations: { candidate: { state: 'ready' } } }))
      expect(await candidateAdmission(wrong.deps, 1, signal())).toMatchObject({ ok: false, code: 'not_ready' })
    }
  })

  it('no key sends nothing', async () => {
    const d = deps(() => json(200, {}), null)
    expect(await candidateAdmission(d.deps, 1, signal())).toMatchObject({ ok: false, code: 'unauthorized' })
    expect(d.calls).toHaveLength(0)
  })
})

describe('submitCandidate (W8.2)', () => {
  const accepted = (call: Call, details?: unknown) => json(200, {
    request_id: call.body.request_id, generation: call.body.generation, authz_version: AUTHZ,
    candidate_id: 'cand-1', status: 'submitted', content_sha256: SHA, submitted_at: '2026-11-09T08:30:00Z',
    ...(details === undefined ? {} : { details }),
  })

  it('sends the body as given, with the key header, no redirects, and reads the candidate', async () => {
    const d = deps((call) => accepted(call))
    const out = await submitCandidate(d.deps, 3, body, signal())
    expect(out).toEqual({ ok: true, candidateId: 'cand-1', contentSha256: SHA, submittedAt: '2026-11-09T08:30:00Z', existing: false, replayed: false })
    const call = d.calls[0]
    expect(call.url).toBe(`${ORIGIN}/api/v1/agent24/knowledge/candidates`)
    expect(call.init.redirect).toBe('error')
    expect((call.init.headers as Record<string, string>)['X-API-Key']).toBe(KEY)
    expect(call.body).toEqual({ request_id: 'req-1', generation: 3, ...body })
    expect('protocol_version' in call.body).toBe(false)
  })

  it('existing and replayed are successes (HS3)', async () => {
    expect(await submitCandidate(deps((c) => accepted(c, { outcome: 'existing' })).deps, 1, body, signal())).toMatchObject({ ok: true, existing: true })
    expect(await submitCandidate(deps((c) => accepted(c, { replayed: true })).deps, 1, body, signal())).toMatchObject({ ok: true, replayed: true })
  })

  it('a success that does not echo this request, or lacks authz_version, or names another hash is a protocol error and the outcome stays unknown (HS5)', async () => {
    const cases: Array<(c: Call) => Response> = [
      (c) => json(200, { ...JSON.parse('{}'), request_id: 'other', generation: c.body.generation, authz_version: AUTHZ, candidate_id: 'cand-1', status: 'submitted', content_sha256: SHA, submitted_at: '2026-11-09T08:30:00Z' }),
      (c) => json(200, { request_id: c.body.request_id, generation: 99, authz_version: AUTHZ, candidate_id: 'cand-1', status: 'submitted', content_sha256: SHA, submitted_at: '2026-11-09T08:30:00Z' }),
      (c) => json(200, { request_id: c.body.request_id, generation: c.body.generation, candidate_id: 'cand-1', status: 'submitted', content_sha256: SHA, submitted_at: '2026-11-09T08:30:00Z' }),
      (c) => json(200, { request_id: c.body.request_id, generation: c.body.generation, authz_version: '', candidate_id: 'cand-1', status: 'submitted', content_sha256: SHA, submitted_at: '2026-11-09T08:30:00Z' }),
      (c) => json(200, { request_id: c.body.request_id, generation: c.body.generation, authz_version: AUTHZ, candidate_id: 'cand-1', status: 'submitted', content_sha256: 'f'.repeat(64), submitted_at: '2026-11-09T08:30:00Z' }),
      (c) => json(200, { request_id: c.body.request_id, generation: c.body.generation, authz_version: AUTHZ, candidate_id: 'cand-1', status: 'approved', content_sha256: SHA, submitted_at: '2026-11-09T08:30:00Z' }),
    ]
    for (const reply of cases) {
      expect(await submitCandidate(deps(reply).deps, 1, body, signal())).toEqual({ ok: false, code: 'internal', unknown: true })
    }
  })

  it('a refusal is definite; a timeout, internal error or a request in progress leaves it unknown', async () => {
    const refused = [['forbidden', 'operation_not_allowed'], ['forbidden', 'empty_scope'], ['forbidden', 'quota_exceeded'], ['invalid_request', 'unsupported_format'], ['invalid_request', 'content_hash_mismatch'], ['invalid_request', 'too_large']] as const
    for (const [code, reason] of refused) {
      expect(await submitCandidate(deps((c) => envelope(c, code, reason)).deps, 1, body, signal())).toEqual({ ok: false, code, reason, unknown: false })
    }
    expect(await submitCandidate(deps((c) => envelope(c, 'unavailable', 'request_in_progress', { retry_after_ms: 1000 })).deps, 1, body, signal()))
      .toEqual({ ok: false, code: 'unavailable', reason: 'request_in_progress', retryAfterMs: 1000, unknown: true })
    expect(await submitCandidate(deps((c) => envelope(c, 'internal')).deps, 1, body, signal())).toMatchObject({ code: 'internal', unknown: true })
    expect(await submitCandidate(deps(() => { throw new Error('reset') }).deps, 1, body, signal())).toMatchObject({ code: 'unavailable', unknown: true })
  })

  it('an envelope must echo this request; null only for the codes R6.1 allows', async () => {
    const nullEcho = (code: string) => (_c: Call) => json(400, { error: { code, message: 'x', details: { retryable: false, request_id: null, generation: null } } })
    expect(await submitCandidate(deps(nullEcho('invalid_request')).deps, 1, body, signal())).toMatchObject({ code: 'invalid_request', unknown: false })
    expect(await submitCandidate(deps(nullEcho('forbidden')).deps, 1, body, signal())).toEqual({ ok: false, code: 'internal', unknown: true })
    const other = (c: Call) => json(403, { error: { code: 'forbidden', message: 'x', details: { retryable: false, request_id: 'other', generation: c.body.generation } } })
    expect(await submitCandidate(deps(other).deps, 1, body, signal())).toEqual({ ok: false, code: 'internal', unknown: true })
    // An unknown code is internal (R6.2).
    expect(await submitCandidate(deps((c) => envelope(c, 'teapot')).deps, 1, body, signal())).toMatchObject({ code: 'internal' })
  })

  it('no key sends nothing', async () => {
    const d = deps(() => json(200, {}), null)
    expect(await submitCandidate(d.deps, 1, body, signal())).toEqual({ ok: false, code: 'unauthorized', unknown: false, local: true })
    expect(d.calls).toHaveLength(0)
  })

  it('an aborted call before sending is cancelled and not unknown', async () => {
    const c = new AbortController()
    c.abort()
    const d = deps(() => json(200, {}))
    expect(await submitCandidate(d.deps, 1, body, c.signal)).toEqual({ ok: false, code: 'cancelled', unknown: false, local: true })
    expect(d.calls).toHaveLength(0)
  })
})

describe('getCandidate (W8.3) and withdrawCandidate (W8.8)', () => {
  const view = (over: Record<string, unknown> = {}) => ({
    candidate_id: 'cand-1', status: 'returned', kind: 'sop', content_sha256: SHA, target_knowledge_base_id: 'kb-2', mode: 'create',
    return_reason: 'reviewer', note: 'Please add the form link.', submitted_at: '2026-11-09T08:30:00Z', updated_at: '2026-11-09T09:00:00Z', ...over,
  })

  it('reads the four states, the return reason and the note', async () => {
    const d = deps((c) => json(200, { request_id: c.body.request_id, generation: c.body.generation, authz_version: AUTHZ, candidate: view() }))
    const got = await getCandidate(d.deps, 2, 'cand-1', signal())
    expect(got).toMatchObject({ ok: true, view: { status: 'returned', returnReason: 'reviewer', note: 'Please add the form link.' } })
    expect(d.calls[0].url).toBe(`${ORIGIN}/api/v1/agent24/knowledge/candidates/get`)
    expect(d.calls[0].body).toEqual({ request_id: 'req-1', generation: 2, candidate_id: 'cand-1' })
    for (const status of ['submitted', 'approved', 'withdrawn']) {
      const s = deps((c) => json(200, { request_id: c.body.request_id, generation: c.body.generation, authz_version: AUTHZ, candidate: view({ status, return_reason: undefined }) }))
      expect(await getCandidate(s.deps, 2, 'cand-1', signal())).toMatchObject({ ok: true, view: { status, returnReason: null } })
    }
  })

  it('an unknown return reason is "other"; another candidate or a bad status is a protocol error', async () => {
    const odd = deps((c) => json(200, { request_id: c.body.request_id, generation: c.body.generation, authz_version: AUTHZ, candidate: view({ return_reason: 'brand_new_reason' }) }))
    expect(await getCandidate(odd.deps, 2, 'cand-1', signal())).toMatchObject({ ok: true, view: { returnReason: 'other' } })
    const other = deps((c) => json(200, { request_id: c.body.request_id, generation: c.body.generation, authz_version: AUTHZ, candidate: view({ candidate_id: 'cand-2' }) }))
    expect(await getCandidate(other.deps, 2, 'cand-1', signal())).toEqual({ ok: false, code: 'internal', unknown: false })
    const bad = deps((c) => json(200, { request_id: c.body.request_id, generation: c.body.generation, authz_version: AUTHZ, candidate: view({ status: 'deleted' }) }))
    expect(await getCandidate(bad.deps, 2, 'cand-1', signal())).toEqual({ ok: false, code: 'internal', unknown: false })
  })

  it('withdraw: withdrawn, or already withdrawn; not submitted is stale', async () => {
    const ok = (details?: unknown) => (c: Call) => json(200, { request_id: c.body.request_id, generation: c.body.generation, authz_version: AUTHZ, candidate_id: 'cand-1', status: 'withdrawn', ...(details ? { details } : {}) })
    const d = deps(ok())
    expect(await withdrawCandidate(d.deps, 2, 'cand-1', signal())).toEqual({ ok: true, already: false })
    expect(d.calls[0].url).toBe(`${ORIGIN}/api/v1/agent24/knowledge/candidates/withdraw`)
    expect(await withdrawCandidate(deps(ok({ already: true })).deps, 2, 'cand-1', signal())).toEqual({ ok: true, already: true })
    expect(await withdrawCandidate(deps((c) => envelope(c, 'stale', 'candidate_not_submitted')).deps, 2, 'cand-1', signal()))
      .toEqual({ ok: false, code: 'stale', reason: 'candidate_not_submitted', unknown: false })
  })
})

describe('nothing secret leaves in the clear (HS4/HS5)', () => {
  it('the key is only ever in the header, never in the URL or the body', async () => {
    const d = deps((c) => envelope(c, 'forbidden', 'operation_not_allowed'))
    await submitCandidate(d.deps, 1, body, signal())
    await getCandidate(d.deps, 1, 'cand-1', signal())
    await withdrawCandidate(d.deps, 1, 'cand-1', signal())
    for (const call of d.calls) {
      expect(call.url).not.toContain(KEY)
      expect(JSON.stringify(call.body)).not.toContain(KEY)
    }
  })
})
