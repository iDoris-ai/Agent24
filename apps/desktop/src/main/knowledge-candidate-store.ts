// KB-SOP-01b (#1670, M3-1c host submit): what the host keeps about a draft
// once it has been sent for review, one file per draft beside the draft store.
//
// - The pending-write record (CONTRACT-OPS-v2 W1.5): the exact request body
//   and its idempotency key are written to disk BEFORE the request goes out,
//   and only a definite answer clears them. A retry, after a timeout or a
//   restart, resends the same key and the same body, within the same binding,
//   and never under a new key. Past RESEND_WINDOW_MS, or once the binding or
//   the credential has changed (W1.5's identity tuple: the idempotency scope
//   is per principal, W3.2), it is not resent: the outcome stays unknown and
//   the user checks. The credential is kept only as an HMAC under the local
//   key below, never as itself.
// - The candidate the server accepted, and the last state read back (W8.3).
// - session_ref (W8.2): HMAC-SHA256 under a random key kept only on this
//   machine (0600), so the server gets an opaque, stable reference it cannot
//   turn back into the conversation id.
//
// Nothing here is logged: not the body, not the key, not the conversation id.

import { createHmac, randomBytes } from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import type { SubmitBody } from './knowledge-candidates'

export const CANDIDATE_RECORD_FORMAT = 'agent24-knowledge-candidate'
export const CANDIDATE_RECORD_VERSION = 1
/** Well inside the server's 90-day idempotency retention (W3.3). */
export const RESEND_WINDOW_MS = 30 * 24 * 60 * 60 * 1000

const DRAFT_ID = /^ked_[a-z0-9]{22}$/
const IDEMPOTENCY_KEY = /^[A-Za-z0-9_-]{22,128}$/
const SHA256 = /^[0-9a-f]{64}$/
const UTC_TIME = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?Z$/
const STATUSES = new Set(['submitted', 'approved', 'returned', 'withdrawn'])

export type CandidateStatus = 'submitted' | 'approved' | 'returned' | 'withdrawn'

export interface PendingSubmit {
  idempotency_key: string
  /** `binding` at the moment the record was made: a resend only goes to the same one. */
  binding: string
  /** HMAC of the credential it was sent with: a resend only goes under the same one. */
  credential_ref: string
  body: SubmitBody
  created_at: string
}

export interface CandidateState {
  candidate_id: string
  target_knowledge_base_id: string
  content_sha256: string
  status: CandidateStatus
  return_reason: string | null
  note: string | null
  submitted_at: string
  checked_at: string
}

export interface CandidateRecord {
  format: typeof CANDIDATE_RECORD_FORMAT
  version: typeof CANDIDATE_RECORD_VERSION
  draft_id: string
  pending: PendingSubmit | null
  candidate: CandidateState | null
}

function isObj(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}

function isUtc(v: unknown): v is string {
  return typeof v === 'string' && UTC_TIME.test(v) && !Number.isNaN(Date.parse(v))
}

function isText(v: unknown, max: number): v is string {
  return typeof v === 'string' && v.length >= 1 && v.length <= max
}

function parseBody(v: unknown): SubmitBody | null {
  if (!isObj(v)) return null
  const p = v.provenance
  if (!isObj(p) || p.source !== 'agent24_conversation') return null
  if (typeof p.session_ref !== 'string' || !SHA256.test(p.session_ref)) return null
  const range = p.message_range
  if (!Array.isArray(range) || range.length !== 2 || !range.every((n) => Number.isInteger(n) && n >= 0 && n <= 100000) || range[0] > range[1]) return null
  if (typeof p.skill_version !== 'string' || typeof p.redaction_rules_version !== 'number') return null
  if (typeof v.idempotency_key !== 'string' || !IDEMPOTENCY_KEY.test(v.idempotency_key)) return null
  if (!isText(v.target_knowledge_base_id, 256) || v.mode !== 'create') return null
  if (typeof v.kind !== 'string' || typeof v.title !== 'string' || typeof v.body !== 'string') return null
  if (typeof v.content_sha256 !== 'string' || !SHA256.test(v.content_sha256)) return null
  return {
    idempotency_key: v.idempotency_key,
    target_knowledge_base_id: v.target_knowledge_base_id,
    mode: 'create',
    kind: v.kind,
    title: v.title,
    body: v.body,
    content_sha256: v.content_sha256,
    provenance: {
      source: 'agent24_conversation',
      session_ref: p.session_ref,
      message_range: [range[0], range[1]],
      skill_version: p.skill_version,
      redaction_rules_version: p.redaction_rules_version,
    },
  }
}

