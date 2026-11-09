import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { createCandidateStore } from './knowledge-candidate-store'
import type { CandidateDeps } from './knowledge-candidates'
import { createEntryDraftStore, entryHash, type EntryDraft } from './knowledge-entry-draft'
import { knowledgeEntrySubmission } from './knowledge-entry-submit'

const AUTHZ = `az1_${'0'.repeat(32)}`
const BINDING = 'kbt1:binding-a'
const KEY = 'C-secret-key'
let dir: string

beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'ksubmit-'))
})
afterEach(() => {
  fs.rmSync(dir, { recursive: true, force: true })
})

interface Call { path: string; body: Record<string, unknown> }

/** A sent body without its per-request id (a resend gets a new one). */
function withoutRequestId(body: Record<string, unknown>): Record<string, unknown> {
  const copy = { ...body }
  delete copy.request_id
  return copy
}

/** A fake WeKnora with S2's three routes and ready. */
function server(opts: { candidate?: string; submit?: (c: Call) => Response | 'throw'; status?: string } = {}) {
  const calls: Call[] = []
  let ids = 0
  const ok = (c: Call, rest: Record<string, unknown>) =>
    new Response(JSON.stringify({ request_id: c.body.request_id, generation: c.body.generation, authz_version: AUTHZ, ...rest }), { status: 200 })
  const deps: CandidateDeps = {
    origin: 'https://kb.example',
    credential: () => KEY,
    newRequestId: () => `req-${++ids}`,
    fetch: async (url, init) => {
      const c = { path: url.replace('https://kb.example/api/v1/agent24/knowledge/', ''), body: JSON.parse(String(init.body)) as Record<string, unknown> }
      calls.push(c)
      if (c.path === 'ready') {
        return new Response(JSON.stringify({ request_id: c.body.request_id, live: true, operations: { candidate: { state: opts.candidate ?? 'ready' } } }), { status: 200 })
      }
      if (c.path === 'candidates') {
        const custom = opts.submit?.(c)
        if (custom === 'throw') throw new Error('connection reset')
        if (custom) return custom
        return ok(c, { candidate_id: 'cand-1', status: 'submitted', content_sha256: c.body.content_sha256, submitted_at: '2026-11-09T08:30:00Z' })
      }
      if (c.path === 'candidates/get') {
        return ok(c, { candidate: { candidate_id: c.body.candidate_id, status: opts.status ?? 'returned', kind: 'sop', content_sha256: 'x'.repeat(64).replace(/x/g, 'a'), target_knowledge_base_id: 'kb-2', mode: 'create', return_reason: 'reviewer', note: 'Add the form.', submitted_at: '2026-11-09T08:30:00Z', updated_at: '2026-11-09T09:00:00Z' } })
      }
      if (c.path === 'candidates/withdraw') return ok(c, { candidate_id: c.body.candidate_id, status: 'withdrawn' })
      throw new Error(`unexpected ${c.path}`)
    },
  }
  return { deps, calls, writes: () => calls.filter((c) => c.path !== 'ready') }
}

function setup(opts: Parameters<typeof server>[0] = {}, binding = BINDING) {
  const drafts = createEntryDraftStore(dir)
  const candidates = createCandidateStore(dir)
  const fake = server(opts)
  let target: string | null = binding
  const submission = knowledgeEntrySubmission({
    drafts,
    candidates,
    target: () => target,
    knowledgeBaseIds: () => ['kb-1', 'kb-2'],
    provider: () => ({ deps: fake.deps, generation: 4 }),
  })
  const draft = (over: Partial<EntryDraft> = {}): EntryDraft => drafts.create({
    binding: BINDING,
    source: { conversation_id: 'chat_00000000000000000001', message_from: 0, message_to: 3 },
    floor: 'ordinary',
    skill_version: 'entry-v1',
    redaction_rules_version: 1,
    kind: 'sop',
    title: 'Leave policy',
    body: 'Ask your lead, then file [the form](https://hr.example/form).',
    ...over,
  })
  return { drafts, candidates, fake, submission, draft, setTarget: (t: string | null) => { target = t } }
}

