// Shapes from Sin90 v0.5.2 (src/core/types.rs, src/store/repo.rs TodayView).
// The desktop reads these fields and ignores the rest.

export interface Sin90Area {
  id: string
  title: string
  status: string
  sort_key: number
  current_state?: string | null
  ideal_state?: string | null
}

export interface Sin90Direction {
  id: string
  area_id: string | null
  title: string
  status: string
  target_window: string
  goal_3y?: string | null
  goal_from_year?: number | null
}

export interface Sin90PeriodPlan {
  id: string; direction_id: string; kind: 'year' | 'half' | 'quarter' | 'month'; period: string
  parent_id: string | null; target: string; metric: 'none' | 'count' | 'minutes'; metric_target: number | null
  routine_id: string | null; status: string
}
export interface Sin90PlanProgress { plan_id: string; actual: number | null; expected: number | null; status: 'on_track' | 'behind' | 'at_risk'; miss_streak: number }
export interface Sin90ReviewSettings { auto: boolean; cadence: 'weekly' | 'monthly'; weekday: number; hour: number }

export function legalPlanParents(kind: Sin90PeriodPlan['kind'], period: string, plans: readonly Sin90PeriodPlan[], directionId: string): Sin90PeriodPlan[] {
  const allowed: Record<Sin90PeriodPlan['kind'], readonly Sin90PeriodPlan['kind'][]> = {
    year: [], half: ['year'], quarter: ['half', 'year'], month: ['quarter'],
  }
  return plans.filter((parent) => {
    if (parent.direction_id !== directionId || !allowed[kind].includes(parent.kind) || parent.status === 'dropped') return false
    if (parent.period.slice(0, 4) !== period.slice(0, 4)) return false
    if (parent.kind === 'half' && kind === 'quarter') return Math.floor((Number(period.slice(6)) - 1) / 2) === Number(parent.period.slice(6)) - 1
    if (parent.kind === 'quarter' && kind === 'month') return Math.floor((Number(period.slice(5)) - 1) / 3) + 1 === Number(parent.period.slice(6))
    return true
  })
}

export interface Sin90Task {
  id: string
  direction_id: string | null
  week_id: string | null
  title: string
  status: string
}

export interface Sin90Block {
  id: string
  direction_id: string | null
  task_id: string | null
  status: string
  planned_minutes: number
}

export interface Sin90Today {
  must_do: Sin90Task[]
  deep_block: Sin90Block | null
  inbox: Sin90Task[]
  carry_over_candidates: Sin90Task[]
}

function isObj(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}

function str(v: unknown): string | null {
  return typeof v === 'string' ? v : null
}

function optStr(v: unknown): string | null {
  return typeof v === 'string' ? v : null
}

export function parseArea(v: unknown): Sin90Area | null {
  if (!isObj(v)) return null
  const id = str(v['id'])
  const title = str(v['title'])
  const status = str(v['status'])
  if (!id || !title || !status) return null
  const sort = v['sort_key']
  return { id, title, status, sort_key: typeof sort === 'number' ? sort : 0,
    current_state: optStr(v['current_state']), ideal_state: optStr(v['ideal_state']) }
}

export function parseDirection(v: unknown): Sin90Direction | null {
  if (!isObj(v)) return null
  const id = str(v['id'])
  const title = str(v['title'])
  const status = str(v['status'])
  const target = str(v['target_window'])
  if (!id || !title || !status || !target) return null
  return { id, area_id: optStr(v['area_id']), title, status, target_window: target,
    goal_3y: optStr(v['goal_3y']), goal_from_year: typeof v['goal_from_year'] === 'number' ? v['goal_from_year'] : null }
}

export function readPeriodPlans(data: unknown): Sin90PeriodPlan[] {
  if (!isObj(data) || !Array.isArray(data['period_plans'])) return []
  return data['period_plans'].filter((row): row is Record<string, unknown> => isObj(row)
    && typeof row['id'] === 'string' && typeof row['direction_id'] === 'string'
    && (row['kind'] === 'year' || row['kind'] === 'half' || row['kind'] === 'quarter' || row['kind'] === 'month') && typeof row['period'] === 'string'
    && typeof row['target'] === 'string' && typeof row['metric'] === 'string'
    && typeof row['metric'] === 'string' && ['none','count','minutes'].includes(row['metric'])
    && typeof row['status'] === 'string').map((row) => ({
      id: String(row['id']), direction_id: String(row['direction_id']), kind: PLAN_KINDS.find((kind) => kind === row['kind']) ?? 'year', period: String(row['period']),
      parent_id: optStr(row['parent_id']), target: String(row['target']), metric: row['metric'] === 'count' || row['metric'] === 'minutes' ? row['metric'] : 'none',
      metric_target: typeof row['metric_target'] === 'number' ? row['metric_target'] : null,
      routine_id: optStr(row['routine_id']), status: String(row['status']),
    }))
}
const PLAN_KINDS = ['year', 'half', 'quarter', 'month'] as const

