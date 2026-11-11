// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, fireEvent, waitFor, within } from '@testing-library/react'
import ChatPage from '../Chat'
import { ChatHistoryList } from './ChatHistoryList'
import { resetHistoryStore } from './history'
import { setLocale } from '../../i18n'
import { chatHistorySummary, type ChatHistoryRecord } from '../../../shared/chat-history'

// UI-04 T2: the model titles a new conversation once, in the background.

type Req = { method: string; path: string; body?: { session_id?: string; idoris_headroom_confirmed?: boolean; messages?: Array<{ role: string; content: string }> } }
const isTitle = (req: Req) => req.body?.messages?.[0]?.role === 'system'

const proxy = vi.fn()
/** What a title request answers; the chat itself always answers `ack`. */
let titleAnswer: (req: Req) => Promise<unknown>
const titleCalls = () => proxy.mock.calls.map(c => c[0] as Req).filter(isTitle)

let saved: Map<string, ChatHistoryRecord>
const RANK = { prompt: 0, model: 1, schedule: 2, assistant: 2, user: 3 } as const
const summary = chatHistorySummary
/** Behaves as main/chat-history.ts does: a stronger title on file wins. */
const chatHistory = {
  list: vi.fn(async () => [...saved.values()].map(summary).sort((a, b) => b.updated_at.localeCompare(a.updated_at))),
  get: vi.fn(async (id: string) => saved.get(id) ?? null),
  save: vi.fn(async (r: ChatHistoryRecord) => {
    const before = saved.get(r.id)
    const record = before && RANK[before.title_source] > RANK[r.title_source]
      ? { ...structuredClone(r), title: before.title, title_source: before.title_source }
      : structuredClone(r)
    saved.set(r.id, record)
    return summary(record)
  }),
  delete: vi.fn(async (id: string) => { saved.delete(id) }),
  clear: vi.fn(async () => { saved.clear() }),
  setTitle: vi.fn(async (id: string, title: string, source: 'user' | 'model') => {
    const before = saved.get(id)
    if (!before) return null
    if (RANK[before.title_source] > RANK[source]) return summary(before)
    saved.set(id, { ...before, title, title_source: source })
    return summary(saved.get(id)!)
  }),
}

const answer = (content: string) => async () => ({ ok: true, status: 200, data: { message: { content } } })
const renderBoth = () => render(<><ChatHistoryList showChat={() => {}} /><ChatPage /></>)
const send = (text: string) => {
  const box = screen.getByPlaceholderText(/输入消息/)
  fireEvent.change(box, { target: { value: text } })
  fireEvent.keyDown(box, { key: 'Enter', shiftKey: false })
}
/** Sends a prompt and waits for its reply and the page to settle. */
const turn = async (text: string, n: number) => {
  send(text)
  await waitFor(() => expect(screen.getAllByText('ack')).toHaveLength(n))
  await new Promise(r => setTimeout(r, 0))
}
const historyList = () => screen.getByRole('region', { name: '历史对话' })
/** UI-06 G2: renaming is in a conversation's ⋯ menu. */
const renameFromMenu = async (title: string) => {
  fireEvent.click(await within(historyList()).findByRole('button', { name: `对话「${title}」的操作` }))
  fireEvent.click(within(historyList()).getByRole('menuitem', { name: '重命名' }))
}
const sessionId = () => (proxy.mock.calls.map(c => c[0] as Req).find(r => !isTitle(r))!.body!.session_id)!

beforeEach(() => {
  setLocale('zh')
  vi.clearAllMocks()
  resetHistoryStore()
  saved = new Map()
  titleAnswer = answer('周报草稿')
  proxy.mockImplementation((req: Req) => isTitle(req)
    ? titleAnswer(req)
    : Promise.resolve({ ok: true, status: 200, data: { message: { content: 'ack' } } }))
  Object.defineProperty(window, 'agent24', { value: { backendProxy: proxy, chatHistory }, writable: true, configurable: true })
})
afterEach(() => { vi.restoreAllMocks() })