describe('submit (W8.2)', () => {
  it('main builds the body from the stored draft, with session_ref and a fresh key, and records the candidate', async () => {
    const t = setup()
    const d = t.draft()
    const out = await t.submission.submit(d.id, 'kb-2')
    expect(out).toMatchObject({ ok: true, submission: { pending: false, candidate: { status: 'submitted', target_knowledge_base_id: 'kb-2', edited: false } } })
    const sent = t.fake.writes()[0].body
    expect(sent).toMatchObject({
      generation: 4, target_knowledge_base_id: 'kb-2', mode: 'create', kind: 'sop', title: d.title, body: d.body, content_sha256: d.content_sha256,
      provenance: { source: 'agent24_conversation', session_ref: t.candidates.sessionRef(d.source.conversation_id), message_range: [0, 3], skill_version: 'entry-v1', redaction_rules_version: 1 },
    })
    expect(sent.idempotency_key).toMatch(/^[A-Za-z0-9_-]{22}$/)
    // HS-N1: nothing the renderer could add rides along; the conversation id never leaves.
    expect(Object.keys(sent).sort()).toEqual(['body', 'content_sha256', 'generation', 'idempotency_key', 'kind', 'mode', 'provenance', 'request_id', 'target_knowledge_base_id', 'title'])
    expect(JSON.stringify(sent)).not.toContain(d.source.conversation_id)
    expect(t.fake.calls[0].path).toBe('ready')
  })

  it('is refused locally, sending nothing, when a gate does not hold', async () => {
    const t = setup()
    const d = t.draft()
    expect(await t.submission.submit(d.id, 'kb-9')).toMatchObject({ ok: false, reason: 'invalid_target' })
    expect(await t.submission.submit(d.id, { approved: true })).toMatchObject({ ok: false, reason: 'invalid_target' })
    const leaky = t.draft({ body: 'Mail me at someone@example.com' })
    expect(await t.submission.submit(leaky.id, 'kb-2')).toMatchObject({ ok: false, reason: 'findings' })
    const html = t.draft({ body: 'Click <b>here</b>' })
    expect(await t.submission.submit(html.id, 'kb-2')).toMatchObject({ ok: false, reason: 'unsupported_format', problem: { line: 1 } })
    t.setTarget('kbt1:other')
    expect(await t.submission.submit(d.id, 'kb-2')).toMatchObject({ ok: false, reason: 'target_changed' })
    t.setTarget(null)
    expect(await t.submission.submit(d.id, 'kb-2')).toMatchObject({ ok: false, reason: 'knowledge_off' })
    expect(await t.submission.submit(`ked_${'z'.repeat(22)}`, 'kb-2')).toMatchObject({ ok: false, reason: 'not_found' })
    expect(t.fake.calls).toHaveLength(0)
  })

  it('W1.4: ready not admitting it sends no write (HS1)', async () => {
    for (const candidate of ['disabled', 'not_ready', 'surprise']) {
      const t = setup({ candidate })
      const d = t.draft()
      const out = await t.submission.submit(d.id, 'kb-2')
      expect(out).toMatchObject({ ok: false, reason: 'server', code: candidate === 'disabled' ? 'unavailable' : 'not_ready', unknown: false })
      expect(t.fake.writes()).toHaveLength(0)
      expect(t.candidates.get(d.id)).toBeNull()
    }
  })

  it('an unknown outcome keeps the record; the retry sends the same key and the same body (HS3)', async () => {
    let fail = true
    const t = setup({ submit: () => (fail ? 'throw' : undefined as unknown as Response) })
    const d = t.draft()
    const first = await t.submission.submit(d.id, 'kb-2')
    expect(first).toMatchObject({ ok: false, reason: 'server', unknown: true, submission: { pending: true, stuck: false } })
    const kept = t.candidates.get(d.id)?.pending
    expect(kept).not.toBeNull()
    fail = false
    // The retry may even name another KB: the pending body wins (no new key, no new body).
    const again = await t.submission.submit(d.id, 'kb-1')
    expect(again).toMatchObject({ ok: true, submission: { pending: false, candidate: { status: 'submitted', target_knowledge_base_id: 'kb-2' } } })
    const [a, b] = t.fake.writes().map((c) => withoutRequestId(c.body))
    expect(b).toEqual(a)
    expect(b.idempotency_key).toBe(kept?.idempotency_key)
  })

  it('HS2: brackets and parentheses in an https destination are encoded when saved; stored = sent, and the hash matches', async () => {
    const t = setup()
    const d = t.draft({ body: 'See [the form](https://hr.example/a[1](v2)) and [x]: https://hr.example/b[2]' })
    expect(d.body).toBe('See [the form](https://hr.example/a%5B1%5D%28v2%29) and [x]: https://hr.example/b%5B2%5D')
    expect(d.content_sha256).toBe(entryHash({ kind: d.kind, title: d.title, body: d.body }))
    expect(await t.submission.submit(d.id, 'kb-2')).toMatchObject({ ok: true })
    const sent = t.fake.writes()[0].body
    expect(sent.body).toBe(d.body)
    expect(sent.content_sha256).toBe(d.content_sha256)
    // NUL and a lone surrogate never become a draft.
    expect(() => t.draft({ body: 'a\u0000b' })).toThrow()
    expect(() => t.draft({ title: 'x\uD800' })).toThrow()
  })

  it('HS3: after a restart, the unknown submit goes again with the same key and the same body', async () => {
    const first = setup({ submit: () => 'throw' })
    const d = first.draft()
    await first.submission.submit(d.id, 'kb-2')
    const sentBefore = first.fake.writes()[0].body
    // A new process: new stores and a new submission over the same directory.
    const again = setup()
    expect(await again.submission.submit(d.id, 'kb-1')).toMatchObject({ ok: true })
    expect(withoutRequestId(again.fake.writes()[0].body)).toEqual(withoutRequestId(sentBefore))
  })

  it('HS4: the session_ref key and the conversation id appear in no request, header or answer', async () => {
    const t = setup({ submit: () => 'throw' })
    const d = t.draft()
    const out = await t.submission.submit(d.id, 'kb-2')
    t.candidates.sessionRef('warm')
    const keyHex = fs.readFileSync(path.join(dir, 'knowledge-candidates', '_session_ref.key')).toString('hex')
    const keyB64 = fs.readFileSync(path.join(dir, 'knowledge-candidates', '_session_ref.key')).toString('base64')
    const seen = JSON.stringify([t.fake.calls, out])
    for (const needle of [keyHex, keyB64, d.source.conversation_id]) expect(seen).not.toContain(needle)
  })

  it('the record is on disk before the request goes out', async () => {
    let seen: unknown = 'not checked'
    const t = setup({ submit: () => { seen = t.candidates.get(draftId)?.pending?.idempotency_key; return undefined as unknown as Response } })
    const d = t.draft()
    const draftId = d.id
    await t.submission.submit(d.id, 'kb-2')
    expect(typeof seen).toBe('string')
    expect(seen).toBe(t.fake.writes()[0].body.idempotency_key)
  })

  it('once the binding changed, an unknown outcome is not resent: it stays unknown for the user to check', async () => {
    const t = setup({ submit: () => 'throw' })
    const d = t.draft()
    await t.submission.submit(d.id, 'kb-2')
    t.setTarget('kbt1:other')
    expect(t.submission.view(d.id, d)).toMatchObject({ pending: true, stuck: true })
    t.setTarget(BINDING)
    // Same binding again: resendable.
    expect(t.submission.view(d.id, d)).toMatchObject({ pending: true, stuck: false })
  })

  it('a definite refusal clears the record, so a corrected draft can go under a new key', async () => {
    const t = setup({ submit: (c) => new Response(JSON.stringify({ error: { code: 'forbidden', message: 'x', details: { retryable: false, request_id: c.body.request_id, generation: c.body.generation, reason: 'operation_not_allowed' } } }), { status: 403 }) })
    const d = t.draft()
    expect(await t.submission.submit(d.id, 'kb-2')).toMatchObject({ ok: false, reason: 'server', code: 'forbidden', serverReason: 'operation_not_allowed', unknown: false, submission: { pending: false } })
    expect(t.candidates.get(d.id)?.pending).toBeNull()
  })

  it('a submitted or approved candidate is not submitted again', async () => {
    const t = setup()
    const d = t.draft()
    await t.submission.submit(d.id, 'kb-2')
    expect(await t.submission.submit(d.id, 'kb-2')).toMatchObject({ ok: false, reason: 'already_submitted' })
    expect(t.fake.writes()).toHaveLength(1)
  })
})

