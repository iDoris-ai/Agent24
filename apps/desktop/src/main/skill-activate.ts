// Skill activation and rollback, main process only (skill-loader plan §4.9).
//
// `POST /api/v1/skills/{id}/activate` is Elevated and is deliberately not on
// the renderer's backend proxy. The page may call only this handler, and it
// supplies a skill id, the target revision ids (or one history id) and the
// failing case ids it accepts. Everything shown in the confirmation comes from
// the daemon's own record, read here with the host credential. Any summary,
// generation or verdict field the page sends along is never read.
//
// Pure of Electron imports; ipc/index.ts supplies `call` and `confirm`.

import type { BackendProxyRequest, BackendProxyResponse } from '../shared/ipc-types'
import { mt } from './main-i18n'

export interface SkillActivateDeps {
  /** Host-credential call to the daemon. */
  call: (req: BackendProxyRequest) => Promise<BackendProxyResponse>
  /** Native yes/no. The text is built here, never taken from the page. */
  confirm: (message: string, detail: string) => Promise<boolean>
}

export type SkillActivateInput =
  | { skillId: string; bodyRevisionId: string; evalsRevisionId: string; accepted: string[] }
  | { skillId: string; rollbackActivationId: string }

const SKILL_ID = /^[a-z0-9][a-z0-9-]{1,31}$/
const REF_ID = /^[A-Za-z0-9_.:-]{1,128}$/
const MAX_ACCEPTED = 256

const fail = (status: number, code: string, message: string): BackendProxyResponse =>
  ({ ok: false, status, data: { error: { code, message } } })

const isRefId = (v: unknown): v is string => typeof v === 'string' && REF_ID.test(v)

/** Reads exactly the fields this IPC takes. Every other key is dropped unread. */
export function parseSkillActivateInput(raw: unknown): SkillActivateInput | null {
  if (typeof raw !== 'object' || raw === null) return null
  const r = raw as Record<string, unknown>
  if (typeof r.skillId !== 'string' || !SKILL_ID.test(r.skillId)) return null
  const rollback = r.rollbackActivationId
  const body = r.bodyRevisionId
  const evals = r.evalsRevisionId
  if (rollback !== undefined) {
    if (body !== undefined || evals !== undefined || r.accepted !== undefined) return null
    return isRefId(rollback) ? { skillId: r.skillId, rollbackActivationId: rollback } : null
  }
  if (!isRefId(body) || !isRefId(evals)) return null
  let accepted: string[] = []
  if (r.accepted !== undefined) {
    if (!Array.isArray(r.accepted) || r.accepted.length > MAX_ACCEPTED || !r.accepted.every(isRefId)) return null
    accepted = [...(r.accepted as string[])]
  }
  return { skillId: r.skillId, bodyRevisionId: body, evalsRevisionId: evals, accepted }
}

interface Revision { revision_id: string; origin: string; semver: string; kind: string; content_digest: string }
interface Pointer { origin: string; body_revision_id: string; evals_revision_id: string; generation: number }
interface Activation { activation_id: string; body_revision_id: string; evals_revision_id: string }
interface Detail {
  skill_id: string
  generation: number
  pointer: Pointer | null
  revisions: Revision[]
  activations: Activation[]
}

const str = (v: unknown): v is string => typeof v === 'string'

/** The daemon's own detail record, or null when it is not the shape promised. */
export function parseDetail(data: unknown, skillId: string): Detail | null {
  if (typeof data !== 'object' || data === null) return null
  const d = data as Record<string, unknown>
  if (d.skill_id !== skillId || !Number.isSafeInteger(d.generation) || !Array.isArray(d.revisions) || !Array.isArray(d.activations)) {
    return null
  }
  const revisions: Revision[] = []
  for (const item of d.revisions) {
    const v = item as Record<string, unknown> | null
    if (!v || !str(v.revision_id) || !str(v.origin) || !str(v.semver) || !str(v.kind) || !str(v.content_digest)) return null
    revisions.push({ revision_id: v.revision_id, origin: v.origin, semver: v.semver, kind: v.kind, content_digest: v.content_digest })
  }
  const activations: Activation[] = []
  for (const item of d.activations) {
    const v = item as Record<string, unknown> | null
    if (!v || !str(v.activation_id) || !str(v.body_revision_id) || !str(v.evals_revision_id)) return null
    activations.push({ activation_id: v.activation_id, body_revision_id: v.body_revision_id, evals_revision_id: v.evals_revision_id })
  }
  let pointer: Pointer | null = null
  if (d.pointer !== null) {
    const p = d.pointer as Record<string, unknown> | undefined
    if (!p || !str(p.origin) || !str(p.body_revision_id) || !str(p.evals_revision_id) || !Number.isSafeInteger(p.generation)) return null
    pointer = { origin: p.origin, body_revision_id: p.body_revision_id, evals_revision_id: p.evals_revision_id, generation: p.generation as number }
  }
  return { skill_id: skillId, generation: d.generation as number, pointer, revisions, activations }
}

