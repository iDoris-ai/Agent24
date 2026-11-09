// M4 A3: the history list's four filters (design §3.2, David 11-09: four
// are enough). "Conversation" is no state: every conversation is under
// "all"; one can be both in progress and waiting on the user.

import type { ChatHistorySummary } from '../../../shared/chat-history'
import type { RunStatus } from './run-status'

export type HistoryFilter = 'all' | 'active' | 'mine' | 'done'
export const HISTORY_FILTERS: readonly HistoryFilter[] = ['all', 'active', 'mine', 'done']

const ACTIVE = new Set(['queued', 'running'])
const ENDED = new Set(['completed', 'failed', 'cancelled'])

/** Which filters a conversation is under, from its tasks (never its
 *  answers: the summary carries none) and what waits on the user. A task
 *  whose status is not known yet counts nowhere but "all".
 *  - active: a task queued or running;
 *  - mine: something waits on the user, or a task waits on an approval;
 *  - done: nothing waits on the user, and its latest task has ended. */
export function filtersOf(item: Pick<ChatHistorySummary, 'tasks' | 'pending'>, status: (runId: string) => RunStatus | undefined): Set<HistoryFilter> {
  const under = new Set<HistoryFilter>(['all'])
  const seen = item.tasks.map(task => status(task.run_id))
  if (seen.some(s => s !== undefined && ACTIVE.has(s))) under.add('active')
  if (item.pending.length > 0 || seen.includes('awaiting_approval')) under.add('mine')
  // The latest task by when the conversation first knew of it; on a tie,
  // the one recorded last.
  let latest = -1
  item.tasks.forEach((task, i) => {
    if (latest < 0 || Date.parse(task.started_at) >= Date.parse(item.tasks[latest].started_at)) latest = i
  })
  const last = latest >= 0 ? seen[latest] : undefined
  if (item.pending.length === 0 && last !== undefined && ENDED.has(last)) under.add('done')
  return under
}
