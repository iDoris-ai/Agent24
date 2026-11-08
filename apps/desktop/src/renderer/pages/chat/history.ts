// UI-03: the main chat's history, between the sidebar list and the chat page.
// The list lives in the sidebar, the conversation in the always-mounted chat
// page; this module carries the list, which conversation is open, and "open
// that one" requests from the list to the page.

import {
  chatHistorySummary, type ChatGroupsState, type ChatHistoryOrganize, type ChatHistoryRecord, type ChatHistorySummary,
} from '../../../shared/chat-history'
import { renameTitle } from './title'

export interface HistoryState {
  items: ChatHistorySummary[]
  /** The conversation the chat page shows, once it has been saved. Only the
   *  chat page sets it. */
  activeId: string | null
  /** A reply is in flight: the page cannot switch conversations, so the list
   *  does not offer to, nor to delete the open one or clear everything. */
  busy: boolean
  /** UI-06: the user's groups and which parts of the list are folded. */
  groups: ChatGroupsState
}

const NO_GROUPS: ChatGroupsState = { groups: [], assistant_collapsed: false, archive_collapsed: true }
const initial: HistoryState = { items: [], activeId: null, busy: false, groups: NO_GROUPS }
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

/** Reads the list again from main. Failures leave the list as it was. A
 *  read that started before a save or a deletion finished does not undo it:
 *  entries saved since it started are kept, ones removed since are dropped. */
