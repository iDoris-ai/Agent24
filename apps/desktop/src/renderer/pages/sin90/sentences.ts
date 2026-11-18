// Human sentences for proposal ops. Unknown ops stay as JSON.

import { translate, type Vars } from '../../i18n'
import { sin90Messages } from './Sin90.i18n'

type Key = keyof typeof sin90Messages.en

function t(key: Key, vars?: Vars): string {
  return translate(sin90Messages, key, vars)
}

function isObj(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}

function taskWord(status: string): string {
  switch (status) {
    case 'backlog': return t('task.backlog')
    case 'planned': return t('task.planned')
    case 'in_progress': return t('task.in_progress')
    case 'done': return t('task.done')
    case 'dropped': return t('task.dropped')
    case 'carried_over': return t('task.carried_over')
    default: return t('statusUnknown')
  }
}

export function sourceLabel(source: string): string {
  if (source === 'local_brain') return t('source.local')
  if (source === 'executive') return t('source.executive')
  if (source === 'rule') return t('source.rule')
  return t('source.local')
}

/** One card line. `carry_over_task` is the sentence in the plan, not a raw op name. */
export function describeOp(op: unknown, taskTitle: (id: string) => string, directionTitle: (id: string) => string): string {
  if (!isObj(op) || typeof op['op'] !== 'string') return t('op.unknown')
  switch (op['op']) {
    case 'carry_over_task':
      return t('op.carry')
    case 'create_direction':
      return t('op.direction', {
        title: typeof op['title'] === 'string' ? op['title'] : '',
        window: typeof op['target_window'] === 'string' && op['target_window'] !== 'n/a' ? op['target_window'] : t('window.na'),
      })
    case 'create_task':
      return t('op.task', { title: typeof op['title'] === 'string' ? op['title'] : '' })
    case 'create_tasks': {
      const titles = Array.isArray(op['tasks'])
        ? op['tasks'].flatMap((row) => (isObj(row) && typeof row['title'] === 'string' && row['title'] ? [row['title']] : [])).join('、')
        : ''
      return t('op.tasks', { titles })
    }
    case 'transition_task': {
      const id = typeof op['task_id'] === 'string' ? op['task_id'] : ''
      const to = typeof op['to'] === 'string' ? op['to'] : ''
      return t('op.transition', { title: taskTitle(id) || id, to: taskWord(to) })
    }
    case 'assign_task_direction':
      return t('op.assign')
    case 'adjust_rhythm': {
      const quota = Array.isArray(op['new_alloc'])
        ? op['new_alloc'].flatMap((row) => {
          if (!isObj(row) || typeof row['direction_id'] !== 'string' || typeof row['pct'] !== 'number') return []
          const name = directionTitle(row['direction_id']) || row['direction_id']
          return [`${name} ${row['pct']}%`]
        }).join('、')
        : ''
      return t('op.quota', { quota })
    }
    case 'reorder_tasks':
      return t('op.reorder')
    case 'draft_review_body':
      return t('op.review')
    case 'create_area':
      return t('op.area', { title: typeof op['title'] === 'string' ? op['title'] : '' })
    default:
      return t('op.unknown')
  }
}
