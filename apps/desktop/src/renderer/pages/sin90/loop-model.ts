// Additive read models from SIN90-LOOP-CONTRACT.md.
export const quadrants = ['do_first', 'schedule', 'delegate', 'eliminate'] as const
export type Quadrant = typeof quadrants[number]
export function isQuadrant(value: unknown): value is Quadrant {
  return quadrants.some((quadrant) => quadrant === value)
}
export interface Assessment {
  quadrant: Quadrant
  importance: 'high' | 'low'
  urgency: 'high' | 'low'
  rationale: string
  evaluated_at: string
}
export type PriorityDecision = Pick<Assessment, 'quadrant' | 'rationale'>
export interface Priority {
  task_id: string
  evaluation_status: 'unevaluated' | 'evaluated'
  ai: Assessment | null
  human_override: PriorityDecision | null
  effective: PriorityDecision | null
}
export interface CompletionEvidence {
  task_id: string
  title: string
  completed_at: string
  quadrant: Quadrant | null
  note: string | null
}
function assessment(value: unknown): Assessment | null {
  if (typeof value !== 'object' || value === null || !('quadrant' in value) || !isQuadrant(value.quadrant)
    || !('importance' in value) || (value.importance !== 'high' && value.importance !== 'low')
    || !('urgency' in value) || (value.urgency !== 'high' && value.urgency !== 'low')
    || !('rationale' in value) || typeof value.rationale !== 'string'
    || !('evaluated_at' in value) || typeof value.evaluated_at !== 'string') return null
  return { quadrant: value.quadrant, importance: value.importance, urgency: value.urgency, rationale: value.rationale, evaluated_at: value.evaluated_at }
}
function decision(value: unknown): PriorityDecision | null {
  if (typeof value !== 'object' || value === null || !('quadrant' in value) || !isQuadrant(value.quadrant)
    || !('rationale' in value) || typeof value.rationale !== 'string') return null
  return { quadrant: value.quadrant, rationale: value.rationale }
}
export function readPriority(value: unknown): Priority {
  if (typeof value !== 'object' || value === null || !('task_id' in value) || typeof value.task_id !== 'string'
    || !('evaluation_status' in value) || (value.evaluation_status !== 'unevaluated' && value.evaluation_status !== 'evaluated')) throw new Error('Invalid priority response')
  const ai = 'ai' in value ? assessment(value.ai) : null
  const human_override = 'human_override' in value ? decision(value.human_override) : null
  return { task_id: value.task_id, evaluation_status: value.evaluation_status, ai, human_override, effective: human_override ?? ai }
}
export function readCompletedTasks(value: unknown): CompletionEvidence[] | null {
  if (typeof value !== 'object' || value === null || !('completed_tasks' in value) || !Array.isArray(value.completed_tasks)) return null
  return value.completed_tasks.flatMap((row: unknown) => {
    if (typeof row !== 'object' || row === null || !('task_id' in row) || typeof row.task_id !== 'string'
      || !('title' in row) || typeof row.title !== 'string' || !('completed_at' in row) || typeof row.completed_at !== 'string') return []
    return [{ task_id: row.task_id, title: row.title, completed_at: row.completed_at,
      quadrant: 'quadrant' in row && isQuadrant(row.quadrant) ? row.quadrant : null,
      note: 'note' in row && typeof row.note === 'string' ? row.note : null }]
  })
}
