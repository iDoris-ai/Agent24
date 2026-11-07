// @vitest-environment jsdom
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { createChatHistoryStore } from '../../../main/chat-history'
import { CHAT_HISTORY_FORMAT, CHAT_HISTORY_VERSION, type ChatHistoryRecord } from '../../../shared/chat-history'
import { ChatHistoryList } from './ChatHistoryList'
import { refreshGroups, resetHistoryStore, saveConversation } from './history'
import { setLocale } from '../../i18n'
import { DRAG_TYPE, dropChange } from './drop'

// UI-06 G2: groups, folds, the ⋯ menu and dragging in the sidebar list,
// against main's real store behind a bridge that calls it as IPC does.

let dir: string
let store: ReturnType<typeof createChatHistoryStore>
const wrap = <A extends unknown[], R>(f: (...a: A) => R) => vi.fn(async (...a: A) => f(...a))
let bridge: Record<string, ReturnType<typeof vi.fn>>
let groupsBridge: Record<string, ReturnType<typeof vi.fn>>

function record(id: string, prompt: string, at: string): ChatHistoryRecord {
  return {
    format: CHAT_HISTORY_FORMAT, version: CHAT_HISTORY_VERSION, id, title: prompt, title_source: 'prompt',
    group_id: null, archived_at: null, created_at: at, updated_at: at,
    messages: [{ role: 'user', content: prompt, at, delivered: true }],
  }
}
const put = (id: string, prompt: string, hour = 1) => store.save(record(id, prompt, `2026-11-08T${String(hour).padStart(2, '0')}:00:00.000Z`))

beforeEach(() => {
  setLocale('zh')
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'groups-ui-'))
  store = createChatHistoryStore(dir)
  bridge = {
    list: wrap(() => store.list()), get: wrap((id: string) => store.get(id)), save: wrap((r: unknown) => store.save(r)),
    delete: wrap((id: string) => store.delete(id)), clear: wrap(() => store.clear()),
    setTitle: wrap((id: string, t: string, s: string) => store.setTitle(id, t, s)),
    organize: wrap((id: string, p: unknown) => store.organize(id, p)),
  }
  const chatGroups = groupsBridge = {
    get: wrap(() => store.groups()), create: wrap((n: string) => store.createGroup(n)),
    rename: wrap((id: string, n: string) => store.renameGroup(id, n)), delete: wrap((id: string) => store.deleteGroup(id)),
    setCollapsed: wrap((t: string, c: boolean) => store.setCollapsed(t, c)),
  }
  Object.defineProperty(window, 'agent24', { value: { chatHistory: bridge, chatGroups }, writable: true, configurable: true })
  resetHistoryStore()
})
afterEach(() => { vi.restoreAllMocks(); fs.rmSync(dir, { recursive: true, force: true }) })

const list = () => screen.getByRole('region', { name: '历史对话' })
const row = (title: string) => within(list()).getByRole('button', { name: title })
const groupBox = (name: string) => within(list()).getByRole('group', { name })
const groupHead = (name: string) => within(groupBox(name)).getByRole('button', { name: new RegExp(`分组：${name}$`) }).parentElement!
const recentHead = () => within(list()).getByRole('button', { name: /历史对话$/ }).parentElement!
const groupOfStored = (id: string) => store.get(id)?.group_id ?? null
const showList = async () => {
  render(<ChatHistoryList showChat={() => {}} />)
  await screen.findByRole('region', { name: '历史对话' })
}
const openRowMenu = (title: string) => {
  fireEvent.click(within(list()).getByRole('button', { name: `对话「${title}」的操作` }))
  return within(list()).getByRole('menu')
}
/** What a drag carries: by default the conversation this list is dragging
 *  (its type and id); `''` for something dragged in from elsewhere. */
let carrying = ''
const dt = (id = carrying, types = id ? [DRAG_TYPE] : ['Files']) =>
  ({ setData: vi.fn(), getData: (type: string) => (type === DRAG_TYPE ? id : ''), types, dropEffect: 'none', effectAllowed: 'all' })