function parsePending(v: unknown): PendingSubmit | null | undefined {
  if (v === null) return null
  if (!isObj(v)) return undefined
  const body = parseBody(v.body)
  if (body === null || v.idempotency_key !== body.idempotency_key) return undefined
  if (!isText(v.binding, 2048) || !isUtc(v.created_at)) return undefined
  if (typeof v.credential_ref !== 'string' || !SHA256.test(v.credential_ref)) return undefined
  return { idempotency_key: body.idempotency_key, binding: v.binding, credential_ref: v.credential_ref, body, created_at: v.created_at }
}

function parseCandidate(v: unknown): CandidateState | null | undefined {
  if (v === null) return null
  if (!isObj(v)) return undefined
  if (!isText(v.candidate_id, 256) || !isText(v.target_knowledge_base_id, 256)) return undefined
  if (typeof v.content_sha256 !== 'string' || !SHA256.test(v.content_sha256)) return undefined
  if (typeof v.status !== 'string' || !STATUSES.has(v.status)) return undefined
  if (v.return_reason !== null && !isText(v.return_reason, 64)) return undefined
  if (v.note !== null && !isText(v.note, 1024)) return undefined
  if (!isUtc(v.submitted_at) || !isUtc(v.checked_at)) return undefined
  return {
    candidate_id: v.candidate_id,
    target_knowledge_base_id: v.target_knowledge_base_id,
    content_sha256: v.content_sha256,
    status: v.status as CandidateStatus,
    return_reason: v.return_reason,
    note: v.note,
    submitted_at: v.submitted_at,
    checked_at: v.checked_at,
  }
}

/** A random 128-bit key, base64url (W3.1). */
export function newIdempotencyKey(): string {
  return randomBytes(16).toString('base64url')
}

