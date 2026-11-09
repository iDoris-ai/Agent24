// KB-SOP-01b (#1670, M3-1c host submit): "submit for review", "check status"
// and "withdraw the submission" behind the drafts page.
//
// The renderer names a draft by id and, for a submit, one knowledge base id
// out of the binding's own list; nothing else it says reaches the request.
// Main builds the whole body from the stored draft (W8.1: only the confirmed
// text and W8.2's metadata), computes session_ref itself, and judges every
// gate again now: the draft's binding is the current one, the post-scan finds
// nothing, the restricted-Markdown check passes, the ready key admits it
// (W1.4), and no earlier submission of this draft is still open.
//
// "Withdraw the submission" is the server's W8.8 and leaves the local draft;
// withdrawing (deleting) the local draft never calls the server.

import { findings } from './knowledge-entry-extract'
import { restrictedMarkdownProblem } from './knowledge-entry-markdown'
import type { KnowledgeEntrySubmission, KnowledgeEntrySubmitAnswer } from '../shared/ipc-types'
import type { EntryDraft } from './knowledge-entry-draft'
import {
  candidateAdmission,
  getCandidate,
  submitCandidate,
  withdrawCandidate,
  type CandidateDeps,
  type CandidateFailure,
  type SubmitBody,
} from './knowledge-candidates'
import { newIdempotencyKey, type CandidateRecord, type CandidateState, type CandidateStore, type PendingSubmit } from './knowledge-candidate-store'

type SubmissionView = KnowledgeEntrySubmission
type SubmissionAnswer = KnowledgeEntrySubmitAnswer

export interface KnowledgeEntrySubmitDeps {
  drafts: { get(id: unknown): EntryDraft | null }
  candidates: CandidateStore
  /** The draft binding Knowledge is bound to now (`entryTarget`), or null. */
  target: () => string | null
  /** The knowledge bases of the binding in effect. */
  knowledgeBaseIds: () => readonly string[]
  /** The Provider's transport for the binding in effect, with its generation; null while Off or incomplete. */
  provider: () => { deps: CandidateDeps; generation: number } | null
  now?: () => Date
}

function viewOf(record: CandidateRecord | null, draft: EntryDraft | null, resendable: (p: PendingSubmit) => boolean): SubmissionView {
  const c = record?.candidate ?? null
  return {
    pending: record?.pending != null,
    stuck: record?.pending != null && !resendable(record.pending),
    candidate: c === null ? null : {
      status: c.status,
      target_knowledge_base_id: c.target_knowledge_base_id,
      return_reason: c.return_reason,
      note: c.note,
      submitted_at: c.submitted_at,
      checked_at: c.checked_at,
      edited: draft !== null && draft.content_sha256 !== c.content_sha256,
    },
  }
}