describe('groups in the history list (UI-06 G2)', () => {
  it('lists groups above the ungrouped conversations, with their counts; folds keep in main', async () => {
    const { groups: [work] } = store.createGroup('工作')
    put('chat_a', '周报', 3)
    put('chat_b', '闲聊', 2)
    store.organize('chat_a', { group_id: work.id })
    await showList()
    await within(list()).findByRole('group', { name: '工作' })
    expect(within(groupBox('工作')).getByRole('button', { name: '周报' })).toBeInTheDocument()
    expect(within(groupBox('工作')).queryByRole('button', { name: '闲聊' })).toBeNull()
    expect(groupHead('工作')).toHaveTextContent('1')
    // The group comes before Recent's own conversations.
    expect(groupBox('工作').compareDocumentPosition(row('闲聊')) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy()

    fireEvent.click(within(list()).getByRole('button', { name: '折叠分组：工作' }))
    expect(within(list()).queryByRole('button', { name: '周报' })).toBeNull()
    expect(within(list()).getByRole('button', { name: '展开分组：工作' })).toHaveAttribute('aria-expanded', 'false')
    await waitFor(() => expect(store.groups().groups[0].collapsed).toBe(true))

    fireEvent.click(within(list()).getByRole('button', { name: '折叠历史对话' }))
    expect(within(list()).queryByRole('button', { name: '闲聊' })).toBeNull()
    expect(within(list()).queryByRole('group')).toBeNull()
    expect(within(list()).getByRole('button', { name: '展开历史对话' })).toHaveAttribute('aria-expanded', 'false')
    await waitFor(() => expect(store.groups().assistant_collapsed).toBe(true))
    fireEvent.click(within(list()).getByRole('button', { name: '展开历史对话' }))
    expect(row('闲聊')).toBeInTheDocument()
  })

  it('makes a group from "+ 分组" with Enter; Escape or an empty name makes none', async () => {
    put('chat_a', '周报')
    await showList()
    fireEvent.click(await within(list()).findByRole('button', { name: '+ 分组' }))
    let box = within(list()).getByRole('textbox', { name: '分组名称' })
    fireEvent.change(box, { target: { value: '  工   作 ' } })
    fireEvent.keyDown(box, { key: 'Enter' })
    await within(list()).findByRole('group', { name: '工 作' })
    fireEvent.click(within(list()).getByRole('button', { name: '+ 分组' }))
    box = within(list()).getByRole('textbox', { name: '分组名称' })
    fireEvent.change(box, { target: { value: '不要' } })
    fireEvent.keyDown(box, { key: 'Escape' })
    fireEvent.click(within(list()).getByRole('button', { name: '+ 分组' }))
    fireEvent.blur(within(list()).getByRole('textbox', { name: '分组名称' }))
    await new Promise(r => setTimeout(r, 0))
    expect(store.groups().groups.map(g => g.name)).toEqual(['工 作'])
  })

  it('moves a conversation from its menu: into a group, into a new one, and out', async () => {
    store.createGroup('工作')
    put('chat_a', '周报')
    await showList()
    await within(list()).findByRole('group', { name: '工作' })
    fireEvent.click(within(openRowMenu('周报')).getByRole('menuitem', { name: '工作' }))
    await waitFor(() => expect(within(groupBox('工作')).getByRole('button', { name: '周报' })).toBeInTheDocument())
    expect(groupOfStored('chat_a')).toBe(store.groups().groups[0].id)

    // The menu does not offer the group it is in; it offers to take it out.
    const menu = openRowMenu('周报')
    expect(within(menu).queryByRole('menuitem', { name: '工作' })).toBeNull()
    fireEvent.click(within(menu).getByRole('menuitem', { name: '新建分组…' }))
    const box = within(list()).getByRole('textbox', { name: '分组名称' })
    fireEvent.change(box, { target: { value: '周报们' } })
    fireEvent.keyDown(box, { key: 'Enter' })
    await waitFor(() => expect(within(groupBox('周报们')).getByRole('button', { name: '周报' })).toBeInTheDocument())

    fireEvent.click(within(openRowMenu('周报')).getByRole('menuitem', { name: '移出分组' }))
    await waitFor(() => expect(groupOfStored('chat_a')).toBeNull())
    expect(within(groupBox('周报们')).queryByRole('button', { name: '周报' })).toBeNull()
    expect(row('周报')).toBeInTheDocument()
  })

  it('a right click opens the same menu where the pointer is; arrows, Home, End and Escape', async () => {
    put('chat_a', '周报')
    await showList()
    const open = await within(list()).findByRole('button', { name: '周报' })
    fireEvent.contextMenu(open.closest('li')!, { clientX: 40, clientY: 90 })
    const menu = within(list()).getByRole('menu', { name: '对话「周报」的操作' })
    expect(menu).toHaveStyle({ left: '40px', top: '90px' })
    const items = within(menu).getAllByRole('menuitem')
    expect(items.map(i => i.textContent)).toEqual(['重命名', '新建分组…', '删除…'])
    expect(items[0]).toHaveFocus()
    fireEvent.keyDown(menu, { key: 'ArrowDown' })
    expect(items[1]).toHaveFocus()
    fireEvent.keyDown(menu, { key: 'End' })
    expect(items[2]).toHaveFocus()
    fireEvent.keyDown(menu, { key: 'ArrowDown' })
    expect(items[0]).toHaveFocus()
    fireEvent.keyDown(menu, { key: 'ArrowUp' })
    expect(items[2]).toHaveFocus()
    fireEvent.keyDown(menu, { key: 'Home' })
    expect(items[0]).toHaveFocus()

    // From the keyboard, Escape gives focus back to what opened it.
    const dots = within(list()).getByRole('button', { name: '对话「周报」的操作' })
    fireEvent.click(dots)
    fireEvent.keyDown(within(list()).getByRole('menu'), { key: 'Escape' })
    expect(within(list()).queryByRole('menu')).toBeNull()
    await waitFor(() => expect(dots).toHaveFocus())
    // A click outside closes it too, and so does Tab.
    fireEvent.click(dots)
    fireEvent.mouseDown(document.body)
    expect(within(list()).queryByRole('menu')).toBeNull()
    fireEvent.click(dots)
    fireEvent.keyDown(within(list()).getByRole('menu'), { key: 'Tab' })
    expect(within(list()).queryByRole('menu')).toBeNull()
  })

  it('renames from the menu; F2 on the entry still does', async () => {
    put('chat_a', '周报')
    await showList()
    await within(list()).findByRole('button', { name: '周报' })
    fireEvent.click(within(openRowMenu('周报')).getByRole('menuitem', { name: '重命名' }), { detail: 0 })
    const input = within(list()).getByRole('textbox', { name: '对话标题' })
    // Picked from the keyboard, the box keeps the focus it took.
    await new Promise(r => requestAnimationFrame(r))
    await new Promise(r => requestAnimationFrame(r))
    expect(input).toHaveFocus()
    fireEvent.change(input, { target: { value: '月报' } })
    fireEvent.keyDown(input, { key: 'Enter' })
    await waitFor(() => expect(store.get('chat_a')?.title).toBe('月报'))
    fireEvent.keyDown(await within(list()).findByRole('button', { name: '月报' }), { key: 'F2' })
    expect(within(list()).getByRole('textbox', { name: '对话标题' })).toBeInTheDocument()
  })

  it('deleting a conversation asks first', async () => {
    put('chat_a', '周报')
    await showList()
    await within(list()).findByRole('button', { name: '周报' })
    const confirm = vi.spyOn(window, 'confirm').mockReturnValueOnce(false).mockReturnValueOnce(true)
    fireEvent.click(within(openRowMenu('周报')).getByRole('menuitem', { name: '删除…' }))
    expect(confirm).toHaveBeenLastCalledWith('删除对话「周报」？删除后不能恢复。')
    expect(bridge.delete).not.toHaveBeenCalled()
    fireEvent.click(within(openRowMenu('周报')).getByRole('menuitem', { name: '删除…' }))
    await waitFor(() => expect(store.get('chat_a')).toBeNull())
  })

  it('deleting a group asks only when it holds conversations, which go back to Recent', async () => {
    const { groups: [work] } = store.createGroup('工作')
    store.createGroup('空组')
    put('chat_a', '周报')
    store.organize('chat_a', { group_id: work.id })
    await showList()
    await within(list()).findByRole('group', { name: '空组' })
    const confirm = vi.spyOn(window, 'confirm').mockReturnValueOnce(false).mockReturnValueOnce(true)

    fireEvent.click(within(list()).getByRole('button', { name: '分组「空组」的操作' }))
    fireEvent.click(within(list()).getByRole('menuitem', { name: '删除分组…' }))
    await waitFor(() => expect(within(list()).queryByRole('group', { name: '空组' })).toBeNull())
    expect(confirm).not.toHaveBeenCalled()

    fireEvent.click(within(list()).getByRole('button', { name: '分组「工作」的操作' }))
    fireEvent.click(within(list()).getByRole('menuitem', { name: '删除分组…' }))
    expect(confirm).toHaveBeenLastCalledWith('删除分组「工作」？里面的 1 个对话会回到「最近」，不会删除。')
    expect(store.groups().groups).toHaveLength(1)
    fireEvent.click(within(list()).getByRole('button', { name: '分组「工作」的操作' }))
    fireEvent.click(within(list()).getByRole('menuitem', { name: '删除分组…' }))
    await waitFor(() => expect(within(list()).queryByRole('group')).toBeNull())
    expect(row('周报')).toBeInTheDocument()
    expect(store.get('chat_a')).not.toBeNull()
  })

  it('renames a group from its menu or a double click', async () => {
    store.createGroup('工作')
    put('chat_a', '周报')
    await showList()
    await within(list()).findByRole('group', { name: '工作' })
    fireEvent.click(within(list()).getByRole('button', { name: '分组「工作」的操作' }))
    fireEvent.click(within(list()).getByRole('menuitem', { name: '重命名分组' }))
    let box = within(list()).getByRole('textbox', { name: '分组名称' })
    expect(box).toHaveValue('工作')
    fireEvent.change(box, { target: { value: '项目' } })
    fireEvent.keyDown(box, { key: 'Enter' })
    await within(list()).findByRole('group', { name: '项目' })
    // Left as it opened: nothing is renamed.
    fireEvent.doubleClick(within(groupHead('项目')).getByText('项目'))
    fireEvent.blur(within(list()).getByRole('textbox', { name: '分组名称' }))
    await new Promise(r => setTimeout(r, 0))
    expect(groupsBridge.rename).toHaveBeenCalledTimes(1)
    fireEvent.doubleClick(within(groupHead('项目')).getByText('项目'))
    box = within(list()).getByRole('textbox', { name: '分组名称' })
    fireEvent.change(box, { target: { value: '项目二' } })
    fireEvent.blur(box)
    await waitFor(() => expect(store.groups().groups[0].name).toBe('项目二'))
  })

  it('a search lists the matches flat, each tagged with its group', async () => {
    const { groups: [work] } = store.createGroup('工作')
    for (let i = 0; i < 9; i++) put(`chat_${i}`, `话题${i}`, i + 1)
    store.organize('chat_3', { group_id: work.id })
    await showList()
    fireEvent.change(await within(list()).findByRole('searchbox', { name: '搜索对话' }), { target: { value: '话题3' } })
    expect(within(list()).queryByRole('group')).toBeNull()
    expect(row('话题3 工作')).toBeInTheDocument()
  })

  it('a query dropped with the search box does not come back with it (Low U-12)', async () => {
    for (let i = 0; i < 9; i++) put(`chat_${i}`, `话题${i}`, i + 1)
    await showList()
    fireEvent.change(await within(list()).findByRole('searchbox', { name: '搜索对话' }), { target: { value: '话题8' } })
    vi.spyOn(window, 'confirm').mockReturnValueOnce(true)
    fireEvent.click(within(openRowMenu('话题8')).getByRole('menuitem', { name: '删除…' }))
    await waitFor(() => expect(within(list()).queryByRole('searchbox')).toBeNull())
    await act(async () => { await saveConversation(record('chat_9', '话题9', '2026-11-08T12:00:00.000Z')) })
    expect(within(list()).getByRole('searchbox', { name: '搜索对话' })).toHaveValue('')
    expect(within(list()).getAllByRole('button', { name: /^话题\d$/ })).toHaveLength(9)
  })
})

describe('focus and placement (UI-06 G2, Codex R1)', () => {
  it('picked from the keyboard, focus goes to the conversation\'s ⋯ where it now is', async () => {
    store.createGroup('工作')
    put('chat_a', '周报')
    await showList()
    await within(list()).findByRole('group', { name: '工作' })
    fireEvent.click(within(openRowMenu('周报')).getByRole('menuitem', { name: '工作' }), { detail: 0 })
    await waitFor(() => expect(within(groupBox('工作')).getByRole('button', { name: '对话「周报」的操作' })).toHaveFocus())
    // With the mouse, focus is left where the pointer put it.
    fireEvent.click(within(openRowMenu('周报')).getByRole('menuitem', { name: '移出分组' }), { detail: 1 })
    await waitFor(() => expect(groupOfStored('chat_a')).toBeNull())
    await new Promise(r => requestAnimationFrame(r))
    expect(within(list()).getByRole('button', { name: '对话「周报」的操作' })).not.toHaveFocus()
  })

  it('focus waits for a slow move: the ⋯ it goes to is the one in the row\'s new place', async () => {
    store.createGroup('工作')
    put('chat_a', '周报')
    await showList()
    await within(list()).findByRole('group', { name: '工作' })
    let land!: () => void
    bridge.organize.mockImplementationOnce((id: string, p: unknown) => {
      const answer = store.organize(id, p)
      return new Promise(r => { land = () => r(answer) })
    })
    fireEvent.click(within(openRowMenu('周报')).getByRole('menuitem', { name: '工作' }), { detail: 0 })
    for (let i = 0; i < 3; i++) await new Promise(r => requestAnimationFrame(r))
    land()
    await waitFor(() => expect(within(groupBox('工作')).getByRole('button', { name: '对话「周报」的操作' })).toHaveFocus())
  })

  it('focus the user has put elsewhere meanwhile is not taken back', async () => {
    store.createGroup('工作')
    put('chat_a', '周报')
    await showList()
    await within(list()).findByRole('group', { name: '工作' })
    let land!: () => void
    bridge.organize.mockImplementationOnce((id: string, p: unknown) => {
      const answer = store.organize(id, p)
      return new Promise(r => { land = () => r(answer) })
    })
    fireEvent.click(within(openRowMenu('周报')).getByRole('menuitem', { name: '工作' }), { detail: 0 })
    fireEvent.click(within(list()).getByRole('button', { name: '+ 分组' }))
    const box = within(list()).getByRole('textbox', { name: '分组名称' })
    fireEvent.change(box, { target: { value: '写到一半' } })
    land()
    await waitFor(() => expect(groupOfStored('chat_a')).not.toBeNull())
    for (let i = 0; i < 3; i++) await new Promise(r => requestAnimationFrame(r))
    expect(box).toHaveFocus()
    expect(store.groups().groups).toHaveLength(1)
  })

  it('a menu closes when the window is resized', async () => {
    put('chat_a', '周报')
    await showList()
    await within(list()).findByRole('button', { name: '周报' })
    openRowMenu('周报')
    fireEvent(window, new Event('resize'))
    expect(within(list()).queryByRole('menu')).toBeNull()
  })

  it('Escape in a menu opened by a right click focuses that row\'s ⋯', async () => {
    put('chat_a', '周报')
    await showList()
    fireEvent.contextMenu((await within(list()).findByRole('button', { name: '周报' })).closest('li')!, { clientX: 5, clientY: 5 })
    fireEvent.keyDown(within(list()).getByRole('menu'), { key: 'Escape' })
    await waitFor(() => expect(within(list()).getByRole('button', { name: '对话「周报」的操作' })).toHaveFocus())
  })

  it('a group\'s name box ended from the keyboard puts focus back: the new group\'s ⋯, or "+ 分组"', async () => {
    put('chat_a', '周报')
    await showList()
    fireEvent.click(await within(list()).findByRole('button', { name: '+ 分组' }))
    let box = within(list()).getByRole('textbox', { name: '分组名称' })
    fireEvent.change(box, { target: { value: '工作' } })
    fireEvent.keyDown(box, { key: 'Enter' })
    await waitFor(() => expect(within(list()).getByRole('button', { name: '分组「工作」的操作' })).toHaveFocus())
    fireEvent.click(within(list()).getByRole('button', { name: '+ 分组' }))
    fireEvent.keyDown(within(list()).getByRole('textbox', { name: '分组名称' }), { key: 'Escape' })
    await waitFor(() => expect(within(list()).getByRole('button', { name: '+ 分组' })).toHaveFocus())
    // Renamed from its menu: back to its ⋯.
    fireEvent.click(within(list()).getByRole('button', { name: '分组「工作」的操作' }))
    fireEvent.click(within(list()).getByRole('menuitem', { name: '重命名分组' }))
    box = within(list()).getByRole('textbox', { name: '分组名称' })
    fireEvent.change(box, { target: { value: '项目' } })
    fireEvent.keyDown(box, { key: 'Enter' })
    await waitFor(() => expect(within(list()).getByRole('button', { name: '分组「项目」的操作' })).toHaveFocus())
  })

  it('the entry under focus stays the same action when a group leaves the open menu', async () => {
    const { groups: [a] } = store.createGroup('甲')
    store.createGroup('乙')
    put('chat_a', '周报')
    await showList()
    await within(list()).findByRole('group', { name: '乙' })
    const menu = openRowMenu('周报')
    within(menu).getByRole('menuitem', { name: '乙' }).focus()
    store.deleteGroup(a.id)
    await act(async () => { await refreshGroups() })
    expect(within(list()).queryByRole('menuitem', { name: '甲' })).toBeNull()
    expect(document.activeElement).toHaveTextContent('乙')
  })

  it('a menu that would run past the window\'s bottom or right edge opens inside it', async () => {
    put('chat_a', '周报')
    await showList()
    const li = (await within(list()).findByRole('button', { name: '周报' })).closest('li')!
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({ width: 200, height: 300 } as DOMRect)
    fireEvent.contextMenu(li, { clientX: window.innerWidth - 50, clientY: window.innerHeight - 20 })
    expect(within(list()).getByRole('menu')).toHaveStyle({
      left: `${window.innerWidth - 204}px`, top: `${window.innerHeight - 304}px`,
    })
  })

  it('deleting a group whose conversations are all archived still asks', async () => {
    const { groups: [work] } = store.createGroup('工作')
    put('chat_a', '周报')
    put('chat_b', '闲聊')
    store.organize('chat_a', { group_id: work.id, archived: true })
    await showList()
    await within(list()).findByRole('group', { name: '工作' })
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false)
    fireEvent.click(within(list()).getByRole('button', { name: '分组「工作」的操作' }))
    fireEvent.click(within(list()).getByRole('menuitem', { name: '删除分组…' }))
    expect(confirm).toHaveBeenCalledWith('删除分组「工作」？里面的 1 个对话会回到「最近」，不会删除。')
    expect(store.groups().groups).toHaveLength(1)
  })
})