export function readPlanProgress(data: unknown): Sin90PlanProgress[] {
  if (!isObj(data) || !Array.isArray(data['progress'])) return []
  return data['progress'].filter((row): row is Record<string, unknown> => isObj(row) && typeof row['plan_id'] === 'string')
    .map((row) => ({ plan_id: String(row['plan_id']), actual: typeof row['actual'] === 'number' ? row['actual'] : null,
      expected: typeof row['expected'] === 'number' ? row['expected'] : null,
      status: row['status'] === 'behind' || row['status'] === 'at_risk' ? row['status'] : 'on_track',
      miss_streak: typeof row['miss_streak'] === 'number' ? row['miss_streak'] : 0 }))
}

export function parseTask(v: unknown): Sin90Task | null {
  if (!isObj(v)) return null
  const id = str(v['id'])
  const title = str(v['title'])
  const status = str(v['status'])
  if (!id || !title || !status) return null
  return { id, direction_id: optStr(v['direction_id']), week_id: optStr(v['week_id']), title, status }
}

export function parseBlock(v: unknown): Sin90Block | null {
  if (!isObj(v)) return null
  const id = str(v['id'])
  const status = str(v['status'])
  if (!id || !status || typeof v['planned_minutes'] !== 'number') return null
  return { id, direction_id: optStr(v['direction_id']), task_id: optStr(v['task_id']), status, planned_minutes: v['planned_minutes'] }
}

function list<T>(data: unknown, key: string, parse: (v: unknown) => T | null): T[] {
  if (!isObj(data) || !Array.isArray(data[key])) throw new Error(`sin90: missing ${key}`)
  return data[key].map(parse).filter((row): row is T => row !== null)
}

export function readAreas(data: unknown): Sin90Area[] {
  return list(data, 'areas', parseArea)
}

export function readDirections(data: unknown): Sin90Direction[] {
  return list(data, 'directions', parseDirection)
}

export function readTasks(data: unknown): Sin90Task[] {
  return list(data, 'tasks', parseTask)
}

export function readToday(data: unknown): Sin90Today {
  if (!isObj(data)) throw new Error('sin90: missing today')
  const deep = data['deep_block']
  return {
    must_do: Array.isArray(data['must_do']) ? data['must_do'].map(parseTask).filter((t): t is Sin90Task => t !== null) : [],
    deep_block: deep === null || deep === undefined ? null : parseBlock(deep),
    inbox: Array.isArray(data['inbox']) ? data['inbox'].map(parseTask).filter((t): t is Sin90Task => t !== null) : [],
    carry_over_candidates: Array.isArray(data['carry_over_candidates'])
      ? data['carry_over_candidates'].map(parseTask).filter((t): t is Sin90Task => t !== null)
      : [],
  }
}

export interface DirectionGroup {
  areaId: string | null
  title: string | null
  directions: Sin90Direction[]
}

/** Areas are group headings. Directions with no area sit under “uncategorized”. */
export function groupDirections(areas: readonly Sin90Area[], directions: readonly Sin90Direction[]): DirectionGroup[] {
  const byArea = new Map<string, Sin90Direction[]>()
  const loose: Sin90Direction[] = []
  for (const direction of directions) {
    if (!direction.area_id) loose.push(direction)
    else {
      const rows = byArea.get(direction.area_id) ?? []
      rows.push(direction)
      byArea.set(direction.area_id, rows)
    }
  }
  const groups: DirectionGroup[] = []
  if (loose.length > 0) groups.push({ areaId: null, title: null, directions: loose })
  const ordered = [...areas].filter((area) => area.status !== 'archived').sort((a, b) => a.sort_key - b.sort_key)
  for (const area of ordered) {
    const rows = byArea.get(area.id)
    if (rows && rows.length > 0) groups.push({ areaId: area.id, title: area.title, directions: rows })
  }
  return groups
}

export function firstDirectionId(groups: readonly DirectionGroup[]): string | null {
  return groups[0]?.directions[0]?.id ?? null
}

/** Direct task edges the page may PATCH. Carrying over is a proposal, not an edge here. */
export function taskTransitions(status: string): Array<'planned' | 'in_progress' | 'done' | 'dropped'> {
  switch (status) {
    case 'backlog': return ['planned', 'dropped']
    case 'planned': return ['in_progress', 'dropped']
    case 'in_progress': return ['done', 'dropped']
    default: return []
  }
}