export function knowledgeEntrySubmission(deps: KnowledgeEntrySubmitDeps) {
  const now = deps.now ?? (() => new Date())
  const busy = new Set<string>()

  const draftOf = (id: unknown): EntryDraft | null => {
    try { return deps.drafts.get(id) } catch { return null }
  }

  const recordOf = (id: string): CandidateRecord | null | 'unreadable' => {
    try { return deps.candidates.get(id) } catch { return 'unreadable' }
  }

  type Provider = { deps: CandidateDeps; generation: number }

  /** The credential a write goes under, as the pending record keeps it; null without one. */
  const credentialRef = (provider: Provider): string | null => {
    try {
      const credential = provider.deps.credential()
      return credential === null ? null : deps.candidates.credentialRef(credential)
    } catch {
      return null
    }
  }

  const resendable = (p: PendingSubmit): boolean => {
    const target = deps.target()
    const provider = deps.provider()
    return target !== null && provider !== null && deps.candidates.resendable(p, target, credentialRef(provider))
  }

  /** R7.3: an answer counts only while its generation and binding are still the current ones. */
  const stillCurrent = (provider: Provider, target: string): boolean => {
    const current = deps.provider()
    return current !== null && current.generation === provider.generation && deps.target() === target
  }

  const view = (id: string, draft: EntryDraft | null): SubmissionView => {
    const record = recordOf(id)
    // A record that cannot be read is not "not submitted": its outcome is unknown.
    if (record === 'unreadable') return { pending: true, stuck: true, candidate: null }
    return viewOf(record, draft, resendable)
  }

  /** The gates status and withdraw share with submit: on, complete, and the draft's own binding (I2). */
  const bound = (draft: EntryDraft): { provider: Provider; target: string } | SubmissionAnswer => {
    const target = deps.target()
    const provider = deps.provider()
    if (target === null || provider === null) return { ok: false, reason: 'knowledge_off', submission: view(draft.id, draft) }
    if (draft.binding !== target) return { ok: false, reason: 'target_changed', submission: view(draft.id, draft) }
    return { provider, target }
  }

  const dropped = (id: string, draft: EntryDraft, unknown: boolean): SubmissionAnswer =>
    ({ ok: false, reason: 'server', code: 'cancelled', unknown, submission: view(id, draft) })

  const serverFailure = (id: string, draft: EntryDraft, f: CandidateFailure): SubmissionAnswer => {
    const answer: SubmissionAnswer = { ok: false, reason: 'server', code: f.code, unknown: f.unknown, submission: view(id, draft) }
    if (f.reason !== undefined) answer.serverReason = f.reason
    return answer
  }

  /** Send `pending` (new, or a resend) and record a definite answer. */
  const send = async (id: string, draft: EntryDraft, pending: PendingSubmit, provider: Provider, target: string, resend: boolean, signal: AbortSignal): Promise<SubmissionAnswer> => {
    const result = await submitCandidate(provider.deps, provider.generation, pending.body, signal)
    // R7.3 / W1.5: an answer to a generation or binding no longer current is
    // dropped; the write may have landed, so the record stays.
    if (!stillCurrent(provider, target)) return dropped(id, draft, true)
    if (result.ok) {
      const state: CandidateState = {
        candidate_id: result.candidateId,
        target_knowledge_base_id: pending.body.target_knowledge_base_id,
        content_sha256: result.contentSha256,
        status: 'submitted',
        return_reason: null,
        note: null,
        submitted_at: result.submittedAt,
        checked_at: now().toISOString(),
      }
      try { deps.candidates.settleSubmit(id, state) } catch { return { ok: false, reason: 'local_storage', submission: view(id, draft) } }
      return { ok: true, submission: view(id, draft) }
    }
    // A definite refusal from the server clears the record (nothing was saved,
    // W1.9). An unknown outcome keeps it, to be resent with the same key and
    // body; so does a resend that never left this machine (no credential,
    // cancelled first): the earlier attempt may still have landed.
    if (!result.unknown && !(resend && result.local)) {
      try { deps.candidates.settleSubmit(id, null) } catch { /* the record stays; it is resent, not lost */ }
    }
    return serverFailure(id, draft, result)
  }

  return {
    view(id: string, draft: EntryDraft | null): SubmissionView {
      return view(id, draft)
    },

    async submit(id: unknown, targetKb: unknown, signal: AbortSignal = new AbortController().signal): Promise<SubmissionAnswer> {
      const draft = draftOf(id)
      if (!draft) return { ok: false, reason: 'not_found' }
      if (busy.has(draft.id)) return { ok: false, reason: 'busy' }
      busy.add(draft.id)
      try {
        const target = deps.target()
        const provider = deps.provider()
        if (target === null || provider === null) return { ok: false, reason: 'knowledge_off' }
        // W8.1 / I2: a draft is bound to the binding it was made under.
        if (draft.binding !== target) return { ok: false, reason: 'target_changed' }
        const record = recordOf(draft.id)
        if (record === 'unreadable') return { ok: false, reason: 'local_storage' }
        const credential = credentialRef(provider)
        if (credential === null) return { ok: false, reason: 'knowledge_off' }
        // W1.5: an earlier submit whose outcome is unknown goes again with the
        // same key and body, under the same binding and credential, or not at
        // all; never a fresh key.
        if (record?.pending) {
          if (!deps.candidates.resendable(record.pending, target, credential)) return { ok: false, reason: 'outcome_unknown', submission: view(draft.id, draft) }
          const admitted = await candidateAdmission(provider.deps, provider.generation, signal)
          if (!admitted.ok) return serverFailure(draft.id, draft, admitted)
          return await send(draft.id, draft, record.pending, provider, target, true, signal)
        }
        if (record?.candidate?.status === 'submitted') return { ok: false, reason: 'already_submitted', submission: view(draft.id, draft) }
        if (record?.candidate?.status === 'approved') return { ok: false, reason: 'already_approved', submission: view(draft.id, draft) }
        if (typeof targetKb !== 'string' || !deps.knowledgeBaseIds().includes(targetKb)) return { ok: false, reason: 'invalid_target' }
        if (findings(draft.title, draft.body).length > 0) return { ok: false, reason: 'findings' }
        const problem = restrictedMarkdownProblem(draft.body)
        if (problem) return { ok: false, reason: 'unsupported_format', problem: { kind: problem.problem, line: problem.line } }
        const admitted = await candidateAdmission(provider.deps, provider.generation, signal)
        if (!admitted.ok) return serverFailure(draft.id, draft, admitted)
        let body: SubmitBody
        let pending: PendingSubmit
        try {
          body = {
            idempotency_key: newIdempotencyKey(),
            target_knowledge_base_id: targetKb,
            mode: 'create',
            kind: draft.kind,
            title: draft.title,
            body: draft.body,
            content_sha256: draft.content_sha256,
            provenance: {
              source: 'agent24_conversation',
              session_ref: deps.candidates.sessionRef(draft.source.conversation_id),
              message_range: [draft.source.message_from, draft.source.message_to],
              skill_version: draft.skill_version,
              redaction_rules_version: draft.redaction_rules_version,
            },
          }
          // Persisted first; if that fails nothing is sent (W1.5).
          pending = deps.candidates.beginSubmit(draft.id, target, credential, body)
        } catch {
          return { ok: false, reason: 'local_storage' }
        }
        return await send(draft.id, draft, pending, provider, target, false, signal)
      } finally {
        busy.delete(draft.id)
      }
    },

    /** W8.3: read the candidate back. Updates the recorded state; a pending submit is left alone. */
    async status(id: unknown, signal: AbortSignal = new AbortController().signal): Promise<SubmissionAnswer> {
      const draft = draftOf(id)
      if (!draft) return { ok: false, reason: 'not_found' }
      const record = recordOf(draft.id)
      if (record === 'unreadable') return { ok: false, reason: 'local_storage' }
      const candidate = record?.candidate
      if (!candidate) return { ok: true, submission: view(draft.id, draft) }
      const gate = bound(draft)
      if ('ok' in gate) return gate
      const { provider, target } = gate
      const got = await getCandidate(provider.deps, provider.generation, candidate.candidate_id, signal)
      if (!stillCurrent(provider, target)) return dropped(draft.id, draft, false)
      if (!got.ok) return serverFailure(draft.id, draft, got)
      try {
        deps.candidates.updateCandidate(draft.id, {
          ...candidate,
          status: got.view.status,
          return_reason: got.view.returnReason,
          note: got.view.note,
          checked_at: now().toISOString(),
        })
      } catch {
        return { ok: false, reason: 'local_storage', submission: view(draft.id, draft) }
      }
      return { ok: true, submission: view(draft.id, draft) }
    },

    /** W8.8: withdraw a `submitted` candidate. The local draft stays. */
    async retract(id: unknown, signal: AbortSignal = new AbortController().signal): Promise<SubmissionAnswer> {
      const draft = draftOf(id)
      if (!draft) return { ok: false, reason: 'not_found' }
      const record = recordOf(draft.id)
      if (record === 'unreadable') return { ok: false, reason: 'local_storage' }
      const candidate = record?.candidate
      if (!candidate || candidate.status !== 'submitted') return { ok: false, reason: 'not_found', submission: view(draft.id, draft) }
      const gate = bound(draft)
      if ('ok' in gate) return gate
      const { provider, target } = gate
      // W1.4: a write, so the ready key admits it first.
      const admitted = await candidateAdmission(provider.deps, provider.generation, signal)
      if (!admitted.ok) return serverFailure(draft.id, draft, admitted)
      const done = await withdrawCandidate(provider.deps, provider.generation, candidate.candidate_id, signal)
      // W8.8 is idempotent: a dropped answer is read back by status or withdrawn again.
      if (!stillCurrent(provider, target)) return dropped(draft.id, draft, true)
      if (!done.ok) return serverFailure(draft.id, draft, done)
      try {
        deps.candidates.updateCandidate(draft.id, { ...candidate, status: 'withdrawn', checked_at: now().toISOString() })
      } catch {
        return { ok: false, reason: 'local_storage', submission: view(draft.id, draft) }
      }
      return { ok: true, submission: view(draft.id, draft) }
    },
  }
}
