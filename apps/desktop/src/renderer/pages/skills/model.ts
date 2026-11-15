// Skill bar rows (skill-loader plan §4.10). The page reads the list, the
// detail and GET /api/v1/components. A dependency is red when no ready
// component provides it, or more than one does. Provider choice is not
// stored here.

export type DepIssue = 'missing' | 'ambiguous'

export interface SkillDependency {
  id: string
  issue: DepIssue | null
}

export interface ActivationStep {
  /** `both` is one button; `body` and `evals` move one side while the pointer keeps the other. */
  side: 'both' | 'body' | 'evals'
  bodyRevisionId: string
  evalsRevisionId: string
}

export interface SkillBarRow {
  skillId: string
  origin: string
  semver: string
  active: boolean
  /** From the newest body of this origin, i.e. what would activate next. */
  requires: SkillDependency[]
  /** The newest body's requires list could not be read (the daemon sent none). */
  requiresUnreadable: boolean
  /** First 8 hex characters of the body content digest. Empty when unknown. */
  digest8: string
  consistency: { consistent: number; total: number } | null
  /** Whether this origin's evals revision differs from the current pointer. */
  evals: 'changed' | 'same' | 'none'
  bodyRevisionId: string | null
  evalsRevisionId: string | null
  /** What the Activate buttons submit. Body and evals both newer in the pointer's origin: one side per step. */
  activations: ActivationStep[]
  /** Newest history row that is not the pointer's pair, when there is a pointer. */
  rollbackActivationId: string | null
}

interface Revision {
  revision_id: string
  origin: string
  semver: string
  kind: string
  content_digest: string
  created_at: string
  requires?: string[]
}

interface Activation {
  activation_id: string
  seq?: number
  body_revision_id: string
  evals_revision_id: string
}

interface Detail {
  skill_id: string
  pointer: { origin: string; body_revision_id: string; evals_revision_id: string } | null
  revisions: Revision[]
  activations: Activation[]
  text_consistency: { consistent: number; total: number } | null
  requires?: string[]
}

export interface SkillListItem {
  skill_id: string
  origin: string
  semver: string
  active: boolean
}

export interface ComponentProvide {
  id: string
  availability?: string | null
}

export interface ComponentRow {
  id: string
  state: string
  provides: ComponentProvide[]
}

const str = (v: unknown): v is string => typeof v === 'string'
const obj = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v)

function stringList(v: unknown): string[] | undefined {
  if (!Array.isArray(v) || v.some((item) => typeof item !== 'string' || item.length === 0)) return undefined
  return v as string[]
}

export function parseSkillList(data: unknown): SkillListItem[] {
  const skills = obj(data) ? data.skills : undefined
  if (!Array.isArray(skills)) return []
  const out: SkillListItem[] = []
  for (const item of skills) {
    if (!obj(item) || !str(item.skill_id) || !str(item.origin) || !str(item.semver) || typeof item.active !== 'boolean') continue
    out.push({ skill_id: item.skill_id, origin: item.origin, semver: item.semver, active: item.active })
  }
  return out
}

function parseRevision(v: unknown): Revision | null {
  if (!obj(v) || !str(v.revision_id) || !str(v.origin) || !str(v.semver) || !str(v.kind) || !str(v.content_digest)) return null
  const revision: Revision = {
    revision_id: v.revision_id,
    origin: v.origin,
    semver: v.semver,
    kind: v.kind,
    content_digest: v.content_digest,
    created_at: str(v.created_at) ? v.created_at : '',
  }
  const requires = stringList(v.requires)
  if (requires) revision.requires = requires
  return revision
}

export function parseSkillDetail(data: unknown): Detail | null {
  if (!obj(data) || !str(data.skill_id) || !Array.isArray(data.revisions)) return null
  const revisions: Revision[] = []
  for (const item of data.revisions) {
    const revision = parseRevision(item)
    if (revision) revisions.push(revision)
  }
  const activations: Activation[] = []
  if (Array.isArray(data.activations)) {
    for (const item of data.activations) {
      if (!obj(item) || !str(item.activation_id) || !str(item.body_revision_id) || !str(item.evals_revision_id)) continue
      const row: Activation = {
        activation_id: item.activation_id,
        body_revision_id: item.body_revision_id,
        evals_revision_id: item.evals_revision_id,
      }
      if (typeof item.seq === 'number') row.seq = item.seq
      activations.push(row)
    }
  }
  let pointer: Detail['pointer'] = null
  if (obj(data.pointer) && str(data.pointer.origin) && str(data.pointer.body_revision_id) && str(data.pointer.evals_revision_id)) {
    pointer = {
      origin: data.pointer.origin,
      body_revision_id: data.pointer.body_revision_id,
      evals_revision_id: data.pointer.evals_revision_id,
    }
  }
  let text_consistency: Detail['text_consistency'] = null
  if (obj(data.text_consistency) && typeof data.text_consistency.consistent === 'number' && typeof data.text_consistency.total === 'number') {
    text_consistency = { consistent: data.text_consistency.consistent, total: data.text_consistency.total }
  }
  const detail: Detail = { skill_id: data.skill_id, pointer, revisions, activations, text_consistency }
  const requires = stringList(data.requires)
  if (requires) detail.requires = requires
  return detail
}

