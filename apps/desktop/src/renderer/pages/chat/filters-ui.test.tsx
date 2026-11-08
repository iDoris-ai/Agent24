// @vitest-environment jsdom
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { createChatHistoryStore } from '../../../main/chat-history'
import { CHAT_HISTORY_FORMAT, CHAT_HISTORY_VERSION, type ChatHistoryRecord } from '../../../shared/chat-history'
import type { RunEvent } from '../../../shared/ipc-types'
import { ChatHistoryList } from './ChatHistoryList'
import { refreshHistory, resetHistoryStore } from './history'
import { resetRunStatusesForTests } from './run-status'
import { setLocale } from '../../i18n'

// M4 A3: the four filters of the history list, against main's real store and
// a daemon whose runs' statuses the test sets, with its /events.

let dir: string
let store: ReturnType<typeof createChatHistoryStore>
const wrap = <A extends unknown[], R>(f: (...a: A) => R) => vi.fn(async (...a: A) => f(...a))
/** The daemon's runs: their status, or absent (404). */
let runs: Record<string, string>
let proxy: ReturnType<typeof vi.fn>
let emit: (event: RunEvent) => void

function record(id: string, prompt: string, hour: number): ChatHistoryRecord {
  const at = `2026-11-09T${String(hour).padStart(2, '0')}:00:00.000Z`
  return {
    format: CHAT_HISTORY_FORMAT, version: CHAT_HISTORY_VERSION, id, title: prompt, title_source: 'prompt',
    group_id: null, archived_at: null, created_at: at, updated_at: at, runs: [], pending: [],
    messages: [{ role: 'user', content: prompt, at, delivered: true }],
  }
}

beforeEach(() => {
  setLocale('zh')
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'filters-ui-'))
  store = createChatHistoryStore(dir)
  runs = {}
  proxy = vi.fn(async (req: { method: string; path: string }) => {
    const id = decodeURIComponent(req.path.replace('/api/v1/runs/', ''))
    return id in runs ? { ok: true, status: 200, data: { id, status: runs[id] } } : { ok: false, status: 404, data: null }
  })
  const listeners = new Set<(e: RunEvent) => void>()
  emit = event => { for (const l of listeners) l(event) }
  const bridge = {
    list: wrap(() => store.list()), get: wrap((id: string) => store.get(id)), save: wrap((r: unknown) => store.save(r)),
    delete: wrap((id: string) => store.delete(id)), clear: wrap(() => store.clear()),
    organize: wrap((id: string, p: unknown) => store.organize(id, p)),
  }
  const chatGroups = { get: wrap(() => store.groups()) }
  Object.defineProperty(window, 'agent24', {
    value: {
      chatHistory: bridge, chatGroups, backendProxy: proxy,
      onRunEvent: (cb: (e: RunEvent) => void) => { listeners.add(cb); return () => listeners.delete(cb) },
    },
    writable: true, configurable: true,
  })
  resetHistoryStore()
  resetRunStatusesForTests()
})
afterEach(() => { vi.restoreAllMocks(); fs.rmSync(dir, { recursive: true, force: true }) })

const list = () => screen.getByRole('region', { name: '历史对话' })
const filters = () => within(list()).getByRole('group', { name: '筛选' })
const filter = (name: RegExp) => within(filters()).getByRole('button', { name })
const shownTitles = () => within(list()).queryAllByRole('button').map(b => b.textContent).filter(t => ['周报', '日报', '闲聊', '旧事'].includes(t ?? ''))
const showList = async () => {
  await refreshHistory()
  render(<ChatHistoryList showChat={() => {}} />)
  await screen.findByRole('region', { name: '历史对话' })
}

