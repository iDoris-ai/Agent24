// M6 C2: the two entries that say the assistant has something for the user
// (docs/ui/ASSISTANT-C.md §7): the banner on top of the main chat, "The
// assistant has something to say (n)", and the "Needs you" count on the
// sidebar's assistant. Two numbers, counted apart (T-1):
// - the banner's n: the runs not seen yet of the conversations the user did
//   not start, archived ones left out (C1's `unseen`);
// - the sidebar's count: the conversations under the list's "Needs you"
//   filter (A3's `mine`), archived ones left out as the list does.
// Decisions: ~/.claude/decisions/2026-11-11-m6c2-banner-semantics/.

import { useEffect, useMemo, useState, useSyncExternalStore } from 'react'
import { unseenCount, type ChatHistorySummary } from '../../../shared/chat-history'
import { filtersOf } from './filters'
import { getHistoryState, markConversationSeen, subscribeHistory } from './history'
import { useRunStatuses, type RunStatus } from './run-status'

/** Past this many, the banner shows "99+"; its accessible name keeps the
 *  exact number. */
export const BANNER_CAP = 99

const live = (items: readonly ChatHistorySummary[]) => items.filter(item => item.archived_at === null)

/** The banner's n: the unseen runs of the conversations not archived. */
export function bannerCount(items: readonly ChatHistorySummary[]): number {
  return unseenCount(live(items))
}

/** How the banner writes n: as it is up to BANNER_CAP, "99+" past it. */
export function bannerNumber(n: number): string {
  return n <= BANNER_CAP ? String(n) : `${BANNER_CAP}+`
}

/** Where the banner goes: the conversation (not archived, not the user's)
 *  whose earliest unseen run came first, by that run's `started_at` in the
 *  conversation's tasks; on a tie the smaller id. Null when nothing is
 *  unseen. */
export function bannerTarget(items: readonly ChatHistorySummary[]): string | null {
  // An unseen run not found in `tasks` counts as the conversation's `updated_at`.
  let best: ChatHistorySummary | undefined
  let bestAt = Infinity
  for (const item of live(items)) {
    if (item.initiator === 'user' || item.unseen.length === 0) continue
    let at = Infinity
    for (const runId of item.unseen) {
      const started = item.tasks.find(task => task.run_id === runId)?.started_at ?? item.updated_at
      const parsed = Date.parse(started)
      if (parsed < at) at = parsed
    }
    if (best === undefined || at < bestAt || (at === bestAt && item.id < best.id)) {
      best = item
      bestAt = at
    }
  }
  return best?.id ?? null
}

/** The sidebar's count: the conversations not archived under "Needs you",
 *  as the list's own count of that filter (ChatHistoryList.tsx). */
export function mineCount(items: readonly ChatHistorySummary[], status: (runId: string) => RunStatus | undefined): number {
  return live(items).filter(item => filtersOf(item, status).has('mine')).length
}

/** The sidebar's count, kept up to date: the list's conversations, and the
 *  status of each of their tasks (asked of the daemon as the list does; a
 *  status not known yet, e.g. offline, counts as the list counts it). */
export function useMineCount(): number {
  const { items } = useSyncExternalStore(subscribeHistory, getHistoryState)
  const alive = useMemo(() => live(items), [items])
  const taskIds = useMemo(() => alive.flatMap(item => item.tasks.map(task => task.run_id)), [alive])
  const statuses = useRunStatuses(taskIds)
  return useMemo(() => mineCount(alive, id => statuses.get(id)), [alive, statuses])
}

/** M6 C2 Q4 (docs/ui/ASSISTANT-C.md §7): the user sees the open
 *  conversation while the main chat is the page `shown` and the window has
 *  the focus; then what of it is unseen is marked seen: when such a run
 *  comes, when the user comes back to the chat page, and when the window
 *  gets the focus again. A conversation opened from the list is marked
 *  seen by the chat page itself (C1); this covers the rest, among them a
 *  click on the banner for the conversation already open. */
export function useSeeingOpenConversation(shown: boolean): void {
  const { items, activeId } = useSyncExternalStore(subscribeHistory, getHistoryState)
  const [focused, setFocused] = useState(() => typeof document !== 'undefined' && document.hasFocus())
  useEffect(() => {
    const onFocus = () => setFocused(true)
    const onBlur = () => setFocused(false)
    window.addEventListener('focus', onFocus)
    window.addEventListener('blur', onBlur)
    return () => { window.removeEventListener('focus', onFocus); window.removeEventListener('blur', onBlur) }
  }, [])
  const unseen = items.find(item => item.id === activeId)?.unseen.length ?? 0
  useEffect(() => {
    if (shown && focused && activeId !== null && unseen > 0) void markConversationSeen(activeId)
  }, [shown, focused, activeId, unseen])
}