/** Printable ASCII only and short: this text goes into a native dialog. */
const plain = (text: string, max = 64): string => {
  const clean = text.replace(/[^\x20-\x7e]/g, '?')
  return clean.length > max ? `${clean.slice(0, max)}...` : clean
}

export interface Confirmation { message: string; detail: string }

/**
 * Four items from the daemon's record: skill name, semver, the first 8 hex of
 * the body digest, and whether the evals side differs from the current
 * pointer (a rollback says which side comes back). A source switch adds
 * from/to. `null` when a revision or the history row is not in the record:
 * nothing is shown and nothing is sent.
 */
export function buildConfirmation(input: SkillActivateInput, detail: Detail): Confirmation | null {
  const find = (id: string, kind: string): Revision | undefined =>
    detail.revisions.find((r) => r.revision_id === id && r.kind === kind)
  const rollback = 'rollbackActivationId' in input
  let bodyId: string
  let evalsId: string
  if (rollback) {
    const row = detail.activations.find((a) => a.activation_id === input.rollbackActivationId)
    if (!row) return null
    bodyId = row.body_revision_id
    evalsId = row.evals_revision_id
  } else {
    bodyId = input.bodyRevisionId
    evalsId = input.evalsRevisionId
  }
  const body = find(bodyId, 'body')
  const evals = find(evalsId, 'evals')
  if (!body || !evals || body.origin !== evals.origin) return null
  const pointer = detail.pointer
  const bodyChanged = !pointer || pointer.body_revision_id !== body.revision_id
  const evalsChanged = !pointer || pointer.evals_revision_id !== evals.revision_id
  let state: string
  if (!pointer) state = mt('skill.evals.first')
  else if (rollback) {
    state = bodyChanged && evalsChanged ? mt('skill.evals.backBoth') : bodyChanged ? mt('skill.evals.backBody') : mt('skill.evals.backEvals')
  } else state = evalsChanged ? mt('skill.evals.changed') : mt('skill.evals.same')
  const lines = [
    mt('skill.version', { semver: plain(body.semver) }),
    mt('skill.digest', { digest: plain(body.content_digest.slice(0, 8), 8) }),
    mt('skill.evals', { state }),
  ]
  if (pointer && pointer.origin !== body.origin) {
    lines.push(mt('skill.switch', { from: plain(pointer.origin, 16), to: plain(body.origin, 16) }))
  }
  if (!rollback && input.accepted.length > 0) lines.push(mt('skill.accepted', { count: input.accepted.length }))
  lines.push('', mt('skill.detail'))
  return {
    message: mt(rollback ? 'skill.rollback' : 'skill.activate', { name: input.skillId }),
    detail: lines.join('\n'),
  }
}

/** `skills:activate` takes a pair, `skills:rollback` takes a history id. */
export type SkillActivateForm = 'activate' | 'rollback'

export async function handleSkillActivate(
  deps: SkillActivateDeps,
  form: SkillActivateForm,
  raw: unknown,
): Promise<BackendProxyResponse> {
  const input = parseSkillActivateInput(raw)
  if (!input || ('rollbackActivationId' in input) !== (form === 'rollback')) {
    return fail(400, 'invalid_request', 'invalid skill activation request')
  }
  const record = await deps.call({ method: 'GET', path: `/api/v1/skills/${input.skillId}` })
  const detail = record.ok ? parseDetail(record.data, input.skillId) : null
  const confirmation = detail ? buildConfirmation(input, detail) : null
  if (!detail || !confirmation) {
    // 404 stays 404; every other failure is one error shape.
    return fail(record.status === 404 ? 404 : 502, 'skill_unavailable', mt('err.skillUnavailable'))
  }
  if (!(await deps.confirm(confirmation.message, confirmation.detail))) {
    return fail(499, 'cancelled', mt('err.cancelled'))
  }
  // The generation is the daemon's own, read above: a page cannot supply one.
  const body =
    'rollbackActivationId' in input
      ? { expected_generation: detail.generation, rollback_activation_id: input.rollbackActivationId }
      : {
          expected_generation: detail.generation,
          body_revision_id: input.bodyRevisionId,
          evals_revision_id: input.evalsRevisionId,
          accepted: input.accepted,
        }
  return deps.call({ method: 'POST', path: `/api/v1/skills/${input.skillId}/activate`, body })
}