describe('dragging a conversation (UI-06 G2)', () => {
  const setUp = async () => {
    const { groups: [work] } = store.createGroup('工作')
    store.createGroup('生活')
    put('chat_a', '周报', 3)
    put('chat_b', '闲聊', 2)
    store.organize('chat_a', { group_id: work.id })
    await showList()
    await within(list()).findByRole('group', { name: '生活' })
    return work.id
  }
  const drag = (title: string) => {
    const li = row(title).closest('li')!
    carrying = row(title).dataset.historyId!
    const data = dt()
    fireEvent.dragStart(li, { dataTransfer: data })
    return { li, data }
  }
  /** Over a target: whether it offered the drop (cancelled the dragover). */
  const offers = (target: Element) => !fireEvent.dragOver(target, { dataTransfer: dt() })

  it('onto a group\'s heading moves it there, at once; the heading lights up while over it', async () => {
    await setUp()
    const { li, data } = drag('闲聊')
    expect(data.setData).toHaveBeenCalledWith('application/x-agent24-conversation', 'chat_b')
    expect(li).toHaveClass('dragging')
    expect(offers(groupHead('生活'))).toBe(true)
    expect(groupHead('生活')).toHaveClass('drop-over')
    // Moving onto the heading's own name is still over the heading.
    // (jsdom has no DragEvent to carry where the pointer went.)
    const leave = (to: Element) => fireEvent(groupHead('生活'), new MouseEvent('dragleave', { bubbles: true, relatedTarget: to }))
    leave(within(groupHead('生活')).getByText('生活'))
    expect(groupHead('生活')).toHaveClass('drop-over')
    leave(document.body)
    expect(groupHead('生活')).not.toHaveClass('drop-over')
    offers(groupHead('生活'))
    fireEvent.drop(groupHead('生活'), { dataTransfer: dt() })
    await waitFor(() => expect(groupOfStored('chat_b')).toBe(store.groups().groups[1].id))
    expect(within(groupBox('生活')).getByRole('button', { name: '闲聊' })).toBeInTheDocument()
    expect(groupHead('生活')).not.toHaveClass('drop-over')
  })

  it('onto "Recent" takes it out of its group', async () => {
    await setUp()
    drag('周报')
    expect(offers(recentHead())).toBe(true)
    fireEvent.drop(recentHead(), { dataTransfer: dt() })
    await waitFor(() => expect(groupOfStored('chat_a')).toBeNull())
    expect(within(groupBox('工作')).queryByRole('button', { name: '周报' })).toBeNull()
  })

  it('anywhere else does nothing: its own group, Recent when ungrouped, another row, after the drag ended', async () => {
    const work = await setUp()
    drag('周报')
    expect(offers(groupHead('工作'))).toBe(false)
    expect(offers(row('闲聊').closest('li')!)).toBe(false)
    // Not taken: the drop is left to the page, which does nothing with it.
    expect(fireEvent.drop(groupHead('工作'), { dataTransfer: dt() })).toBe(true)
    expect(fireEvent.drop(row('闲聊').closest('li')!, { dataTransfer: dt() })).toBe(true)
    fireEvent.dragEnd(row('周报').closest('li')!)
    expect(row('周报').closest('li')).not.toHaveClass('dragging')
    drag('闲聊')
    expect(offers(recentHead())).toBe(false)
    fireEvent.drop(recentHead(), { dataTransfer: dt() })
    fireEvent.dragEnd(row('闲聊').closest('li')!)
    // Nothing being dragged: a drop from elsewhere (a file, text) is not ours.
    expect(offers(groupHead('生活'))).toBe(false)
    fireEvent.drop(groupHead('生活'), { dataTransfer: dt() })
    await new Promise(r => setTimeout(r, 0))
    expect(bridge.organize).not.toHaveBeenCalled()
    expect(groupOfStored('chat_a')).toBe(work)
    expect(groupOfStored('chat_b')).toBeNull()
  })

  it('a file or text dragged in, or a drop carrying another conversation, does nothing', async () => {
    await setUp()
    drag('闲聊')
    // Something from elsewhere, while a drag of ours is (still) on record.
    expect(!fireEvent.dragOver(groupHead('生活'), { dataTransfer: dt('') })).toBe(false)
    expect(fireEvent.drop(groupHead('生活'), { dataTransfer: dt('') })).toBe(true)
    drag('闲聊')
    expect(fireEvent.drop(groupHead('生活'), { dataTransfer: dt('chat_a') })).toBe(true)
    await new Promise(r => setTimeout(r, 0))
    expect(bridge.organize).not.toHaveBeenCalled()
  })

  it('a drag whose row never heard its end is over at the next mouse press', async () => {
    await setUp()
    drag('闲聊')
    expect(row('闲聊').closest('li')).toHaveClass('dragging')
    fireEvent.mouseDown(document.body)
    expect(row('闲聊').closest('li')).not.toHaveClass('dragging')
    expect(offers(groupHead('生活'))).toBe(false)
    drag('闲聊')
    fireEvent(window, new Event('dragend'))
    expect(row('闲聊').closest('li')).not.toHaveClass('dragging')
  })

  it('a row being renamed is not draggable', async () => {
    await setUp()
    fireEvent.keyDown(row('闲聊'), { key: 'F2' })
    const box = within(list()).getByRole('textbox', { name: '对话标题' })
    expect(box.closest('li')).toHaveAttribute('draggable', 'false')
    expect(row('周报').closest('li')).toHaveAttribute('draggable', 'true')
  })
})