export async function refreshHistory(): Promise<void> {
  const mark = seq
  const list = await api()?.list().catch(() => null)
  if (!list) return
  const newer = state.items.filter(item => (publishedAt.get(item.id) ?? 0) > mark)
  set({
    items: byNewest([...newer, ...list.filter(item => !newer.some(n => n.id === item.id) && !removedSince(item.id, mark))]),
  })
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
const removedSince = (id: string, mark: number): boolean => (deletedAt.get(id) ?? 0) > mark || clearedAt > mark
const superseded = (id: string, mine: number): boolean => newestSave.get(id) !== mine || removedSince(id, mine)
/** The newest record of each conversation whose save has not finished yet:
 *  what is on disk may be older (e.g. while a failed save waits to retry),
 *  so opening the conversation reads this first. */
const unsaved = new Map<string, { mine: number; record: ChatHistoryRecord }>()
/** The save whose summary the list shows, per conversation, and when (in
 *  the same order as `seq`) that summary went into the list. */
const listedAt = new Map<string, number>()
const publishedAt = new Map<string, number>()
const byNewest = (items: ChatHistorySummary[]): ChatHistorySummary[] =>
  items.sort((a, b) => Date.parse(b.updated_at) - Date.parse(a.updated_at))
const realSleep = (ms: number) => new Promise<void>(resolve => setTimeout(resolve, ms))
let sleep = realSleep
/** For tests: how a failed save waits before its retry. */
export function setRetrySleepForTests(wait: (ms: number) => Promise<void>): void {
  sleep = wait
}

/** The chat page saves a conversation after each change and refreshes the
 *  list. A failed save is tried again a few times; false when it still
 *  failed (the page then writes it again on its next change). History is
 *  best effort; the chat itself goes on. */
export async function saveConversation(record: ChatHistoryRecord): Promise<boolean> {
  const history = api()
  if (!history) return true
  const mine = ++seq
  newestSave.set(record.id, mine)
  unsaved.set(record.id, { mine, record })
  const done = () => { if (unsaved.get(record.id)?.mine === mine) unsaved.delete(record.id) }
  let written: ChatHistorySummary | undefined
  for (let attempt = 0; ; attempt++) {
    try {
      written = await history.save(record)
      break
    } catch {
      if (attempt >= SAVE_RETRY_MS.length) {
        done()
        return false
      }
      await sleep(SAVE_RETRY_MS[attempt])
      if (superseded(record.id, mine)) return true
    }
  }
  done()
  // Listed unless a newer save already listed it, or it was deleted or
  // everything cleared since this save was asked for.
  if ((listedAt.get(record.id) ?? 0) > mine || removedSince(record.id, mine)) return true
  listedAt.set(record.id, mine)
  publishedAt.set(record.id, ++seq)
  // The list is updated from what main wrote (which keeps a rename over the
  // first prompt's title), not by reading every conversation again from
  // disk: that would cost a full scan per chat turn.
  const summary = written ?? chatHistorySummary(record)
  set({ items: byNewest([summary, ...state.items.filter(item => item.id !== record.id)]) })
  return true
}

/** UI-04: renames a saved conversation. Main keeps it over the first
 *  prompt's title and the model's, so later saves do not undo it. False when
 *  there was nothing to keep, or the rename failed; a conversation deleted
 *  meanwhile stays deleted. */
export function renameConversation(id: string, input: string): Promise<boolean> {
  return retitle(id, renameTitle(input), 'user')
}

/** UI-04 T2: gives a saved conversation the title the model wrote. Main
 *  refuses it over a rename, however late it arrives. */
export function titleConversation(id: string, title: string): Promise<boolean> {
  return retitle(id, title, 'model')
}

/** UI-06: moves a saved conversation into a group, or out of any (null). */
export function moveConversation(id: string, groupId: string | null): Promise<boolean> {
  return organize(id, { group_id: groupId })
}

/** UI-06: archives a saved conversation. Archiving the open one starts a new
 *  chat, as deleting it does, and not while its reply is in flight: the
 *  reply would land in a conversation the list no longer shows. */
export function archiveConversation(id: string): Promise<boolean> {
  if (state.busy && state.activeId === id) return Promise.resolve(false)
  if (state.activeId === id) {
    starter?.()
    set({ activeId: null })
  }
  return organize(id, { archived: true })
}

/** UI-06: brings an archived conversation back, into its group if that
 *  still exists. */
export function restoreConversation(id: string): Promise<boolean> {
  return organize(id, { archived: false })
}

async function organize(id: string, patch: ChatHistoryOrganize): Promise<boolean> {
  const history = api()
  if (!history) return false
  const mine = ++seq
  const summary = await history.organize(id, patch).catch(() => null)
  if (!summary || removedSince(id, mine)) return false
  // Ordered against saves, deletions and list reads as a rename is.
  publishedAt.set(id, ++seq)
  set({ items: byNewest([summary, ...state.items.filter(item => item.id !== id)]) })
  return true
}

const groupsApi = () => (typeof window === 'undefined' ? undefined : window.agent24?.chatGroups)

/** Main answers each group call with the groups as they then stand, in the
 *  order it handled them: the last answer is the newest. `groupSeq` counts
 *  the changes asked for. */
let groupSeq = 0

/** UI-06: reads the groups again from main; a failure leaves them as they
 *  were. An answer is shown only when no change was asked for since: that
 *  change's own answer comes later and is newer. */
export async function refreshGroups(): Promise<void> {
  const mark = groupSeq
  const groups = await groupsApi()?.get().catch(() => null)
  if (groups && mark === groupSeq) set({ groups })
}

async function changeGroups(change: () => Promise<ChatGroupsState> | undefined): Promise<boolean> {
  return (await groupChange(change)) !== null
}

/** A group change; main's answer, or null when it failed. */
async function groupChange(change: () => Promise<ChatGroupsState> | undefined): Promise<ChatGroupsState | null> {
  const mine = ++groupSeq
  const groups = await Promise.resolve(change()).catch(() => undefined)
  // Only the newest call's answer is shown: it carries every change main
  // handled before it, and showing an older one first would flicker a fold.
  // When the newest call fails, the groups are read again, so neither an
  // older change that did land nor a fold shown before main kept it is lost.
  if (!groups) {
    if (mine === groupSeq) await refreshGroups()
    return null
  }
  if (mine === groupSeq) set({ groups })
  return groups
}

/** Adds a group at the end; its id, or null when nothing was made. */
export async function createGroup(name: string): Promise<string | null> {
  const text = renameTitle(name)
  if (!text) return null
  const groups = await groupChange(() => groupsApi()?.create(text))
  // Main appends: the new one is the last group it answers with.
  return groups?.groups[groups.groups.length - 1]?.id ?? null
}

export function renameGroup(id: string, name: string): Promise<boolean> {
  const text = renameTitle(name)
  return text ? changeGroups(() => groupsApi()?.rename(id, text)) : Promise.resolve(false)
}

/** Deletes a group; its conversations stay, listed with the others. */
export function deleteGroup(id: string): Promise<boolean> {
  return changeGroups(() => groupsApi()?.delete(id))
}

/** Folds or unfolds a group, the whole list ('assistant') or the archived
 *  conversations ('archive'). The list folds at once; main keeps it. */
export function setCollapsed(target: 'assistant' | 'archive' | string, collapsed: boolean): Promise<boolean> {
  const g = state.groups
  if (target === 'assistant') set({ groups: { ...g, assistant_collapsed: collapsed } })
  else if (target === 'archive') set({ groups: { ...g, archive_collapsed: collapsed } })
  else set({ groups: { ...g, groups: g.groups.map(x => (x.id === target ? { ...x, collapsed } : x)) } })
  return changeGroups(() => groupsApi()?.setCollapsed(target, collapsed))
}

/** Whether conversations are kept at all (there is a history bridge). */
export const historyAvailable = (): boolean => api() !== undefined

async function retitle(id: string, title: string, source: 'user' | 'model'): Promise<boolean> {
  const history = api()
  if (!title || !history) return false
  const mine = ++seq
  const summary = await history.setTitle(id, title, source).catch(() => null)
  if (!summary || removedSince(id, mine)) return false
  // Main answers in the order it handled the calls, so a save handled before
  // this one has already updated the list and one handled after it returns
  // this title too.
  publishedAt.set(id, ++seq)
  set({ items: byNewest([summary, ...state.items.filter(item => item.id !== id)]) })
  return summary.title_source === source
}

/** Reads a conversation to open it: its newest record not yet saved if
 *  there is one, else the file. Null when it is gone, or was deleted or
 *  everything cleared while it was being read. */
export async function loadConversation(id: string): Promise<ChatHistoryRecord | null> {
  const pending = unsaved.get(id)
  if (pending) return pending.record
  const mark = seq
  const record = (await api()?.get(id).catch(() => null)) ?? null
  return record && !removedSince(id, mark) ? record : null
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
  unsaved.delete(id)
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
  unsaved.clear()
  starter?.()
  set({ activeId: null })
  await api()?.clear().catch(() => {})
  await refreshHistory()
}

/** For tests: forget everything. */
export function resetHistoryStore(): void {
  state = initial
  unsaved.clear()
  sleep = realSleep
  opener = null
  starter = null
}
