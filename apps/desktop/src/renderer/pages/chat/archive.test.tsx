// @vitest-environment jsdom
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { createChatHistoryStore } from '../../../main/chat-history'
import { CHAT_HISTORY_FORMAT, CHAT_HISTORY_VERSION, type ChatHistoryRecord } from '../../../shared/chat-history'
import ChatPage from '../Chat'
import { ChatHistoryList } from './ChatHistoryList'
import { DRAG_TYPE } from './drop'
import { archiveConversation, getHistoryState, resetHistoryStore, setChatBusy, setActiveConversation } from './history'
import { setLocale } from '../../i18n'

// UI-06 G3: the archive in the sidebar list and an archived conversation on
// the chat page, against main's real store behind a bridge that calls it as
// IPC does.

let dir: string
let store: ReturnType<typeof createChatHistoryStore>
const wrap = <A extends unknown[], R>(f: (...a: A) => R) => vi.fn(async (...a: A) => f(...a))
let bridge: Record<string, ReturnType<typeof vi.fn>>
const proxy = vi.fn()

function record(id: string, prompt: string, at: string): ChatHistoryRecord {
  return {
    format: CHAT_HISTORY_FORMAT, version: CHAT_HISTORY_VERSION, id, title: prompt, title_source: 'prompt',
    group_id: null, archived_at: null, created_at: at, updated_at: at,
    messages: [{ role: 'user', content: prompt, at, delivered: true }, { role: 'assistant', content: `答：${prompt}`, at, run_id: null, latency: null }],
  }
}
const put = (id: string, prompt: string, hour = 1) => store.save(record(id, prompt, `2026-11-08T${String(hour).padStart(2, '0')}:00:00.000Z`))

beforeEach(() => {
  setLocale('zh')
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'archive-ui-'))
  store = createChatHistoryStore(dir)
  bridge = {
    list: wrap(() => store.list()), get: wrap((id: string) => store.get(id)), save: wrap((r: unknown) => store.save(r)),
    delete: wrap((id: string) => store.delete(id)), clear: wrap(() => store.clear()),
    setTitle: wrap((id: string, t: string, s: string) => store.setTitle(id, t, s)),
    organize: wrap((id: string, p: unknown) => store.organize(id, p)),
  }
  const chatGroups = {
    get: wrap(() => store.groups()), create: wrap((n: string) => store.createGroup(n)),
    rename: wrap((id: string, n: string) => store.renameGroup(id, n)), delete: wrap((id: string) => store.deleteGroup(id)),
    setCollapsed: wrap((t: string, c: boolean) => store.setCollapsed(t, c)),
  }
  // A title request finds no model; every other call is the chat's.
  proxy.mockReset()
  proxy.mockImplementation(async (req: { body?: { messages?: Array<{ role: string }> } }) =>
    req.body?.messages?.[0]?.role === 'system'
      ? { ok: false, status: 503, data: { error: { code: 'provider_unavailable', message: 'no local model' } } }
      : { ok: true, status: 200, data: { message: { content: 'ack' } } })
  Object.defineProperty(window, 'agent24', {
    value: { chatHistory: bridge, chatGroups, backendProxy: proxy }, writable: true, configurable: true,
  })
  resetHistoryStore()
})
afterEach(() => { vi.restoreAllMocks(); fs.rmSync(dir, { recursive: true, force: true }) })

const list = () => screen.getByRole('region', { name: '历史对话' })
const row = (title: string) => within(list()).getByRole('button', { name: title })
const archive = () => within(list()).getByRole('group', { name: '已归档' })
const archiveHead = () => within(archive()).getByRole('button', { name: /已归档的对话$/ }).parentElement!
const openRowMenu = (title: string) => {
  fireEvent.click(within(list()).getByRole('button', { name: `对话「${title}」的操作` }))
  return within(list()).getByRole('menu')
}
const archivedAt = (id: string) => store.get(id)?.archived_at ?? null
const showList = async () => {
  render(<ChatHistoryList showChat={() => {}} />)
  await screen.findByRole('region', { name: '历史对话' })
}

