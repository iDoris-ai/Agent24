// UI-03: the main chat's history, between the sidebar list and the chat page.
// The list lives in the sidebar, the conversation in the always-mounted chat
// page; this module carries the list, which conversation is open, and "open
// that one" requests from the list to the page.

import type { ChatHistoryRecord, ChatHistorySummary } from '../../../shared/chat-history'

export interface HistoryState {
  items: ChatHistorySummary[]
  /** The conversation the chat page shows, once it has been saved. */
  activeId: string | null
}

let state: HistoryState = { items: [], activeId: null }
const listeners = new Set<() => void>()
let opener: ((id: string) => void) | null = null
let starter: (() => void) | null = null

const api = () => (typeof window === 'undefined' ? undefined : window.agent24?.chatHistory)

function set(patch: Partial<HistoryState>): void {
  state = { ...state, ...patch }
  for (const l of listeners) l()
}

export const getHistoryState = (): HistoryState => state
export function subscribeHistory(listener: () => void): () => void {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

/** Reads the list again from main. Failures leave the list as it was. */
export async function refreshHistory(): Promise<void> {
  const list = await api()?.list().catch(() => null)
  if (list) set({ items: list })
}

/** The chat page saves a conversation after each change and refreshes the list. */
export async function saveConversation(record: ChatHistoryRecord): Promise<void> {
  const history = api()
  if (!history) return
  try {
    await history.save(record)
    set({ activeId: record.id })
    await refreshHistory()
  } catch { /* history is best effort: the chat itself goes on */ }
}

export async function loadConversation(id: string): Promise<ChatHistoryRecord | null> {
  return (await api()?.get(id).catch(() => null)) ?? null
}

export function setActiveConversation(id: string | null): void {
  if (state.activeId !== id) set({ activeId: id })
}

/** The chat page registers how to open a conversation and start a new one. */
export function registerChatPage(open: (id: string) => void, start: () => void): () => void {
  opener = open
  starter = start
  return () => {
    if (opener === open) opener = null
    if (starter === start) starter = null
  }
}

export function openConversation(id: string): void {
  opener?.(id)
}

/** Deletes one conversation; deleting the open one starts a new chat. */
export async function deleteConversation(id: string): Promise<void> {
  await api()?.delete(id).catch(() => {})
  if (state.activeId === id) {
    set({ activeId: null })
    starter?.()
  }
  await refreshHistory()
}

/** Deletes every conversation and starts a new chat. */
export async function clearConversations(): Promise<void> {
  await api()?.clear().catch(() => {})
  set({ activeId: null })
  starter?.()
  await refreshHistory()
}

/** For tests: forget everything. */
export function resetHistoryStore(): void {
  state = { items: [], activeId: null }
  opener = null
  starter = null
}