describe('status and withdrawing the submission (W8.3, W8.8, HS6)', () => {
  it('status reads the state, the return reason and the note back', async () => {
    const t = setup({ status: 'returned' })
    const d = t.draft()
    await t.submission.submit(d.id, 'kb-2')
    const out = await t.submission.status(d.id)
    expect(out).toMatchObject({ ok: true, submission: { candidate: { status: 'returned', return_reason: 'reviewer', note: 'Add the form.' } } })
  })

  it('an edit after submitting shows as edited', async () => {
    const t = setup()
    const d = t.draft()
    await t.submission.submit(d.id, 'kb-2')
    const edited = t.drafts.update(d.id, { title: 'Leave policy (v2)' })
    expect(t.submission.view(d.id, edited).candidate?.edited).toBe(true)
  })

  it('withdrawing the submission calls the server and keeps the local draft', async () => {
    const t = setup()
    const d = t.draft()
    await t.submission.submit(d.id, 'kb-2')
    const out = await t.submission.retract(d.id)
    expect(out).toMatchObject({ ok: true, submission: { candidate: { status: 'withdrawn' } } })
    expect(t.fake.writes().map((c) => c.path)).toEqual(['candidates', 'candidates/withdraw'])
    expect(t.drafts.get(d.id)).not.toBeNull()
    // Withdrawn: a new submission is allowed again.
    expect(await t.submission.submit(d.id, 'kb-2')).toMatchObject({ ok: true })
  })

  it('there is nothing to withdraw before it is submitted', async () => {
    const t = setup()
    const d = t.draft()
    expect(await t.submission.retract(d.id)).toMatchObject({ ok: false, reason: 'not_found' })
    expect(t.fake.calls).toHaveLength(0)
  })
})