describe('the archive in the history list (UI-06 G3)', () => {
  it('sits at the bottom, folded at first, with its count; archiving from the menu moves a row there', async () => {
    put('chat_a', '周报', 2)
    put('chat_b', '闲聊', 1)
    await showList()
    await within(list()).findByRole('button', { name: '周报' })
    expect(archiveHead()).toHaveTextContent('0')
    expect(within(archive()).getByRole('button', { name: '展开已归档的对话' })).toHaveAttribute('aria-expanded', 'false')
    // Recent's rows come before it.
    expect(row('闲聊').compareDocumentPosition(archive()) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy()

    fireEvent.click(within(openRowMenu('周报')).getByRole('menuitem', { name: '归档' }))
    await waitFor(() => expect(archivedAt('chat_a')).not.toBeNull())
    expect(within(list()).queryByRole('button', { name: '周报' })).toBeNull()
    expect(archiveHead()).toHaveTextContent('1')

    fireEvent.click(within(archive()).getByRole('button', { name: '展开已归档的对话' }))
    expect(within(archive()).getByRole('button', { name: '周报' })).toBeInTheDocument()
    await waitFor(() => expect(store.groups().archive_collapsed).toBe(false))
    // An archived row cannot be dragged; its menu restores or deletes.
    expect(row('周报').closest('li')).toHaveAttribute('draggable', 'false')
    expect(within(openRowMenu('周报')).getAllByRole('menuitem').map(i => i.textContent)).toEqual(['恢复', '删除…'])
  })

  it('archived conversations of a group are not in its rows or its count (PR-Daemon #1648 L1)', async () => {
    const { groups: [work] } = store.createGroup('工作')
    put('chat_a', '周报', 2)
    put('chat_b', '月报', 1)
    store.organize('chat_a', { group_id: work.id })
    store.organize('chat_b', { group_id: work.id, archived: true })
    await showList()
    const box = await within(list()).findByRole('group', { name: '工作' })
    expect(within(box).getByRole('button', { name: '周报' })).toBeInTheDocument()
    expect(within(box).queryByRole('button', { name: '月报' })).toBeNull()
    expect(within(box).getByRole('button', { name: /^折叠分组/ }).parentElement).toHaveTextContent(/工作1⋯$/)
    expect(archiveHead()).toHaveTextContent('1')
  })

  it('restoring brings it back into its group when the group is still there, else into Recent', async () => {
    const { groups: [work, gone] } = (store.createGroup('工作'), store.createGroup('旧组'))
    put('chat_a', '周报', 2)
    put('chat_b', '月报', 1)
    store.organize('chat_a', { group_id: work.id, archived: true })
    store.organize('chat_b', { group_id: gone.id, archived: true })
    store.deleteGroup(gone.id)
    store.setCollapsed('archive', false)
    await showList()
    await within(archive()).findByRole('button', { name: '周报' })
    fireEvent.click(within(openRowMenu('周报')).getByRole('menuitem', { name: '恢复' }))
    await waitFor(() => expect(within(within(list()).getByRole('group', { name: '工作' })).getByRole('button', { name: '周报' }))
      .toBeInTheDocument())
    fireEvent.click(within(openRowMenu('月报')).getByRole('menuitem', { name: '恢复' }))
    await waitFor(() => expect(within(archive()).queryByRole('button', { name: '月报' })).toBeNull())
    expect(row('月报')).toBeInTheDocument()
    expect(archivedAt('chat_a')).toBeNull()
    expect(archivedAt('chat_b')).toBeNull()
  })

  it('an archived conversation is not renamed by F2 or a double click', async () => {
    put('chat_a', '周报')
    store.organize('chat_a', { archived: true })
    store.setCollapsed('archive', false)
    await showList()
    const entry = await within(archive()).findByRole('button', { name: '周报' })
    fireEvent.keyDown(entry, { key: 'F2' })
    fireEvent.doubleClick(entry)
    expect(within(list()).queryByRole('textbox', { name: '对话标题' })).toBeNull()
  })

  it('a rename is not started while a conversation is being archived, and one open is dropped when it is', async () => {
    put('chat_a', '周报', 2)
    put('chat_b', '闲聊', 1)
    await showList()
    await within(list()).findByRole('button', { name: '周报' })
    let land!: () => void
    bridge.organize.mockImplementation((id: string, p: unknown) => {
      const answer = store.organize(id, p)
      return new Promise(r => { land = () => r(answer) })
    })
    // Being archived: F2 opens nothing.
    let archivingA!: Promise<boolean>
    act(() => { archivingA = archiveConversation('chat_a') })
    fireEvent.keyDown(row('周报'), { key: 'F2' })
    expect(within(list()).queryByRole('textbox', { name: '对话标题' })).toBeNull()
    await act(async () => { land(); await archivingA })
    // A rename open when its conversation is archived (from elsewhere) is dropped, unsaved.
    fireEvent.keyDown(row('闲聊'), { key: 'F2' })
    fireEvent.change(within(list()).getByRole('textbox', { name: '对话标题' }), { target: { value: '新名' } })
    let archivingB!: Promise<boolean>
    act(() => { archivingB = archiveConversation('chat_b') })
    await waitFor(() => expect(within(list()).queryByRole('textbox', { name: '对话标题' })).toBeNull())
    await act(async () => { land(); await archivingB })
    expect(archivedAt('chat_b')).not.toBeNull()
    expect(bridge.setTitle).not.toHaveBeenCalled()
  })

  it('deleting an archived conversation asks first', async () => {
    put('chat_a', '周报')
    store.organize('chat_a', { archived: true })
    store.setCollapsed('archive', false)
    await showList()
    await within(archive()).findByRole('button', { name: '周报' })
    const confirm = vi.spyOn(window, 'confirm').mockReturnValueOnce(false).mockReturnValueOnce(true)
    fireEvent.click(within(openRowMenu('周报')).getByRole('menuitem', { name: '删除…' }))
    expect(store.get('chat_a')).not.toBeNull()
    fireEvent.click(within(openRowMenu('周报')).getByRole('menuitem', { name: '删除…' }))
    await waitFor(() => expect(store.get('chat_a')).toBeNull())
    expect(confirm).toHaveBeenCalledTimes(2)
  })

  it('a search finds archived conversations too, tagged', async () => {
    for (let i = 0; i < 9; i++) put(`chat_${i}`, `话题${i}`, i + 1)
    store.organize('chat_4', { archived: true })
    await showList()
    fireEvent.change(await within(list()).findByRole('searchbox', { name: '搜索对话' }), { target: { value: '话题4' } })
    expect(row('话题4 已归档')).toBeInTheDocument()
    expect(within(list()).queryByRole('group', { name: '已归档' })).toBeNull()
  })

  it('a search box counts archived conversations, as the search finds them (D5)', async () => {
    for (let i = 0; i < 9; i++) put(`chat_${i}`, `话题${i}`, i + 1)
    for (let i = 0; i < 5; i++) store.organize(`chat_${i}`, { archived: true })
    await showList()
    expect(await within(list()).findByRole('searchbox', { name: '搜索对话' })).toBeInTheDocument()
  })
})

describe('dragging onto the archive (UI-06 G3)', () => {
  const dt = (id: string) => ({ setData: vi.fn(), getData: (t: string) => (t === DRAG_TYPE ? id : ''), types: [DRAG_TYPE], dropEffect: 'none', effectAllowed: 'all' })
  const drag = (title: string) => {
    const id = row(title).dataset.historyId!
    fireEvent.dragStart(row(title).closest('li')!, { dataTransfer: dt(id) })
    return id
  }

  it('archives it at once; the heading lights up while over it', async () => {
    put('chat_a', '周报')
    await showList()
    await within(list()).findByRole('button', { name: '周报' })
    const id = drag('周报')
    expect(!fireEvent.dragOver(archiveHead(), { dataTransfer: dt(id) })).toBe(true)
    expect(archiveHead()).toHaveClass('drop-over')
    fireEvent.drop(archiveHead(), { dataTransfer: dt(id) })
    await waitFor(() => expect(archivedAt('chat_a')).not.toBeNull())
    expect(archiveHead()).toHaveTextContent('1')
    expect(archiveHead()).not.toHaveClass('drop-over')
  })

  it('not the open conversation while its reply is on its way', async () => {
    put('chat_a', '周报')
    await showList()
    await within(list()).findByRole('button', { name: '周报' })
    setActiveConversation('chat_a')
    setChatBusy(true)
    const id = drag('周报')
    expect(!fireEvent.dragOver(archiveHead(), { dataTransfer: dt(id) })).toBe(false)
    expect(fireEvent.drop(archiveHead(), { dataTransfer: dt(id) })).toBe(true)
    await new Promise(r => setTimeout(r, 0))
    expect(bridge.organize).not.toHaveBeenCalled()
    // Nor from the menu.
    expect(within(openRowMenu('周报')).getByRole('menuitem', { name: '归档' })).toBeDisabled()
  })
})

describe('an archived conversation on the chat page (UI-06 D7)', () => {
  const both = () => render(<><ChatHistoryList showChat={() => {}} /><ChatPage /></>)

  it('opens read-only; restoring it lets the chat go on', async () => {
    put('chat_a', '周报')
    store.organize('chat_a', { archived: true })
    store.setCollapsed('archive', false)
    both()
    fireEvent.click(await within(await screen.findByRole('region', { name: '历史对话' })).findByRole('button', { name: '周报' }))
    await waitFor(() => expect(screen.getByText('答：周报')).toBeInTheDocument())
    expect(screen.getByText('这个对话已归档，只能查看。')).toBeInTheDocument()
    expect(screen.queryByPlaceholderText(/输入消息/)).toBeNull()

    fireEvent.click(screen.getByRole('button', { name: '恢复后继续' }))
    await waitFor(() => expect(screen.getByPlaceholderText(/输入消息/)).toBeInTheDocument())
    expect(archivedAt('chat_a')).toBeNull()
    const box = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(box, { target: { value: '再写一点' } })
    fireEvent.keyDown(box, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    expect(store.get('chat_a')?.messages.at(-1)?.content).toBe('ack')
  })

  it('archiving the open conversation leaves it for a new chat once main has archived it', async () => {
    both()
    const box = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(box, { target: { value: '周报' } })
    fireEvent.keyDown(box, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    const id = await waitFor(() => { const a = getHistoryState().activeId; expect(a).not.toBeNull(); return a! })
    await waitFor(() => expect(within(list()).getByRole('button', { name: '对话「周报」的操作' })).toBeInTheDocument())
    fireEvent.click(within(openRowMenu('周报')).getByRole('menuitem', { name: '归档' }))
    await waitFor(() => expect(screen.queryByText('ack')).toBeNull())
    expect(archivedAt(id)).not.toBeNull()
    expect(getHistoryState().activeId).toBeNull()
    expect(screen.getByPlaceholderText(/输入消息/)).toBeInTheDocument()
  })

  it('while main is archiving the open conversation, it is read-only already; a failed archive gives it back', async () => {
    both()
    const box = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(box, { target: { value: '周报' } })
    fireEvent.keyDown(box, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    await waitFor(() => expect(within(list()).getByRole('button', { name: '对话「周报」的操作' })).toBeInTheDocument())
    let answer!: (fail: boolean) => void
    bridge.organize.mockImplementationOnce(() => new Promise((ok, no) => { answer = fail => (fail ? no(new Error('EACCES')) : ok(null)) }))
    fireEvent.click(within(openRowMenu('周报')).getByRole('menuitem', { name: '归档' }))
    await waitFor(() => expect(screen.getByText('这个对话已归档，只能查看。')).toBeInTheDocument())
    expect(screen.queryByPlaceholderText(/输入消息/)).toBeNull()
    // Nor restored before main has answered.
    expect(screen.getByRole('button', { name: '恢复后继续' })).toBeDisabled()
    const sent = proxy.mock.calls.length
    answer(true)
    await waitFor(() => expect(screen.getByPlaceholderText(/输入消息/)).toBeInTheDocument())
    expect(screen.getByText('ack')).toBeInTheDocument()
    expect(proxy.mock.calls.length).toBe(sent)
  })

  it('a failed archive leaves the open conversation open (PR-Daemon #1647 L2)', async () => {
    both()
    const box = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(box, { target: { value: '周报' } })
    fireEvent.keyDown(box, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    await waitFor(() => expect(within(list()).getByRole('button', { name: '对话「周报」的操作' })).toBeInTheDocument())
    bridge.organize.mockRejectedValueOnce(new Error('EACCES'))
    fireEvent.click(within(openRowMenu('周报')).getByRole('menuitem', { name: '归档' }))
    await waitFor(() => expect(bridge.organize).toHaveBeenCalled())
    await new Promise(r => setTimeout(r, 0))
    expect(screen.getByText('ack')).toBeInTheDocument()
    expect(getHistoryState().activeId).not.toBeNull()
  })
})
