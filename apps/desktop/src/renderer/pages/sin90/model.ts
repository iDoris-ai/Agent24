// Shapes from Sin90 v0.5.2 (src/core/types.rs, src/store/repo.rs TodayView).
// The desktop reads these fields and ignores the rest.

export interface Sin90Area {
  id: string
  title: string
  status: string
  sort_key: number
}

export interface Sin90Direction {
  id: string
  area_id: string | null
  title: string
  status: string
  target_window: string
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
  return { id, title, status, sort_key: typeof sort === 'number' ? sort : 0 }
}

export function parseDirection(v: unknown): Sin90Direction | null {
  if (!isObj(v)) return null
  const id = str(v['id'])
  const title = str(v['title'])
  const status = str(v['status'])
  const target = str(v['target_window'])
  if (!id || !title || !status || !target) return null
  return { id, area_id: optStr(v['area_id']), title, status, target_window: target }
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

/** Newest quota that is still in force. Retired quotas stay off the page. */
export function latestRhythm(rhythms: readonly Sin90Rhythm[]): Sin90Rhythm | null {
  const open = rhythms.filter((rhythm) => rhythm.status !== 'retired')
  open.sort((a, b) => (a.created_at < b.created_at ? 1 : a.created_at > b.created_at ? -1 : 0))
  return open[0] ?? null
}
