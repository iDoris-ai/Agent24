// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, fireEvent, waitFor, within } from '@testing-library/react'
import ChatPage from '../Chat'
import { ChatHistoryList } from './ChatHistoryList'
import { resetHistoryStore } from './history'
import { setLocale } from '../../i18n'
import type { ChatHistoryRecord } from '../../../shared/chat-history'

const proxy = vi.fn()
let saved: Map<string, ChatHistoryRecord>
const chatHistory = {
  list: vi.fn(async () => [...saved.values()]
    .map(r => ({ id: r.id, title: r.title, created_at: r.created_at, updated_at: r.updated_at }))
    .sort((a, b) => b.updated_at.localeCompare(a.updated_at))),
  get: vi.fn(async (id: string) => saved.get(id) ?? null),
  save: vi.fn(async (r: ChatHistoryRecord) => { saved.set(r.id, structuredClone(r)) }),
  delete: vi.fn(async (id: string) => { saved.delete(id) }),
  clear: vi.fn(async () => { saved.clear() }),
}

function stored(id: string, title: string, updated: string): ChatHistoryRecord {
  return {
    format: 'agent24.desktop.chat-history', version: 1, id, title,
    created_at: '2026-11-07T10:00:00.000Z', updated_at: updated,
    messages: [
      { role: 'user', content: title, at: '2026-11-07T10:00:00.000Z', delivered: true },
      { role: 'assistant', content: `答：${title}`, at: '2026-11-07T10:00:01.000Z', run_id: null,
        latency: { modelName: 'qwen', tier: 'local', firstTokenMs: null, totalMs: 1200 } },
      { role: 'user', content: '没发出去的', at: '2026-11-07T10:00:02.000Z', delivered: false },
    ],
  }
}

const showChat = vi.fn()
function renderBoth() {
  return render(<><ChatHistoryList showChat={showChat} /><ChatPage /></>)
}
const send = (text: string) => {
  const box = screen.getByPlaceholderText(/输入消息/)
  fireEvent.change(box, { target: { value: text } })
  fireEvent.keyDown(box, { key: 'Enter', shiftKey: false })
}
const historyList = () => screen.getByRole('region', { name: '历史对话' })
const findHistoryList = () => screen.findByRole('region', { name: '历史对话' })

beforeEach(() => {
  setLocale('zh')
  vi.clearAllMocks()
  resetHistoryStore()
  saved = new Map()
  proxy.mockResolvedValue({ ok: true, status: 200, data: { message: { content: 'ack' } } })
  Object.defineProperty(window, 'agent24', { value: { backendProxy: proxy, chatHistory }, writable: true, configurable: true })
})
afterEach(() => { vi.restoreAllMocks() })

describe('chat history', () => {
  it('saves each conversation under its session_id, and lists it', async () => {
    renderBoth()
    expect(screen.queryByRole('region', { name: '历史对话' })).toBeNull()
    send('第一个问题')
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    const sessionId = proxy.mock.calls[0][0].body.session_id
    await waitFor(() => expect(saved.get(sessionId)?.messages).toHaveLength(2))
    const record = saved.get(sessionId)!
    expect(record).toMatchObject({ format: 'agent24.desktop.chat-history', version: 1, id: sessionId, title: '第一个问题' })
    expect(record.messages[0]).toMatchObject({ role: 'user', content: '第一个问题', delivered: true })
    expect(record.messages[1]).toMatchObject({ role: 'assistant', content: 'ack', run_id: null })
    expect(record.messages.every(m => /Z$/.test(m.at))).toBe(true)
    const item = await within(await screen.findByRole('region', { name: '历史对话' })).findByRole('button', { name: '第一个问题' })
    expect(item).toHaveAttribute('aria-current', 'true')
  })

  it('opens a saved conversation and continues it under the same session_id', async () => {
    saved.set('chat_old_1', stored('chat_old_1', '旧话题', '2026-11-07T10:00:02.000Z'))
    renderBoth()
    fireEvent.click(await within(await findHistoryList()).findByRole('button', { name: '旧话题' }))
    expect(showChat).toHaveBeenCalled()
    await waitFor(() => expect(screen.getByText('答：旧话题')).toBeInTheDocument())
    expect(screen.getByText('未发送')).toBeInTheDocument()
    // Opening alone writes nothing.
    expect(chatHistory.save).not.toHaveBeenCalled()

    send('接着问')
    await waitFor(() => expect(proxy).toHaveBeenCalledTimes(1))
    const body = proxy.mock.calls[0][0].body
    expect(body.session_id).toBe('chat_old_1')
    // The prompt that was never sent is not sent now as context either.
    expect(body.messages).toEqual([
      { role: 'user', content: '旧话题' },
      { role: 'assistant', content: '答：旧话题' },
      { role: 'user', content: '接着问' },
    ])
    await waitFor(() => expect(saved.get('chat_old_1')?.messages).toHaveLength(5))
  })

  it('deleting the open conversation starts a new chat; deleting another keeps it', async () => {
    saved.set('chat_a_1', stored('chat_a_1', '话题甲', '2026-11-07T10:00:02.000Z'))
    saved.set('chat_b_1', stored('chat_b_1', '话题乙', '2026-11-07T11:00:02.000Z'))
    renderBoth()
    fireEvent.click(await within(await findHistoryList()).findByRole('button', { name: '话题甲' }))
    await waitFor(() => expect(screen.getByText('答：话题甲')).toBeInTheDocument())

    fireEvent.click(within(historyList()).getByRole('button', { name: '删除对话：话题乙' }))
    await waitFor(() => expect(within(historyList()).queryByRole('button', { name: '话题乙' })).toBeNull())
    expect(screen.getByText('答：话题甲')).toBeInTheDocument()

    fireEvent.click(within(historyList()).getByRole('button', { name: '删除对话：话题甲' }))
    await waitFor(() => expect(screen.queryByText('答：话题甲')).toBeNull())
    expect(saved.size).toBe(0)
    expect(screen.queryByRole('region', { name: '历史对话' })).toBeNull()
  })

  it('clears everything only after confirmation', async () => {
    saved.set('chat_a_1', stored('chat_a_1', '话题甲', '2026-11-07T10:00:02.000Z'))
    renderBoth()
    await within(await findHistoryList()).findByRole('button', { name: '话题甲' })
    const confirm = vi.spyOn(window, 'confirm').mockReturnValueOnce(false).mockReturnValueOnce(true)
    fireEvent.click(within(historyList()).getByRole('button', { name: '清空' }))
    expect(chatHistory.clear).not.toHaveBeenCalled()
    fireEvent.click(within(historyList()).getByRole('button', { name: '清空' }))
    await waitFor(() => expect(chatHistory.clear).toHaveBeenCalledTimes(1))
    expect(confirm).toHaveBeenCalledTimes(2)
    await waitFor(() => expect(screen.queryByRole('region', { name: '历史对话' })).toBeNull())
  })

  it('works without the history bridge: the chat goes on, nothing is listed', async () => {
    Object.defineProperty(window, 'agent24', { value: { backendProxy: proxy }, writable: true, configurable: true })
    renderBoth()
    send('你好')
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    expect(screen.queryByRole('region', { name: '历史对话' })).toBeNull()
  })
})
