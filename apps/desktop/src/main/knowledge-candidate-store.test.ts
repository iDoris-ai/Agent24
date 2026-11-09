import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { createCandidateStore, newIdempotencyKey, RESEND_WINDOW_MS, type CandidateState } from './knowledge-candidate-store'
import type { SubmitBody } from './knowledge-candidates'

const DRAFT = `ked_${'a'.repeat(22)}`
let dir: string

beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kcand-'))
})
afterEach(() => {
  fs.rmSync(dir, { recursive: true, force: true })
})

const body = (key = newIdempotencyKey()): SubmitBody => ({
  idempotency_key: key,
  target_knowledge_base_id: 'kb-2',
  mode: 'create',
  kind: 'sop',
  title: 'Leave policy',
  body: 'Ask your lead.',
  content_sha256: 'a'.repeat(64),
  provenance: { source: 'agent24_conversation', session_ref: 'b'.repeat(64), message_range: [0, 3], skill_version: 'entry-v1', redaction_rules_version: 1 },
})

const state = (over: Partial<CandidateState> = {}): CandidateState => ({
  candidate_id: 'cand-1', target_knowledge_base_id: 'kb-2', content_sha256: 'a'.repeat(64), status: 'submitted',
  return_reason: null, note: null, submitted_at: '2026-11-09T08:30:00Z', checked_at: '2026-11-09T08:30:01Z', ...over,
})

const mode = (file: string): number => fs.statSync(file).mode & 0o777

describe('the pending write (W1.5, HS3)', () => {
  it('is on disk, 0600, with the exact body and key, before anything is sent; a second one is refused', () => {
    const store = createCandidateStore(dir)
    const b = body()
    store.beginSubmit(DRAFT, 'kbt1:x', b)
    const file = path.join(dir, 'knowledge-candidates', `${DRAFT}.json`)
    expect(mode(file)).toBe(0o600)
    expect(mode(path.dirname(file))).toBe(0o700)
    // A new store (a restart) reads back the same key and the same body.
    const again = createCandidateStore(dir).get(DRAFT)
    expect(again?.pending?.body).toEqual(b)
    expect(again?.pending?.idempotency_key).toBe(b.idempotency_key)
    expect(() => store.beginSubmit(DRAFT, 'kbt1:x', body())).toThrow()
  })

  it('a record that cannot be persisted throws, so nothing is sent', () => {
    const store = createCandidateStore(dir)
    fs.writeFileSync(path.join(dir, 'knowledge-candidates'), 'not a directory')
    expect(() => store.beginSubmit(DRAFT, 'kbt1:x', body())).toThrow()
  })

  it('is resent only within the same binding and inside the window', () => {
    let now = new Date('2026-11-09T08:00:00Z')
    const store = createCandidateStore(dir, () => now)
    const pending = store.beginSubmit(DRAFT, 'kbt1:x', body())
    expect(store.resendable(pending, 'kbt1:x')).toBe(true)
    expect(store.resendable(pending, 'kbt1:y')).toBe(false)
    now = new Date(now.getTime() + RESEND_WINDOW_MS)
    expect(store.resendable(pending, 'kbt1:x')).toBe(false)
  })

  it('a definite answer clears it and keeps the candidate; a refusal clears it and keeps none', () => {
    const store = createCandidateStore(dir)
    store.beginSubmit(DRAFT, 'kbt1:x', body())
    store.settleSubmit(DRAFT, state())
    expect(store.get(DRAFT)).toMatchObject({ pending: null, candidate: { candidate_id: 'cand-1', status: 'submitted' } })
    store.updateCandidate(DRAFT, state({ status: 'withdrawn' }))
    store.beginSubmit(DRAFT, 'kbt1:x', body())
    store.settleSubmit(DRAFT, null)
    // A refused resubmit leaves the earlier candidate as it was.
    expect(store.get(DRAFT)).toMatchObject({ pending: null, candidate: { status: 'withdrawn' } })
  })

  it('updateCandidate only touches the candidate already recorded', () => {
    const store = createCandidateStore(dir)
    store.beginSubmit(DRAFT, 'kbt1:x', body())
    store.settleSubmit(DRAFT, state())
    store.updateCandidate(DRAFT, state({ candidate_id: 'cand-2', status: 'approved' }))
    expect(store.get(DRAFT)?.candidate).toMatchObject({ candidate_id: 'cand-1', status: 'submitted' })
  })

  it('a record that does not parse is an error, never "no submission"', () => {
    const store = createCandidateStore(dir)
    store.beginSubmit(DRAFT, 'kbt1:x', body())
    const file = path.join(dir, 'knowledge-candidates', `${DRAFT}.json`)
    fs.writeFileSync(file, JSON.stringify({ format: 'agent24-knowledge-candidate', version: 1, draft_id: DRAFT, pending: { junk: true }, candidate: null }))
    expect(() => store.get(DRAFT)).toThrow()
    fs.writeFileSync(file, '{')
    expect(() => store.get(DRAFT)).toThrow()
    expect(store.get(`ked_${'b'.repeat(22)}`)).toBeNull()
  })
})

describe('session_ref (W8.2, HS4)', () => {
  it('is 64 lowercase hex, stable for a conversation, different across conversations, and not the id', () => {
    const store = createCandidateStore(dir)
    const a = store.sessionRef('chat_abc')
    expect(a).toMatch(/^[0-9a-f]{64}$/)
    expect(createCandidateStore(dir).sessionRef('chat_abc')).toBe(a)
    expect(store.sessionRef('chat_abd')).not.toBe(a)
    expect(a).not.toContain('chat_abc')
  })

  it('is keyed by a 32-byte machine key in a 0600 file; another key gives another value', () => {
    const store = createCandidateStore(dir)
    const a = store.sessionRef('chat_abc')
    const keyFile = path.join(dir, 'knowledge-candidates', '_session_ref.key')
    expect(fs.readFileSync(keyFile)).toHaveLength(32)
    expect(mode(keyFile)).toBe(0o600)
    fs.rmSync(keyFile)
    expect(createCandidateStore(dir).sessionRef('chat_abc')).not.toBe(a)
  })

  it('a damaged key file is an error, not silently replaced', () => {
    const store = createCandidateStore(dir)
    store.sessionRef('chat_abc')
    fs.writeFileSync(path.join(dir, 'knowledge-candidates', '_session_ref.key'), 'short')
    expect(() => store.sessionRef('chat_abc')).toThrow()
  })
})

describe('newIdempotencyKey (W3.1)', () => {
  it('is 128 random bits in base64url', () => {
    const a = newIdempotencyKey()
    expect(a).toMatch(/^[A-Za-z0-9_-]{22}$/)
    expect(newIdempotencyKey()).not.toBe(a)
  })
})
