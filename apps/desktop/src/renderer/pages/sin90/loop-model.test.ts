import { describe, expect, it } from 'vitest'
import { readCompletedTasks, readPriority } from './loop-model'

describe('Sin90 additive loop read models', () => {
  it('accepts the backend’s minimal human override and gives it precedence over AI', () => {
    const human = { quadrant: 'schedule', rationale: '先访谈' }
    const priority = readPriority({ task_id: 't1', evaluation_status: 'evaluated', human_override: human,
      ai: { quadrant: 'do_first', importance: 'high', urgency: 'high', rationale: '今天交付', evaluated_at: '2026-11-24T00:00:00Z' }, effective: human })
    expect(priority.effective).toEqual(human)
    expect(priority.ai?.quadrant).toBe('do_first')
  })
  it('does not invent a quadrant from an absent or invalid assessment', () => {
    expect(readPriority({ task_id: 't1', evaluation_status: 'unevaluated', ai: null, human_override: null, effective: null }).effective).toBeNull()
    expect(readPriority({ task_id: 't1', evaluation_status: 'evaluated', ai: { quadrant: 'urgent' } }).effective).toBeNull()
  })
  it('distinguishes an old draft from a supported week with no completion events', () => {
    expect(readCompletedTasks({ tasks_done: 3 })).toBeNull()
    expect(readCompletedTasks({ completed_tasks: [] })).toEqual([])
  })
  it('retains missing notes and the classification recorded at completion', () => {
    const evidence = { task_id: 't1', title: '交付方案', completed_at: '2026-11-24T00:00:00Z', quadrant: 'schedule', note: null }
    expect(readCompletedTasks({ completed_tasks: [evidence] })).toEqual([evidence])
  })
})
