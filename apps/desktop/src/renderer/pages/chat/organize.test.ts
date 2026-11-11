// @vitest-environment jsdom
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createChatHistoryStore } from '../../../main/chat-history'
import { CHAT_HISTORY_FORMAT, CHAT_HISTORY_VERSION, type ChatHistoryRecord } from '../../../shared/chat-history'
import {
  archiveConversation, createGroup, deleteConversation, deleteGroup, getHistoryState, moveConversation,
  refreshGroups, refreshHistory, registerChatPage, renameGroup, resetHistoryStore, restoreConversation,
  saveConversation, setActiveConversation, setChatBusy, setCollapsed,
} from './history'

// UI-06 G1: the page's side of groups and archiving, against main's real
// store behind a bridge that calls it as the IPC handlers do.

let dir: string
let store: ReturnType<typeof createChatHistoryStore>
const wrap = <A extends unknown[], R>(f: (...a: A) => R) => vi.fn(async (...a: A) => f(...a))
let bridge: Record<string, ReturnType<typeof vi.fn>>
let groupsBridge: Record<string, ReturnType<typeof vi.fn>>

function record(id: string, prompt: string, at = '2026-11-08T01:00:00.000Z'): ChatHistoryRecord {
  return {
    format: CHAT_HISTORY_FORMAT, version: CHAT_HISTORY_VERSION, id, title: prompt, title_source: 'prompt',
    group_id: null, archived_at: null, created_at: at, updated_at: at, runs: [], pending: [], initiator: 'user', unseen: [],
    messages: [{ role: 'user', content: prompt, at, delivered: true }],
  }
}
const item = (id: string) => getHistoryState().items.find(i => i.id === id)

beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'organize-'))
  store = createChatHistoryStore(dir)
  bridge = {
    list: wrap(() => store.list()), get: wrap((id: string) => store.get(id)), save: wrap((r: unknown) => store.save(r)),
    delete: wrap((id: string) => store.delete(id)), clear: wrap(() => store.clear()),
    setTitle: wrap((id: string, t: string, s: string) => store.setTitle(id, t, s)),
    organize: wrap((id: string, p: unknown) => store.organize(id, p)),
  }
  groupsBridge = {
    get: wrap(() => store.groups()), create: wrap((n: string) => store.createGroup(n)),
    rename: wrap((id: string, n: string) => store.renameGroup(id, n)), delete: wrap((id: string) => store.deleteGroup(id)),
    setCollapsed: wrap((t: string, c: boolean) => store.setCollapsed(t, c)),
  }
  Object.defineProperty(window, 'agent24', { value: { chatHistory: bridge, chatGroups: groupsBridge }, writable: true, configurable: true })
  resetHistoryStore()
})
afterEach(() => { fs.rmSync(dir, { recursive: true, force: true }) })

describe('organizing conversations (UI-06 G1)', () => {
  it('moves, archives and restores, updating the list from what main wrote', async () => {
    await saveConversation(record('chat_a', '周报'))
    await createGroup('工作')
    const work = getHistoryState().groups.groups[0]
    expect(await moveConversation('chat_a', work.id)).toBe(true)
    expect(item('chat_a')).toMatchObject({ group_id: work.id, archived_at: null })
    expect(await archiveConversation('chat_a')).toBe(true)
    expect(item('chat_a')?.archived_at).toMatch(/Z$/)
    // A later save of the page carries neither, and changes neither.
    await saveConversation({ ...record('chat_a', '周报', '2026-11-08T02:00:00.000Z') })
    expect(item('chat_a')).toMatchObject({ group_id: work.id, archived_at: expect.stringMatching(/Z$/) })
    expect(await restoreConversation('chat_a')).toBe(true)
    expect(item('chat_a')).toMatchObject({ group_id: work.id, archived_at: null })
    expect(await moveConversation('chat_a', null)).toBe(true)
    expect(item('chat_a')?.group_id).toBeNull()
  })

  it('archiving the open conversation starts a new chat first; not while its reply is in flight', async () => {
    const start = vi.fn()
    registerChatPage(() => {}, start)
    await saveConversation(record('chat_a', '周报'))
    setActiveConversation('chat_a')
    setChatBusy(true)
    expect(await archiveConversation('chat_a')).toBe(false)
    expect(start).not.toHaveBeenCalled()
    expect(bridge.organize).not.toHaveBeenCalled()
    setChatBusy(false)
    expect(await archiveConversation('chat_a')).toBe(true)
    expect(start).toHaveBeenCalledTimes(1)
    expect(getHistoryState().activeId).toBeNull()
  })

  it('a reply started while main archives the open conversation keeps it open (read-only)', async () => {
    const start = vi.fn()
    registerChatPage(() => {}, start)
    await saveConversation(record('chat_a', '周报'))
    setActiveConversation('chat_a')
    let deliver!: () => void
    bridge.organize.mockImplementationOnce((id: string, p: unknown) => {
      const answer = store.organize(id, p)
      return new Promise(r => { deliver = () => r(answer) })
    })
    const archiving = archiveConversation('chat_a')
    await vi.waitFor(() => expect(bridge.organize).toHaveBeenCalled())
    setChatBusy(true)
    deliver()
    expect(await archiving).toBe(true)
    expect(start).not.toHaveBeenCalled()
    expect(getHistoryState().activeId).toBe('chat_a')
  })

  it('a change answered after its conversation was deleted does not list it again', async () => {
    await saveConversation(record('chat_a', '周报'))
    // Main handles the change at once; its answer arrives after the deletion.
    let deliver!: () => void
    bridge.organize.mockImplementationOnce((id: string, p: unknown) => {
      const answer = store.organize(id, p)
      return new Promise(r => { deliver = () => r(answer) })
    })
    const archiving = archiveConversation('chat_a')
    await vi.waitFor(() => expect(bridge.organize).toHaveBeenCalled())
    await deleteConversation('chat_a')
    deliver()
    expect(await archiving).toBe(false)
    expect(item('chat_a')).toBeUndefined()
  })

  it('a list read that started before a change does not undo it', async () => {
    await saveConversation(record('chat_a', '周报'))
    const snapshot = store.list()
    let listed!: (v: unknown) => void
    bridge.list.mockImplementationOnce(() => new Promise(r => { listed = r }))
    const reading = refreshHistory()
    await archiveConversation('chat_a')
    listed(snapshot)
    await reading
    expect(item('chat_a')?.archived_at).toMatch(/Z$/)
  })

  it('a failed change leaves the list as it was', async () => {
    await saveConversation(record('chat_a', '周报'))
    expect(await moveConversation('chat_a', 'grp_never')).toBe(false)
    expect(item('chat_a')).toMatchObject({ group_id: null })
    bridge.organize.mockRejectedValueOnce(new Error('EACCES'))
    expect(await archiveConversation('chat_a')).toBe(false)
    expect(item('chat_a')?.archived_at).toBeNull()
  })
})

