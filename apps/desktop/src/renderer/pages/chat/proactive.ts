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
  // GROK: unseenCount over the items not archived.
  void items; void live; void unseenCount
  return 0 // stub
}

/** How the banner writes n: as it is up to BANNER_CAP, "99+" past it. */
export function bannerNumber(n: number): string {
  // GROK: String(n) when n <= BANNER_CAP, else `${BANNER_CAP}+`.
  void n
  return '' // stub
}

/** Where the banner goes: the conversation (not archived, not the user's)
 *  whose earliest unseen run came first, by that run's `started_at` in the
 *  conversation's tasks; on a tie the smaller id. Null when nothing is
 *  unseen. */
export function bannerTarget(items: readonly ChatHistorySummary[]): string | null {
  // GROK: implement as the comment says. An unseen run not found in `tasks`
  // (it should not happen: unseen runs are never answers) counts as the
  // conversation's own `updated_at`, so it is still reachable.
  void items
  return null // stub
}

/** The sidebar's count: the conversations not archived under "Needs you",
 *  as the list's own count of that filter (ChatHistoryList.tsx). */
export function mineCount(items: readonly ChatHistorySummary[], status: (runId: string) => RunStatus | undefined): number {
  // GROK: live(items) filtered by filtersOf(item, status).has('mine'), counted.
  void items; void status; void filtersOf
  return 0 // stub
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
  // GROK: listen to window 'focus' / 'blur' (set `focused`), removed on
  // unmount, as CliToolSettings.tsx does.
  useEffect(() => { void setFocused }, [])
  const unseen = items.find(item => item.id === activeId)?.unseen.length ?? 0
  useEffect(() => {
    // GROK: when shown && focused && activeId !== null && unseen > 0, call
    // `void markConversationSeen(activeId)`.
    void markConversationSeen
  }, [shown, focused, activeId, unseen])
}