export function createCandidateStore(dir: string, now: () => Date = () => new Date()) {
  const root = path.join(dir, 'knowledge-candidates')
  const keyFile = path.join(root, '_session_ref.key')

  const ensureRoot = (): void => {
    fs.mkdirSync(root, { recursive: true, mode: 0o700 })
    fs.chmodSync(root, 0o700)
  }

  const fileOf = (draftId: string): string => {
    if (!DRAFT_ID.test(draftId)) throw new Error('Invalid candidate record')
    return path.join(root, `${draftId}.json`)
  }

  const replace = (file: string, body: string): void => {
    ensureRoot()
    const temp = `${file}.${randomBytes(6).toString('hex')}.tmp`
    const fd = fs.openSync(temp, 'wx', 0o600)
    let renamed = false
    try {
      try {
        fs.writeFileSync(fd, body)
        fs.fsyncSync(fd)
      } finally { fs.closeSync(fd) }
      fs.renameSync(temp, file)
      renamed = true
    } finally {
      if (!renamed) fs.rmSync(temp, { force: true })
    }
  }

  /** null: no record. Throws when there is one that cannot be read: never treat it as none (W1.5). */
  const read = (draftId: string): CandidateRecord | null => {
    let raw: string
    try { raw = fs.readFileSync(fileOf(draftId), 'utf8') }
    catch (err) {
      if ((err as NodeJS.ErrnoException).code === 'ENOENT') return null
      throw err
    }
    const v: unknown = JSON.parse(raw)
    if (!isObj(v) || v.format !== CANDIDATE_RECORD_FORMAT || v.version !== CANDIDATE_RECORD_VERSION || v.draft_id !== draftId) {
      throw new Error('Invalid candidate record')
    }
    const pending = parsePending(v.pending)
    const candidate = parseCandidate(v.candidate)
    if (pending === undefined || candidate === undefined) throw new Error('Invalid candidate record')
    return { format: CANDIDATE_RECORD_FORMAT, version: CANDIDATE_RECORD_VERSION, draft_id: draftId, pending, candidate }
  }

  const write = (record: CandidateRecord): void => {
    replace(fileOf(record.draft_id), JSON.stringify(record))
  }

  /** The machine-local HMAC key; made once, 0600, never leaves this directory. */
  const sessionKey = (): Buffer => {
    try {
      const key = fs.readFileSync(keyFile)
      if (key.length === 32) return key
      throw new Error('Invalid session_ref key')
    } catch (err) {
      if ((err as NodeJS.ErrnoException).code !== 'ENOENT') throw err
    }
    ensureRoot()
    const key = randomBytes(32)
    try {
      fs.writeFileSync(keyFile, key, { mode: 0o600, flag: 'wx' })
    } catch (err) {
      // Another writer won the race: use theirs.
      if ((err as NodeJS.ErrnoException).code !== 'EEXIST') throw err
      return sessionKey()
    }
    return key
  }

  return {
    get: read,

    /** W8.2 session_ref: 64 lowercase hex, stable per conversation on this machine, not reversible off it. */
    sessionRef(conversationId: string): string {
      return createHmac('sha256', sessionKey()).update(`agent24-session-ref-v1\n${conversationId}`, 'utf8').digest('hex')
    },

    /** Which credential a pending write went under, without keeping the credential. */
    credentialRef(credential: string): string {
      return createHmac('sha256', sessionKey()).update(`agent24-credential-ref-v1\n${credential}`, 'utf8').digest('hex')
    },

    /**
     * Persist the pending write before sending (W1.5). A failure to persist
     * throws, and then nothing is sent.
     */
    beginSubmit(draftId: string, binding: string, credentialRef: string, body: SubmitBody): PendingSubmit {
      const record = read(draftId)
      if (record?.pending) throw new Error('A submission is already pending')
      if (!SHA256.test(credentialRef)) throw new Error('Invalid credential reference')
      const pending: PendingSubmit = { idempotency_key: body.idempotency_key, binding, credential_ref: credentialRef, body, created_at: now().toISOString() }
      write({
        format: CANDIDATE_RECORD_FORMAT,
        version: CANDIDATE_RECORD_VERSION,
        draft_id: draftId,
        pending,
        candidate: record?.candidate ?? null,
      })
      return pending
    },

    /**
     * Whether a pending write may be resent now (W1.5): same binding, same
     * credential, inside the window. Otherwise it stays pending and unknown.
     */
    resendable(pending: PendingSubmit, binding: string, credentialRef: string | null): boolean {
      if (pending.binding !== binding || pending.credential_ref !== credentialRef) return false
      return now().getTime() - Date.parse(pending.created_at) < RESEND_WINDOW_MS
    },

    /** A definite answer to the pending submit: the candidate, or none when it was refused. */
    settleSubmit(draftId: string, candidate: CandidateState | null): void {
      const record = read(draftId)
      if (!record) return
      write({ ...record, pending: null, candidate: candidate ?? record.candidate })
    },

    /** The latest state read back, or after a withdraw. Only for the candidate already recorded. */
    updateCandidate(draftId: string, candidate: CandidateState): void {
      const record = read(draftId)
      if (!record?.candidate || record.candidate.candidate_id !== candidate.candidate_id) return
      write({ ...record, candidate })
    },

    /** The user gives up on an unknown outcome they have checked themselves. */
    dropPending(draftId: string): void {
      const record = read(draftId)
      if (!record?.pending) return
      write({ ...record, pending: null })
    },
  }
}

export type CandidateStore = ReturnType<typeof createCandidateStore>
