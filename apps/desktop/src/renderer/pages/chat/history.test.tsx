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
const chatHistory = { list: vi.fn(), get: vi.fn(), save: vi.fn(), delete: vi.fn(), clear: vi.fn() }
/** An in-memory store behind the bridge; reset before every test. */
function memoryStore() {
  chatHistory.list.mockReset().mockImplementation(async () => [...saved.values()]
    .map(r => ({ id: r.id, title: r.title, created_at: r.created_at, updated_at: r.updated_at }))
    .sort((a, b) => b.updated_at.localeCompare(a.updated_at)))
  chatHistory.get.mockReset().mockImplementation(async (id: string) => saved.get(id) ?? null)
  chatHistory.save.mockReset().mockImplementation(async (r: ChatHistoryRecord) => { saved.set(r.id, structuredClone(r)) })
  chatHistory.delete.mockReset().mockImplementation(async (id: string) => { saved.delete(id) })
  chatHistory.clear.mockReset().mockImplementation(async () => { saved.clear() })
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
  memoryStore()
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

  it('does not delete the open conversation or clear while its reply is in flight', async () => {
    saved.set('chat_a_1', stored('chat_a_1', '话题甲', '2026-11-07T10:00:02.000Z'))
    saved.set('chat_b_1', stored('chat_b_1', '话题乙', '2026-11-07T09:00:02.000Z'))
    let reply!: (v: unknown) => void
    proxy.mockReturnValueOnce(new Promise(r => { reply = r }))
    renderBoth()
    fireEvent.click(await within(await findHistoryList()).findByRole('button', { name: '话题甲' }))
    await waitFor(() => expect(screen.getByText('答：话题甲')).toBeInTheDocument())
    send('再问一句')
    await waitFor(() => expect(within(historyList()).getByRole('button', { name: '删除对话：话题甲' })).toBeDisabled())
    expect(within(historyList()).getByRole('button', { name: '清空' })).toBeDisabled()
    expect(within(historyList()).getByRole('button', { name: '话题乙' })).toBeDisabled()
    expect(within(historyList()).getByRole('button', { name: '删除对话：话题乙' })).toBeEnabled()

    reply({ ok: true, status: 200, data: { message: { content: '后来的答' } } })
    await waitFor(() => expect(saved.get('chat_a_1')?.messages.at(-1)?.content).toBe('后来的答'))
    await waitFor(() => expect(within(historyList()).getByRole('button', { name: '删除对话：话题甲' })).toBeEnabled())
  })

  it('a load that finishes after "New chat" is dropped', async () => {
    saved.set('chat_a_1', stored('chat_a_1', '话题甲', '2026-11-07T10:00:02.000Z'))
    let load!: (v: ChatHistoryRecord | null) => void
    chatHistory.get.mockImplementationOnce(() => new Promise(r => { load = r }))
    renderBoth()
    fireEvent.click(await within(await findHistoryList()).findByRole('button', { name: '话题甲' }))
    fireEvent.click(screen.getByRole('button', { name: '新对话' }))
    load(saved.get('chat_a_1')!)
    await new Promise(r => setTimeout(r, 0))
    expect(screen.queryByText('答：话题甲')).toBeNull()
    send('新话题')
    await waitFor(() => expect(proxy).toHaveBeenCalledTimes(1))
    expect(proxy.mock.calls[0][0].body.session_id).not.toBe('chat_a_1')
    expect(proxy.mock.calls[0][0].body.messages).toEqual([{ role: 'user', content: '新话题' }])
  })

  it('a save finishing after "New chat" does not mark the old conversation open again', async () => {
    const pending: Array<() => void> = []
    chatHistory.save.mockImplementation(async (r: ChatHistoryRecord) => {
      await new Promise<void>(done => pending.push(done))
      saved.set(r.id, structuredClone(r))
    })
    renderBoth()
    send('话题丙')
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    fireEvent.click(screen.getByRole('button', { name: '新对话' }))
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), { target: { value: '草稿' } })
    pending.forEach(done => done())
    const item = await within(await findHistoryList()).findByRole('button', { name: '话题丙' })
    expect(item).not.toHaveAttribute('aria-current')
    // Deleting it now leaves the new chat and its draft alone.
    fireEvent.click(within(historyList()).getByRole('button', { name: '删除对话：话题丙' }))
    await waitFor(() => expect(saved.size).toBe(0))
    expect(screen.getByPlaceholderText(/输入消息/)).toHaveValue('草稿')
  })

  it('retries a failed save of the finished reply without another change', async () => {
    renderBoth()
    send('一')
    await waitFor(() => expect(chatHistory.save).toHaveBeenCalledTimes(1))
    // The save holding the reply fails once, then succeeds on its own retry.
    chatHistory.save.mockRejectedValueOnce(new Error('ENOSPC'))
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    await waitFor(() => expect([...saved.values()][0]?.messages).toHaveLength(2), { timeout: 2000 })
  })

  it('a failed older save, retried late, does not overwrite the newer one with the reply', async () => {
    renderBoth()
    // The prompt-only save fails; the reply's save then succeeds before the retry.
    chatHistory.save.mockRejectedValueOnce(new Error('ENOSPC'))
    send('一')
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    await waitFor(() => expect([...saved.values()][0]?.messages).toHaveLength(2))
    await new Promise(r => setTimeout(r, 700))
    expect([...saved.values()][0]?.messages).toHaveLength(2)
    expect(chatHistory.save).toHaveBeenCalledTimes(2)
  })

  it('deleting another conversation does not cancel a retry', async () => {
    saved.set('chat_b_1', stored('chat_b_1', '话题乙', '2026-11-07T09:00:02.000Z'))
    renderBoth()
    await within(await findHistoryList()).findByRole('button', { name: '话题乙' })
    send('一')
    await waitFor(() => expect(chatHistory.save).toHaveBeenCalledTimes(1))
    chatHistory.save.mockRejectedValueOnce(new Error('ENOSPC'))
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    fireEvent.click(within(historyList()).getByRole('button', { name: '删除对话：话题乙' }))
    await waitFor(() => expect(saved.has('chat_b_1')).toBe(false))
    await waitFor(() => expect([...saved.values()][0]?.messages).toHaveLength(2), { timeout: 2000 })
  })

  it('a retry still waiting gives up once the conversation is deleted', async () => {
    saved.set('chat_a_1', stored('chat_a_1', '话题甲', '2026-11-07T10:00:02.000Z'))
    renderBoth()
    fireEvent.click(await within(await findHistoryList()).findByRole('button', { name: '话题甲' }))
    await waitFor(() => expect(screen.getByText('答：话题甲')).toBeInTheDocument())
    chatHistory.save.mockRejectedValueOnce(new Error('ENOSPC'))
    send('再问')
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    await waitFor(() => expect(within(historyList()).getByRole('button', { name: '删除对话：话题甲' })).toBeEnabled())
    fireEvent.click(within(historyList()).getByRole('button', { name: '删除对话：话题甲' }))
    // The page left it at once, before the deletion settled.
    expect(screen.queryByText('答：话题甲')).toBeNull()
    await waitFor(() => expect(saved.has('chat_a_1')).toBe(false))
    await new Promise(r => setTimeout(r, 2200))
    expect(saved.has('chat_a_1')).toBe(false)
  })

  it('works without the history bridge: the chat goes on, nothing is listed', async () => {
    Object.defineProperty(window, 'agent24', { value: { backendProxy: proxy }, writable: true, configurable: true })
    renderBoth()
    send('你好')
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    expect(screen.queryByRole('region', { name: '历史对话' })).toBeNull()
  })
})
