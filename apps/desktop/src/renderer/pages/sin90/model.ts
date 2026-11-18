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
  if (!isObj(v) || !str(v['id']) || !str(v['title']) || !str(v['status'])) return null
  const sort = v['sort_key']
  return { id: v['id'], title: v['title'], status: v['status'], sort_key: typeof sort === 'number' ? sort : 0 }
}

export function parseDirection(v: unknown): Sin90Direction | null {
  if (!isObj(v) || !str(v['id']) || !str(v['title']) || !str(v['status']) || !str(v['target_window'])) return null
  return {
    id: v['id'],
    area_id: optStr(v['area_id']),
    title: v['title'],
    status: v['status'],
    target_window: v['target_window'],
  }
}

export function parseTask(v: unknown): Sin90Task | null {
  if (!isObj(v) || !str(v['id']) || !str(v['title']) || !str(v['status'])) return null
  return {
    id: v['id'],
    direction_id: optStr(v['direction_id']),
    week_id: optStr(v['week_id']),
    title: v['title'],
    status: v['status'],
  }
}

export function parseBlock(v: unknown): Sin90Block | null {
  if (!isObj(v) || !str(v['id']) || !str(v['status']) || typeof v['planned_minutes'] !== 'number') return null
  return {
    id: v['id'],
    direction_id: optStr(v['direction_id']),
    task_id: optStr(v['task_id']),
    status: v['status'],
    planned_minutes: v['planned_minutes'],
  }
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