describe('groups (UI-06 G1)', () => {
  it('loads, creates, renames and deletes groups as main keeps them', async () => {
    store.createGroup('旧组')
    await refreshGroups()
    expect(getHistoryState().groups.groups.map(g => g.name)).toEqual(['旧组'])
    expect(await createGroup('  新\n组 ')).toMatch(/^grp_/)
    const [, added] = getHistoryState().groups.groups
    expect(added.name).toBe('新 组')
    expect(await renameGroup(added.id, '改名')).toBe(true)
    expect(getHistoryState().groups.groups.map(g => g.name)).toEqual(['旧组', '改名'])
    expect(await deleteGroup(added.id)).toBe(true)
    expect(getHistoryState().groups.groups.map(g => g.name)).toEqual(['旧组'])
    expect(await createGroup('   ')).toBeNull()
    expect(groupsBridge.create).toHaveBeenCalledTimes(1)
  })

  it('folds at once and keeps the fold in main', async () => {
    await createGroup('工作')
    const work = getHistoryState().groups.groups[0]
    const folding = setCollapsed('assistant', true)
    expect(getHistoryState().groups.assistant_collapsed).toBe(true)
    await folding
    await setCollapsed(work.id, true)
    await setCollapsed('archive', false)
    expect(store.groups()).toMatchObject({ assistant_collapsed: true, archive_collapsed: false, groups: [{ collapsed: true }] })
  })

  it('an answer overtaken by a newer change is not shown', async () => {
    let first!: () => void
    // Main handles both in order; the first answer is delivered last.
    groupsBridge.setCollapsed.mockImplementationOnce((t: string, c: boolean) => {
      const answer = store.setCollapsed(t, c)
      return new Promise(r => { first = () => r(answer) })
    })
    const folding = setCollapsed('assistant', true)
    await setCollapsed('assistant', false)
    first()
    await folding
    expect(getHistoryState().groups.assistant_collapsed).toBe(false)
    expect(store.groups().assistant_collapsed).toBe(false)
  })

  it('a read that started before a change does not bring back what the change removed', async () => {
    await createGroup('工作')
    const work = getHistoryState().groups.groups[0]
    // Main reads the groups first; that answer is delivered after the deletion's.
    let read!: () => void
    groupsBridge.get.mockImplementationOnce(() => {
      const answer = store.groups()
      return new Promise(r => { read = () => r(answer) })
    })
    const refreshing = refreshGroups()
    await deleteGroup(work.id)
    read()
    await refreshing
    expect(getHistoryState().groups.groups).toEqual([])
  })

  it('when the newest change fails, the groups are read again: an earlier one that landed shows', async () => {
    // Main makes the group; its answer is held until the fold has failed.
    let made!: () => void
    groupsBridge.create.mockImplementationOnce((n: string) => {
      const answer = store.createGroup(n)
      return new Promise(r => { made = () => r(answer) })
    })
    groupsBridge.setCollapsed.mockRejectedValueOnce(new Error('EACCES'))
    const creating = createGroup('工作')
    expect(await setCollapsed('assistant', true)).toBe(false)
    made()
    await creating
    expect(getHistoryState().groups.groups.map(g => g.name)).toEqual(['工作'])
    // The fold shown at once goes back, as main did not keep it.
    expect(getHistoryState().groups.assistant_collapsed).toBe(false)
  })
})
