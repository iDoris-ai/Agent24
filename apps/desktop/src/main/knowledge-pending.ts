/**
 * M4 A7: what Knowledge waits on the user for, written into the
 * conversation it belongs to, so it shows under "waiting on me"
 * (docs/ui/ASSISTANT-A.md §6). Two callers: the 04b confirmation dialog
 * (knowledge-view.ts) and a 1b-2 entry draft (knowledge-entry-ipc.ts).
 * Only chat-history's `setPending` and `clearPending` are called.
 *
 * A write that fails (the conversation was deleted, the file cannot be
 * written) is logged and dropped: the dialog and the draft work the same
 * without their pending item.
 */
import type { createChatHistoryStore } from './chat-history'

type PendingStore = Pick<ReturnType<typeof createChatHistoryStore>, 'setPending' | 'clearPending'>

export interface KnowledgePending {
  /** `id` is the caller's own: the dialog's id or the draft's id, cleared by it. */
  set(conversationId: string, id: string, label: string): void
  clear(conversationId: string, id: string): void
}

export function knowledgePending(
  store: () => PendingStore,
  now: () => number = Date.now,
  log: (message: string) => void = (message) => console.warn(message),
): KnowledgePending {
  return {
    set(conversationId, id, label) {
      try {
        store().setPending(conversationId, { id, kind: 'confirm', label, at: new Date(now()).toISOString() })
      } catch (error) {
        log(`[knowledge-pending] set failed: ${error instanceof Error ? error.message : 'error'}`)
      }
    },
    clear(conversationId, id) {
      try {
        store().clearPending(conversationId, id)
      } catch (error) {
        log(`[knowledge-pending] clear failed: ${error instanceof Error ? error.message : 'error'}`)
      }
    },
  }
}
