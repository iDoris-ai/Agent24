// UI-06 G2: where a conversation dragged in the history list may land, and
// what landing there changes. Shared by the drag and drop and the menu.

import type { ChatGroup, ChatHistoryOrganize, ChatHistorySummary } from '../../../shared/chat-history'

/** A group's heading, the ungrouped list ("Recent"), or the archive. */
export type DropTarget = { kind: 'group'; id: string } | { kind: 'recent' } | { kind: 'archive' }

/** The group a conversation is listed under: none when its group was deleted. */
export const groupOf = (item: ChatHistorySummary, groups: readonly ChatGroup[]): string | null =>
  item.group_id !== null && groups.some(g => g.id === item.group_id) ? item.group_id : null

/** What dropping the conversation on the target changes, or null when it
 *  would change nothing (already there) or the target is gone: such a drop is
 *  not offered. */
export function dropChange(target: DropTarget, item: ChatHistorySummary, groups: readonly ChatGroup[]): ChatHistoryOrganize | null {
  if (target.kind === 'archive') return item.archived_at === null ? { archived: true } : null
  // Archived conversations are brought back from the archive, not dragged
  // between groups.
  if (item.archived_at !== null) return null
  const current = groupOf(item, groups)
  if (target.kind === 'recent') return current === null ? null : { group_id: null }
  if (!groups.some(g => g.id === target.id) || current === target.id) return null
  return { group_id: target.id }
}

/** The key a target is highlighted under while something is dragged over it. */
export const targetKey = (target: DropTarget): string => (target.kind === 'group' ? `group:${target.id}` : target.kind)

/** What a drag carries, besides the page's own record of it: other
 *  applications see a conversation id, nothing they could act on. */
export const DRAG_TYPE = 'application/x-agent24-conversation'