export function canCarry(status: string): boolean {
  return status === 'backlog' || status === 'planned' || status === 'in_progress'
}

/** ISO week label in UTC, `YYYY-Www`. */
export function isoWeekUTC(date: Date): string {
  const utc = new Date(Date.UTC(date.getUTCFullYear(), date.getUTCMonth(), date.getUTCDate()))
  const day = utc.getUTCDay() || 7
  utc.setUTCDate(utc.getUTCDate() + 4 - day)
  const yearStart = new Date(Date.UTC(utc.getUTCFullYear(), 0, 1))
  const week = Math.ceil((((utc.getTime() - yearStart.getTime()) / 86400000) + 1) / 7)
  return `${utc.getUTCFullYear()}-W${String(week).padStart(2, '0')}`
}

export function nextIsoWeek(date: Date): string {
  return isoWeekUTC(new Date(date.getTime() + 7 * 86400000))
}

export interface Sin90Week {
  id: string
  status: string
  iso_week: string
}

export interface Sin90Routine {
  id: string
  direction_id: string | null
  title: string
  status: string
}

export interface Sin90Allocation {
  direction_id: string
  pct: number
}

export interface Sin90Rhythm {
  id: string
  status: string
  created_at: string
  allocations: Sin90Allocation[]
}

export interface Sin90WeekAttention {
  planned_min: number
  actual_min: number
  deviation_min: number
}

export function parseWeek(v: unknown): Sin90Week | null {
  if (!isObj(v)) return null
  const id = str(v['id'])
  const status = str(v['status'])
  const iso = str(v['iso_week'])
  if (!id || !status || !iso) return null
  return { id, status, iso_week: iso }
}

export function parseRoutine(v: unknown): Sin90Routine | null {
  if (!isObj(v)) return null
  const id = str(v['id'])
  const title = str(v['title'])
  const status = str(v['status'])
  if (!id || !title || !status) return null
  return { id, direction_id: optStr(v['direction_id']), title, status }
}

export function parseRhythm(v: unknown): Sin90Rhythm | null {
  if (!isObj(v) || !Array.isArray(v['allocations'])) return null
  const id = str(v['id'])
  const status = str(v['status'])
  if (!id || !status) return null
  const allocations: Sin90Allocation[] = []
  for (const row of v['allocations']) {
    if (!isObj(row) || typeof row['pct'] !== 'number') continue
    const directionId = str(row['direction_id'])
    if (!directionId) continue
    allocations.push({ direction_id: directionId, pct: row['pct'] })
  }
  return { id, status, created_at: str(v['created_at']) ?? '', allocations }
}

export function readWeeks(data: unknown): Sin90Week[] {
  return list(data, 'weeks', parseWeek)
}

export function readBlocks(data: unknown): Sin90Block[] {
  return list(data, 'blocks', parseBlock)
}

export function readRoutines(data: unknown): Sin90Routine[] {
  return list(data, 'routines', parseRoutine)
}

export function readRhythms(data: unknown): Sin90Rhythm[] {
  return list(data, 'rhythms', parseRhythm)
}

export function readWeekAttention(data: unknown): Sin90WeekAttention {
  if (!isObj(data) || typeof data['planned_min'] !== 'number' || typeof data['actual_min'] !== 'number' || typeof data['deviation_min'] !== 'number') {
    throw new Error('sin90: missing attention')
  }
  return { planned_min: data['planned_min'], actual_min: data['actual_min'], deviation_min: data['deviation_min'] }
}

/** The week whose label is this ISO week. Blocks have no date, so the week is the only time container. */
export function currentWeek(weeks: readonly Sin90Week[], iso: string): Sin90Week | null {
  return weeks.find((week) => week.iso_week === iso) ?? null
}

export function weekNext(status: string): 'active' | 'reviewing' | 'closed' | null {
  if (status === 'planning') return 'active'
  if (status === 'active') return 'reviewing'
  if (status === 'reviewing') return 'closed'
  return null
}

export function blockTransitions(status: string): Array<'started' | 'completed' | 'skipped'> {
  if (status === 'planned') return ['started', 'skipped']
  if (status === 'started') return ['completed', 'skipped']
  return []
}

export function routineNext(status: string): 'active' | 'paused' | null {
  if (status === 'active') return 'paused'
  if (status === 'paused') return 'active'
  return null
}

export function blocksForDirection(blocks: readonly Sin90Block[], directionId: string, tasks: readonly Sin90Task[]): Sin90Block[] {
  return blocks.filter((block) => {
    if (block.direction_id) return block.direction_id === directionId
    if (!block.task_id) return false
    return tasks.some((task) => task.id === block.task_id && task.direction_id === directionId)
  })
}