export function parseComponents(data: unknown): ComponentRow[] {
  const components = obj(data) ? data.components : undefined
  if (!Array.isArray(components)) return []
  const out: ComponentRow[] = []
  for (const item of components) {
    if (!obj(item) || !str(item.id) || !str(item.state) || !Array.isArray(item.provides)) continue
    const provides: ComponentProvide[] = []
    for (const provide of item.provides) {
      if (!obj(provide) || !str(provide.id)) continue
      provides.push({
        id: provide.id,
        availability: str(provide.availability) ? provide.availability : null,
      })
    }
    out.push({ id: item.id, state: item.state, provides })
  }
  return out
}

/** Ready components whose provide is currently available. One component once. */
export function providerCount(capability: string, components: readonly ComponentRow[]): number {
  return components.filter((component) =>
    component.state === 'ready'
    && component.provides.some((provide) => provide.id === capability && !provide.availability),
  ).length
}

export function dependencyIssue(capability: string, components: readonly ComponentRow[]): DepIssue | null {
  const count = providerCount(capability, components)
  if (count === 0) return 'missing'
  if (count > 1) return 'ambiguous'
  return null
}

function newest(revisions: readonly Revision[], origin: string, kind: string): Revision | null {
  const mine = revisions.filter((revision) => revision.origin === origin && revision.kind === kind)
  if (mine.length === 0) return null
  return mine.reduce((best, revision) => {
    const byTime = revision.created_at.localeCompare(best.created_at)
    if (byTime > 0 || (byTime === 0 && revision.revision_id > best.revision_id)) return revision
    return best
  })
}

export function skillBarRows(
  list: readonly SkillListItem[],
  details: ReadonlyMap<string, Detail>,
  components: readonly ComponentRow[],
): SkillBarRow[] {
  return list.map((item) => {
    const detail = details.get(item.skill_id) ?? null
    const revisions = detail?.revisions ?? []
    const body = newest(revisions, item.origin, 'body')
    const evals = newest(revisions, item.origin, 'evals')
    const requiresUnreadable = body === null || body.requires === undefined
    const requires = body?.requires ?? []
    const pointer = detail?.pointer ?? null
    let evalsState: SkillBarRow['evals'] = 'none'
    if (pointer && evals) evalsState = evals.revision_id === pointer.evals_revision_id ? 'same' : 'changed'
    const sameOrigin = pointer?.origin === item.origin
    let rollbackActivationId: string | null = null
    if (pointer && detail && sameOrigin) {
      const past = detail.activations.filter((row) =>
        row.body_revision_id !== pointer.body_revision_id || row.evals_revision_id !== pointer.evals_revision_id)
      const last = past.reduce<Activation | null>((best, row) => {
        if (!best) return row
        return (row.seq ?? -1) >= (best.seq ?? -1) ? row : best
      }, null)
      rollbackActivationId = last?.activation_id ?? null
    }
    const activations: ActivationStep[] = []
    if (body && evals) {
      const bodyNew = !pointer || pointer.body_revision_id !== body.revision_id
      const evalsNew = !pointer || pointer.evals_revision_id !== evals.revision_id
      if (pointer && sameOrigin && bodyNew && evalsNew) {
        // The store takes one side per activation inside an origin.
        activations.push({ side: 'body', bodyRevisionId: body.revision_id, evalsRevisionId: pointer.evals_revision_id })
        activations.push({ side: 'evals', bodyRevisionId: pointer.body_revision_id, evalsRevisionId: evals.revision_id })
      } else {
        activations.push({ side: 'both', bodyRevisionId: body.revision_id, evalsRevisionId: evals.revision_id })
      }
    }
    return {
      skillId: item.skill_id,
      origin: item.origin,
      semver: item.semver,
      active: item.active,
      requiresUnreadable,
      activations,
      requires: requires.map((id) => ({ id, issue: dependencyIssue(id, components) })),
      digest8: body ? body.content_digest.slice(0, 8) : '',
      consistency: sameOrigin ? (detail?.text_consistency ?? null) : null,
      evals: evalsState,
      bodyRevisionId: body?.revision_id ?? null,
      evalsRevisionId: evals?.revision_id ?? null,
      rollbackActivationId,
    }
  })
}

/** Skill ids the session may offer. An installed skill with no pointer is not one of them. */
export function selectableSkillIds(list: readonly SkillListItem[]): string[] {
  const ids: string[] = []
  for (const item of list) {
    if (item.active && !ids.includes(item.skill_id)) ids.push(item.skill_id)
  }
  return ids
}