describe('model titles (UI-04 T2)', () => {
  it('asks once after the first answer, without session_id, and lists the title', async () => {
    renderBoth()
    await turn('帮我写一份本周的周报', 1)
    await within(historyList()).findByRole('button', { name: '周报草稿' })
    const id = sessionId()
    expect(saved.get(id)).toMatchObject({ title: '周报草稿', title_source: 'model' })
    expect(titleCalls()).toHaveLength(1)
    const req = titleCalls()[0]
    expect(req).toMatchObject({ method: 'POST', path: '/api/v1/chat' })
    expect(req.body).not.toHaveProperty('session_id')
    expect(req.body).not.toHaveProperty('idoris_headroom_confirmed')
    expect(req.body!.messages![1].content).toBe('User: 帮我写一份本周的周报\nAssistant: ack')
    // Later turns ask for nothing, and their saves keep the model's title.
    await turn('再加一段总结', 2)
    await turn('好了', 3)
    expect(titleCalls()).toHaveLength(1)
    expect(saved.get(id)).toMatchObject({ title: '周报草稿', title_source: 'model' })
    expect(saved.get(id)!.messages).toHaveLength(6)
  })

  it('waits while the prompts are too short, and asks by the third answer at the latest', async () => {
    renderBoth()
    await turn('你好', 1)
    expect(titleCalls()).toHaveLength(0)
    await turn('在吗', 2)
    expect(titleCalls()).toHaveLength(0)
    await turn('嗯', 3)
    await waitFor(() => expect(titleCalls()).toHaveLength(1))
    expect(titleCalls()[0].body!.messages![1].content).toContain('User: 你好\nAssistant: ack\n\nUser: 在吗')
    await turn('写周报', 4)
    expect(titleCalls()).toHaveLength(1)
  })

  it('a rename made while the model writes wins over its late title', async () => {
    let release!: () => void
    titleAnswer = () => new Promise(r => { release = () => r({ ok: true, status: 200, data: { message: { content: '迟到的模型标题' } } }) })
    renderBoth()
    await turn('帮我写一份本周的周报', 1)
    await waitFor(() => expect(titleCalls()).toHaveLength(1))
    await renameFromMenu('帮我写一份本周的周报')
    const input = within(historyList()).getByRole('textbox', { name: '对话标题' })
    fireEvent.change(input, { target: { value: '我的周报' } })
    fireEvent.keyDown(input, { key: 'Enter' })
    await within(historyList()).findByRole('button', { name: '我的周报' })
    release()
    await waitFor(() => expect(chatHistory.setTitle).toHaveBeenCalledWith(sessionId(), '迟到的模型标题', 'model'))
    await new Promise(r => setTimeout(r, 0))
    expect(saved.get(sessionId())).toMatchObject({ title: '我的周报', title_source: 'user' })
    expect(within(historyList()).getByRole('button', { name: '我的周报' })).toBeInTheDocument()
    expect(within(historyList()).queryByRole('button', { name: '迟到的模型标题' })).toBeNull()
  })

  it('does not ask at all once the user has renamed the conversation', async () => {
    renderBoth()
    await turn('你好', 1)
    await renameFromMenu('你好')
    const input = within(historyList()).getByRole('textbox', { name: '对话标题' })
    fireEvent.change(input, { target: { value: '闲聊' } })
    fireEvent.keyDown(input, { key: 'Enter' })
    await within(historyList()).findByRole('button', { name: '闲聊' })
    await turn('帮我写一份本周的周报', 2)
    expect(titleCalls()).toHaveLength(0)
  })

  it.each([
    ['no local model', async () => ({ ok: false, status: 503, data: { error: { code: 'provider_unavailable' } } })],
    ['a headroom confirmation', async () => ({ ok: false, status: 409, data: { error: { code: 'idoris_headroom_confirmation_required' } } })],
    ['a broken bridge', async () => { throw new Error('ipc closed') }],
    ['a failed answer that carries text', async () => ({ ok: false, status: 500, data: { message: { content: '服务出错页' } } })],
    ['only thinking', answer('<think>想了很久，被截断')],
    ['an empty answer', answer('  ')],
  ])('keeps the prompt\'s title, and asks no more, after %s', async (_name, fails) => {
    titleAnswer = fails
    renderBoth()
    await turn('帮我写一份本周的周报', 1)
    await waitFor(() => expect(titleCalls()).toHaveLength(1))
    await turn('再加一段总结', 2)
    expect(titleCalls()).toHaveLength(1)
    expect(chatHistory.setTitle).not.toHaveBeenCalled()
    expect(saved.get(sessionId())).toMatchObject({ title: '帮我写一份本周的周报', title_source: 'prompt' })
    expect(titleCalls().some(r => r.body?.idoris_headroom_confirmed)).toBe(false)
  })

  it('a conversation opened from the history asks for none; a new chat asks for its own', async () => {
    saved.set('chat_old_1', {
      format: 'agent24.desktop.chat-history', version: 3, id: 'chat_old_1', title: '旧', title_source: 'prompt', group_id: null, archived_at: null, runs: [], pending: [], initiator: 'user', unseen: [],
      created_at: '2026-11-07T10:00:00.000Z', updated_at: '2026-11-07T10:00:01.000Z',
      messages: [{ role: 'user', content: '旧', at: '2026-11-07T10:00:00.000Z', delivered: true }],
    })
    renderBoth()
    fireEvent.click(await within(await screen.findByRole('region', { name: '历史对话' })).findByRole('button', { name: '旧' }))
    // Shown in the chat as well as in the list.
    await waitFor(() => expect(screen.getAllByText('旧').length).toBeGreaterThan(1))
    await turn('接着聊聊周报怎么写', 1)
    await turn('再写长一点吧', 2)
    expect(titleCalls()).toHaveLength(0)
    fireEvent.click(screen.getByRole('button', { name: /新对话/ }))
    await turn('另起一个话题：请假流程', 1)
    await waitFor(() => expect(titleCalls()).toHaveLength(1))
    expect(titleCalls()[0].body!.messages![1].content).toContain('另起一个话题')
  })

  it('without the history bridge, never asks', async () => {
    Object.defineProperty(window, 'agent24', { value: { backendProxy: proxy }, writable: true, configurable: true })
    renderBoth()
    await turn('帮我写一份本周的周报', 1)
    expect(titleCalls()).toHaveLength(0)
  })
})