describe('the history list\'s four filters (M4 A3)', () => {
  it('shows no filters while no conversation has a task or waits on the user', async () => {
    store.save(record('chat_a', '闲聊', 1))
    await showList()
    expect(within(list()).queryByRole('group', { name: '筛选' })).toBeNull()
  })

  it('counts each filter from the daemon\'s status of each task and what waits on the user', async () => {
    store.save(record('chat_a', '周报', 4))
    store.save(record('chat_b', '日报', 3))
    store.save(record('chat_c', '闲聊', 2))
    store.promoteRun('chat_a', 'run_a')
    store.promoteRun('chat_b', 'run_b')
    store.setPending('chat_c', { id: 'ask_1', kind: 'confirm', label: '加进对话？', at: '2026-11-09T02:00:00.000Z' })
    runs = { run_a: 'running', run_b: 'completed' }
    await showList()
    await waitFor(() => expect(filter(/^进行中/)).toHaveTextContent('进行中(1)'))
    expect(filter(/^待我处理/)).toHaveTextContent('待我处理(1)')
    expect(filter(/^已完成/)).toHaveTextContent('已完成(1)')
    expect(filter(/^全部/)).toHaveAttribute('aria-pressed', 'true')
    expect(proxy).toHaveBeenCalledTimes(2)
  })

  it('shows a filter\'s conversations flat, searched too, and goes back to the groups under "all"', async () => {
    for (let i = 0; i < 9; i++) store.save(record(`chat_x${i}`, `其他${i}`, 1))
    store.save(record('chat_a', '周报', 4))
    store.save(record('chat_b', '日报', 3))
    store.promoteRun('chat_a', 'run_a')
    store.promoteRun('chat_b', 'run_b')
    runs = { run_a: 'running', run_b: 'running' }
    await showList()
    await waitFor(() => expect(filter(/^进行中/)).toHaveTextContent('进行中(2)'))
    fireEvent.click(filter(/^进行中/))
    expect(filter(/^进行中/)).toHaveAttribute('aria-pressed', 'true')
    expect(shownTitles()).toEqual(['周报', '日报'])
    expect(within(list()).queryByRole('button', { name: '其他0' })).toBeNull()
    fireEvent.change(within(list()).getByRole('searchbox', { name: '搜索对话' }), { target: { value: '日' } })
    expect(shownTitles()).toEqual(['日报'])
    fireEvent.click(filter(/^待我处理/))
    expect(within(list()).getByRole('status')).toHaveTextContent('没有匹配的对话')
    fireEvent.change(within(list()).getByRole('searchbox', { name: '搜索对话' }), { target: { value: '' } })
    expect(within(list()).getByRole('status')).toHaveTextContent('这里没有对话')
    fireEvent.click(filter(/^全部/))
    expect(within(list()).getByRole('button', { name: '其他0' })).toBeInTheDocument()
  })

  it('keeps the counts up to date from the daemon\'s events, asking again only the run that changed', async () => {
    store.save(record('chat_a', '周报', 4))
    store.promoteRun('chat_a', 'run_a')
    runs = { run_a: 'running' }
    await showList()
    await waitFor(() => expect(filter(/^进行中/)).toHaveTextContent('进行中(1)'))
    runs = { run_a: 'awaiting_approval' }
    act(() => emit({ type: 'approval.required', run_id: 'run_a' }))
    await waitFor(() => expect(filter(/^待我处理/)).toHaveTextContent('待我处理(1)'))
    expect(filter(/^进行中/)).toHaveTextContent(/^进行中$/)
    runs = { run_a: 'completed' }
    act(() => emit({ type: 'run.completed', run_id: 'run_a' }))
    await waitFor(() => expect(filter(/^已完成/)).toHaveTextContent('已完成(1)'))
    // A run the list does not show is not asked about.
    const asked = proxy.mock.calls.length
    act(() => emit({ type: 'run.completed', run_id: 'run_other' }))
    await act(async () => {})
    expect(proxy).toHaveBeenCalledTimes(asked)
  })

  it('leaves archived conversations out of every filter but "all"', async () => {
    store.save(record('chat_a', '周报', 4))
    store.save(record('chat_d', '旧事', 3))
    store.promoteRun('chat_a', 'run_a')
    store.promoteRun('chat_d', 'run_d')
    store.organize('chat_d', { archived: true })
    runs = { run_a: 'running', run_d: 'running' }
    await showList()
    await waitFor(() => expect(filter(/^进行中/)).toHaveTextContent('进行中(1)'))
    fireEvent.click(filter(/^进行中/))
    expect(shownTitles()).toEqual(['周报'])
    // Its run is not even asked about.
    expect(proxy).toHaveBeenCalledTimes(1)
  })

  it('counts a task the daemon does not have, or could not tell, nowhere but "all"', async () => {
    store.save(record('chat_a', '周报', 4))
    store.promoteRun('chat_a', 'run_gone')
    await showList()
    await waitFor(() => expect(proxy).toHaveBeenCalled())
    await act(async () => {})
    for (const name of [/^进行中/, /^待我处理/, /^已完成/]) expect(filter(name).textContent).not.toMatch(/\(/)
  })

  it('is reached by keyboard: each filter is a button that says whether it is on', async () => {
    store.save(record('chat_a', '周报', 4))
    store.promoteRun('chat_a', 'run_a')
    runs = { run_a: 'running' }
    await showList()
    const active = await waitFor(() => filter(/^进行中/))
    active.focus()
    expect(active).toHaveFocus()
    expect(active.tagName).toBe('BUTTON')
    expect(active).toHaveAttribute('aria-pressed', 'false')
  })
})
