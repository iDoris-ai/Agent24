import { describe, expect, it } from 'vitest'
import { filtersOf } from './filters'
import type { ChatHistoryPending, ChatHistoryRun } from '../../../shared/chat-history'

// M4 A3: which filters a conversation is under.
const run = (run_id: string, started_at = '2026-11-09T01:00:00.000Z', kind: ChatHistoryRun['kind'] = 'task'): ChatHistoryRun => ({ run_id, kind, started_at })
const ask: ChatHistoryPending = { id: 'ask_1', kind: 'confirm', label: '加进对话？', at: '2026-11-09T01:00:00.000Z' }
const of = (tasks: ChatHistoryRun[], status: Record<string, string>, pending: ChatHistoryPending[] = []) =>
  [...filtersOf({ tasks, pending }, id => status[id])].sort()

describe('filtersOf', () => {
  it('a conversation without tasks or anything waiting is under "all" only', () => {
    expect(of([], {})).toEqual(['all'])
  })

  it.each(['queued', 'running'])('a task %s is in progress', status => {
    expect(of([run('r1')], { r1: status })).toEqual(['active', 'all'])
  })

  it('a task waiting on its approval needs the user, and is not in progress', () => {
    expect(of([run('r1')], { r1: 'awaiting_approval' })).toEqual(['all', 'mine'])
  })

  it('anything waiting needs the user, and keeps it from done', () => {
    expect(of([run('r1')], { r1: 'completed' }, [ask])).toEqual(['all', 'mine'])
    expect(of([], {}, [ask])).toEqual(['all', 'mine'])
  })

  it.each(['completed', 'failed', 'cancelled'])('is done once its latest task is %s', status => {
    expect(of([run('r1')], { r1: status })).toEqual(['all', 'done'])
  })

  it('done goes by the latest task; on a tie, the one recorded last', () => {
    const early = run('r1', '2026-11-09T01:00:00.000Z')
    const late = run('r2', '2026-11-09T02:00:00.000Z')
    expect(of([late, early], { r1: 'running', r2: 'completed' })).toEqual(['active', 'all', 'done'])
    expect(of([early, late], { r1: 'completed', r2: 'running' })).toEqual(['active', 'all'])
    expect(of([run('r1'), run('r2')], { r1: 'running', r2: 'completed' })).toEqual(['active', 'all', 'done'])
    expect(of([run('r1'), run('r2')], { r1: 'completed', r2: 'running' })).toEqual(['active', 'all'])
  })

  it('a task whose status is not known, or that the daemon does not have, counts nowhere but "all"', () => {
    expect(of([run('r1')], {})).toEqual(['all'])
    expect(of([run('r1')], { r1: 'missing' })).toEqual(['all'])
  })

  it('a conversation can be in progress and need the user at once', () => {
    expect(of([run('r1'), run('r2')], { r1: 'running', r2: 'awaiting_approval' })).toEqual(['active', 'all', 'mine'])
  })
})
