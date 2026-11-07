// UI-03: the main chat's history, between the sidebar list and the chat page.
// The list lives in the sidebar, the conversation in the always-mounted chat
// page; this module carries the list, which conversation is open, and "open
// that one" requests from the list to the page.

import type { ChatHistoryRecord, ChatHistorySummary } from '../../../shared/chat-history'

export interface HistoryState {
  items: ChatHistorySummary[]
  /** The conversation the chat page shows, once it has been saved. Only the
   *  chat page sets it. */
  activeId: string | null
  /** A reply is in flight: the page cannot switch conversations, so the list
   *  does not offer to, nor to delete the open one or clear everything. */
  busy: boolean
}

const initial: HistoryState = { items: [], activeId: null, busy: false }
let state: HistoryState = initial
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

/** Waits before each retry of a failed save. */
export const SAVE_RETRY_MS = [500, 1500]
/** Orders saves, deletions and clears. A retry still waiting gives up when
 *  a newer save of the same conversation was asked for (that one carries
 *  everything this one did, and must not be overwritten by it), or when
 *  that conversation was deleted or everything cleared since (it must not
 *  be written back). */
let seq = 0
const newestSave = new Map<string, number>()
const deletedAt = new Map<string, number>()
let clearedAt = 0
const superseded = (id: string, mine: number): boolean =>
  newestSave.get(id) !== mine || (deletedAt.get(id) ?? 0) > mine || clearedAt > mine
const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms))

/** The chat page saves a conversation after each change and refreshes the
 *  list. A failed save is tried again a few times; false when it still
 *  failed (the page then writes it again on its next change). History is
 *  best effort; the chat itself goes on. */
export async function saveConversation(record: ChatHistoryRecord): Promise<boolean> {
  const history = api()
  if (!history) return true
  const mine = ++seq
  newestSave.set(record.id, mine)
  for (let attempt = 0; ; attempt++) {
    try {
      await history.save(record)
      break
    } catch {
      if (attempt >= SAVE_RETRY_MS.length) return false
      await sleep(SAVE_RETRY_MS[attempt])
      if (superseded(record.id, mine)) return true
    }
  }
  await refreshHistory()
  return true
}

export async function loadConversation(id: string): Promise<ChatHistoryRecord | null> {
  return (await api()?.get(id).catch(() => null)) ?? null
}

export function setActiveConversation(id: string | null): void {
  if (state.activeId !== id) set({ activeId: id })
}

export function setChatBusy(busy: boolean): void {
  if (state.busy !== busy) set({ busy })
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

/** Deletes one conversation; deleting the open one starts a new chat. The
 *  open one is not deleted while its reply is in flight: the reply would
 *  save it again. */
export async function deleteConversation(id: string): Promise<void> {
  if (state.busy && state.activeId === id) return
  deletedAt.set(id, ++seq)
  // The page leaves the conversation first, synchronously, so nothing sent
  // while the deletion is on its way can land in it and save it again.
  if (state.activeId === id) {
    starter?.()
    set({ activeId: null })
  }
  await api()?.delete(id).catch(() => {})
  await refreshHistory()
}

/** Deletes every conversation and starts a new chat; not while a reply is
 *  in flight, for the same reason. */
export async function clearConversations(): Promise<void> {
  if (state.busy) return
  clearedAt = ++seq
  starter?.()
  set({ activeId: null })
  await api()?.clear().catch(() => {})
  await refreshHistory()
}

/** For tests: forget everything. */
export function resetHistoryStore(): void {
  state = initial
  opener = null
  starter = null
}