describe('where a drop goes (UI-06 G2)', () => {
  const groups = [{ id: 'grp_a', name: '甲', created_at: '2026-11-08T00:00:00.000Z', collapsed: false }]
  const item = (group_id: string | null, archived_at: string | null = null) => ({ group_id, archived_at })
  it.each([
    ['ungrouped onto a group', { kind: 'group', id: 'grp_a' }, item(null), { group_id: 'grp_a' }],
    ['grouped onto Recent', { kind: 'recent' }, item('grp_a'), { group_id: null }],
    ['onto its own group', { kind: 'group', id: 'grp_a' }, item('grp_a'), null],
    ['ungrouped onto Recent', { kind: 'recent' }, item(null), null],
    ['onto a group that is gone', { kind: 'group', id: 'grp_x' }, item(null), null],
    ['in a deleted group onto Recent', { kind: 'recent' }, item('grp_x'), null],
    ['in a deleted group onto a group', { kind: 'group', id: 'grp_a' }, item('grp_x'), { group_id: 'grp_a' }],
    ['onto the archive', { kind: 'archive' }, item('grp_a'), { archived: true }],
    ['archived onto the archive', { kind: 'archive' }, item(null, '2026-11-08T00:00:00.000Z'), null],
    ['archived onto a group', { kind: 'group', id: 'grp_a' }, item(null, '2026-11-08T00:00:00.000Z'), null],
  ] as const)('%s', (_name, target, it_, expected) => {
    expect(dropChange(target, it_ as never, groups)).toEqual(expected)
  })
})