export function routinesForDirection(routines: readonly Sin90Routine[], directionId: string): Sin90Routine[] {
  return routines.filter((routine) => routine.direction_id === directionId && routine.status !== 'retired')
}

export interface Sin90Proposal {
  id: string
  status: string
  source: string
  ops: unknown[]
  rationale: string | null
}

export interface Sin90Review {
  id: string
  kind: string
  status: string
  period: string
}

export function parseProposal(v: unknown): Sin90Proposal | null {
  if (!isObj(v) || !Array.isArray(v['ops'])) return null
  const id = str(v['id'])
  const status = str(v['status'])
  const source = str(v['source'])
  if (!id || !status || !source) return null
  return { id, status, source, ops: v['ops'], rationale: str(v['rationale']) }
}

export function parseReview(v: unknown): Sin90Review | null {
  if (!isObj(v)) return null
  const id = str(v['id'])
  const kind = str(v['kind'])
  const status = str(v['status'])
  const period = str(v['period'])
  if (!id || !kind || !status || !period) return null
  return { id, kind, status, period }
}

export function readProposals(data: unknown): Sin90Proposal[] {
  return list(data, 'proposals', parseProposal)
}

export function readReviews(data: unknown): Sin90Review[] {
  return list(data, 'reviews', parseReview)
}

/** A weekly review that is still a draft. Prefer the one for this ISO week. */
export function weeklyDraftReviewId(reviews: readonly Sin90Review[], iso: string): string | null {
  const drafts = reviews.filter((review) => review.kind === 'weekly' && review.status === 'draft')
  return (drafts.find((review) => review.period === iso) ?? drafts[0])?.id ?? null
}

/** Newest quota that is still in force. Retired quotas stay off the page. */
export function latestRhythm(rhythms: readonly Sin90Rhythm[]): Sin90Rhythm | null {
  const open = rhythms.filter((rhythm) => rhythm.status !== 'retired')
  open.sort((a, b) => (a.created_at < b.created_at ? 1 : a.created_at > b.created_at ? -1 : 0))
  return open[0] ?? null
}

export interface Sin90TemplateInfo {
  id: string
  lang: string
  loaded_at: string
  title: string
  guide: string
}

export interface Sin90BackupItem {
  name: string
  at: string
  bytes: number
}

export interface Sin90DataCounts {
  areas: number
  directions: number
  tasks: number
  plans: number
  routines: number
}

export interface Sin90DataStatus {
  template: Sin90TemplateInfo | null
  backups: Sin90BackupItem[]
  counts: Sin90DataCounts
}

export interface Sin90TemplateChoice {
  id: string
  langs: string[]
  title: string
}

export function parseDataStatus(v: unknown): Sin90DataStatus {
  if (!isObj(v)) {
    return {
      template: null,
      backups: [],
      counts: { areas: 0, directions: 0, tasks: 0, plans: 0, routines: 0 },
    }
  }
  const tmplRaw = v['template']
  const template: Sin90TemplateInfo | null = isObj(tmplRaw)
    ? {
        id: str(tmplRaw['id']) ?? '',
        lang: str(tmplRaw['lang']) ?? '',
        loaded_at: str(tmplRaw['loaded_at']) ?? '',
        title: str(tmplRaw['title']) ?? '',
        guide: str(tmplRaw['guide']) ?? '',
      }
    : null
  const backupsRaw = Array.isArray(v['backups']) ? v['backups'] : []
  const backups: Sin90BackupItem[] = backupsRaw.filter(isObj).map((b) => ({
    name: str(b['name']) ?? '',
    at: str(b['at']) ?? '',
    bytes: typeof b['bytes'] === 'number' ? b['bytes'] : 0,
  }))
  const c = isObj(v['counts']) ? v['counts'] : {}
  const counts: Sin90DataCounts = {
    areas: typeof c['areas'] === 'number' ? c['areas'] : 0,
    directions: typeof c['directions'] === 'number' ? c['directions'] : 0,
    tasks: typeof c['tasks'] === 'number' ? c['tasks'] : 0,
    plans: typeof c['plans'] === 'number' ? c['plans'] : 0,
    routines: typeof c['routines'] === 'number' ? c['routines'] : 0,
  }
  return { template, backups, counts }
}

export function parseTemplates(v: unknown): Sin90TemplateChoice[] {
  if (!Array.isArray(v)) return []
  return v.filter(isObj).map((item) => ({
    id: str(item['id']) ?? '',
    langs: Array.isArray(item['langs']) ? item['langs'].filter((l): l is string => typeof l === 'string') : [],
    title: str(item['title']) ?? '',
  }))
}
