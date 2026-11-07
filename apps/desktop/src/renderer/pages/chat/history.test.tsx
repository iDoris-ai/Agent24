// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, fireEvent, waitFor, within } from '@testing-library/react'
import ChatPage from '../Chat'
import { ChatHistoryList, shortTime } from './ChatHistoryList'
import { resetHistoryStore, setRetrySleepForTests } from './history'
import { setLocale } from '../../i18n'
import { chatHistorySummary, type ChatHistoryRecord } from '../../../shared/chat-history'

const proxy = vi.fn()
let saved: Map<string, ChatHistoryRecord>
const chatHistory = { list: vi.fn(), get: vi.fn(), save: vi.fn(), delete: vi.fn(), clear: vi.fn(), setTitle: vi.fn() }
const summary = chatHistorySummary
const RANK = { prompt: 0, model: 1, user: 2 } as const
/** Saves as main does: a stronger title on file is kept (main/chat-history.ts). */
function put(r: ChatHistoryRecord) {
  const before = saved.get(r.id)
  const record = before && RANK[before.title_source] > RANK[r.title_source]
    ? { ...structuredClone(r), title: before.title, title_source: before.title_source }
    : structuredClone(r)
  saved.set(r.id, record)
  return summary(record)
}
/** An in-memory store behind the bridge; reset before every test. */
function memoryStore() {
  chatHistory.list.mockReset().mockImplementation(async () => [...saved.values()]
    .map(summary)
    .sort((a, b) => b.updated_at.localeCompare(a.updated_at)))
  chatHistory.get.mockReset().mockImplementation(async (id: string) => saved.get(id) ?? null)
  chatHistory.save.mockReset().mockImplementation(async (r: ChatHistoryRecord) => put(r))
  chatHistory.delete.mockReset().mockImplementation(async (id: string) => { saved.delete(id) })
  chatHistory.clear.mockReset().mockImplementation(async () => { saved.clear() })
  chatHistory.setTitle.mockReset().mockImplementation(async (id: string, title: string, source: 'user' | 'model') => {
    const before = saved.get(id)
    if (!before) return null
    if (RANK[before.title_source] > RANK[source]) return summary(before)
    saved.set(id, { ...before, title, title_source: source })
    return summary(saved.get(id)!)
  })
}

function stored(id: string, title: string, updated: string): ChatHistoryRecord {
  return {
    format: 'agent24.desktop.chat-history', version: 1, id, title, title_source: 'prompt',
    created_at: '2026-11-07T10:00:00.000Z', updated_at: updated,
    messages: [
      { role: 'user', content: title, at: '2026-11-07T10:00:00.000Z', delivered: true },
      { role: 'assistant', content: `答：${title}`, at: '2026-11-07T10:00:01.000Z', run_id: null,
        latency: { modelName: 'qwen', tier: 'local', firstTokenMs: null, totalMs: 1200 } },
      { role: 'user', content: '没发出去的', at: '2026-11-07T10:00:02.000Z', delivered: false },
    ],
  }
}

/** Failed saves wait for `retry()` instead of a timer, so tests decide when
 *  a retry happens, however slow the queries before it are. */
let waiting: Array<() => void> = []
function holdRetries() {
  setRetrySleepForTests(() => new Promise<void>(go => { waiting.push(go) }))
}
async function retry() {
  await waitFor(() => expect(waiting.length).toBeGreaterThan(0))
  const go = waiting
  waiting = []
  go.forEach(g => g())
  await new Promise(r => setTimeout(r, 0))
}

/** These calls to save (1-based) fail; the others save as usual. */
function failSave(...nth: number[]) {
  let calls = 0
  chatHistory.save.mockImplementation(async (r: ChatHistoryRecord) => {
    calls += 1
    if (nth.includes(calls)) throw new Error('ENOSPC')
    return put(r)
  })
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
  waiting = []
  memoryStore()
  // A title request (UI-04 T2, tested in title-gen.test.tsx) finds no model
  // here, so the prompt's title stays; every other call is the chat's.
  proxy.mockImplementation(async (req: { body?: { messages?: Array<{ role: string }> } }) =>
    req.body?.messages?.[0]?.role === 'system'
      ? { ok: false, status: 503, data: { error: { code: 'provider_unavailable', message: 'no local model' } } }
      : { ok: true, status: 200, data: { message: { content: 'ack' } } })
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
    // Saves update the list themselves: the whole list is read once, at mount.
    expect(chatHistory.list).toHaveBeenCalledTimes(1)
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
    // The save holding the reply (the second) fails once, then succeeds on its own retry.
    failSave(2)
    holdRetries()
    renderBoth()
    send('一')
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    await waitFor(() => expect(chatHistory.save).toHaveBeenCalledTimes(2))
    expect(saved.size).toBe(1)
    expect([...saved.values()][0].messages).toHaveLength(1)
    await retry()
    await waitFor(() => expect([...saved.values()][0]?.messages).toHaveLength(2))
  })

  it('a failed older save, retried late, does not overwrite the newer one with the reply', async () => {
    // The prompt-only save fails; the reply's save then succeeds before the retry.
    failSave(1)
    holdRetries()
    renderBoth()
    send('一')
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    await waitFor(() => expect([...saved.values()][0]?.messages).toHaveLength(2))
    await retry()
    expect([...saved.values()][0]?.messages).toHaveLength(2)
    expect(chatHistory.save).toHaveBeenCalledTimes(2)
  })

  it('deleting another conversation does not cancel a retry', async () => {
    saved.set('chat_b_1', stored('chat_b_1', '话题乙', '2026-11-07T09:00:02.000Z'))
    failSave(2)
    holdRetries()
    renderBoth()
    await within(await findHistoryList()).findByRole('button', { name: '话题乙' })
    send('一')
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    await waitFor(() => expect(chatHistory.save).toHaveBeenCalledTimes(2))
    fireEvent.click(within(historyList()).getByRole('button', { name: '删除对话：话题乙' }))
    await waitFor(() => expect(saved.has('chat_b_1')).toBe(false))
    await retry()
    await waitFor(() => expect([...saved.values()][0]?.messages).toHaveLength(2))
  })

  it('a retry still waiting gives up once the conversation is deleted', async () => {
    saved.set('chat_a_1', stored('chat_a_1', '话题甲', '2026-11-07T10:00:02.000Z'))
    renderBoth()
    fireEvent.click(await within(await findHistoryList()).findByRole('button', { name: '话题甲' }))
    await waitFor(() => expect(screen.getByText('答：话题甲')).toBeInTheDocument())
    failSave(2)
    holdRetries()
    send('再问')
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    await waitFor(() => expect(within(historyList()).getByRole('button', { name: '删除对话：话题甲' })).toBeEnabled())
    fireEvent.click(within(historyList()).getByRole('button', { name: '删除对话：话题甲' }))
    // The page left it at once, before the deletion settled.
    expect(screen.queryByText('答：话题甲')).toBeNull()
    await waitFor(() => expect(saved.has('chat_a_1')).toBe(false))
    await retry()
    expect(saved.has('chat_a_1')).toBe(false)
  })

  it('reopening a conversation while its save waits to retry keeps the reply (PR-Daemon B1)', async () => {
    failSave(2)
    holdRetries()
    renderBoth()
    send('问A')
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    await waitFor(() => expect(chatHistory.save).toHaveBeenCalledTimes(2))
    // On disk: only the prompt; the reply's save waits to retry.
    expect([...saved.values()][0].messages.map(m => m.content)).toEqual(['问A'])
    // New chat, then reopen A inside the retry wait: the page gets the
    // unsaved record with the reply, not the prompt-only file.
    fireEvent.click(screen.getByRole('button', { name: '新对话' }))
    fireEvent.click(await within(await findHistoryList()).findByRole('button', { name: '问A' }))
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    proxy.mockResolvedValueOnce({ ok: true, status: 200, data: { message: { content: 'ack2' } } })
    send('第二问')
    await waitFor(() => expect(proxy).toHaveBeenCalledTimes(2))
    expect(proxy.mock.calls[1][0].body.messages).toEqual([
      { role: 'user', content: '问A' }, { role: 'assistant', content: 'ack' }, { role: 'user', content: '第二问' },
    ])
    await waitFor(() => expect([...saved.values()][0]?.messages.map(m => m.content)).toEqual(['问A', 'ack', '第二问', 'ack2']))
    await retry()
    expect([...saved.values()][0]?.messages.map(m => m.content)).toEqual(['问A', 'ack', '第二问', 'ack2'])
    expect([...saved.values()][0]?.messages[0]).toMatchObject({ delivered: true })
  })

  it('a conversation deleted while it is being opened is not opened (L1)', async () => {
    saved.set('chat_a_1', stored('chat_a_1', '话题甲', '2026-11-07T10:00:02.000Z'))
    saved.set('chat_b_1', stored('chat_b_1', '话题乙', '2026-11-07T09:00:02.000Z'))
    let load!: (v: ChatHistoryRecord | null) => void
    renderBoth()
    await within(await findHistoryList()).findByRole('button', { name: '话题乙' })
    chatHistory.get.mockImplementationOnce(() => new Promise(r => { load = r }))
    fireEvent.click(within(historyList()).getByRole('button', { name: '话题乙' }))
    const copy = saved.get('chat_b_1')!
    fireEvent.click(within(historyList()).getByRole('button', { name: '删除对话：话题乙' }))
    await waitFor(() => expect(saved.has('chat_b_1')).toBe(false))
    load(copy)
    await new Promise(r => setTimeout(r, 0))
    expect(screen.queryByText('答：话题乙')).toBeNull()
    send('新的')
    await waitFor(() => expect(proxy).toHaveBeenCalledTimes(1))
    expect(saved.has('chat_b_1')).toBe(false)
  })

  it.each([
    ['deleted', (title: string) => fireEvent.click(within(historyList()).getByRole('button', { name: `删除对话：${title}` }))],
    ['cleared', () => { vi.spyOn(window, 'confirm').mockReturnValue(true); fireEvent.click(within(historyList()).getByRole('button', { name: '清空' })) }],
  ])('the last save, failing and waiting to retry, stays gone once %s (L4)', async (_name, remove) => {
    // The reply's save (the second) fails and waits to retry.
    failSave(2)
    holdRetries()
    renderBoth()
    send('问C')
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    await waitFor(() => expect(chatHistory.save).toHaveBeenCalledTimes(2))
    await within(await findHistoryList()).findByRole('button', { name: '问C' })
    remove('问C')
    await waitFor(() => expect(saved.size).toBe(0))
    await retry()
    expect(saved.size).toBe(0)
    expect(screen.queryByRole('region', { name: '历史对话' })).toBeNull()
  })

  it('a save finishing after its conversation was deleted does not list it again', async () => {
    saved.set('chat_b_1', stored('chat_b_1', '话题乙', '2026-11-07T09:00:02.000Z'))
    renderBoth()
    await within(await findHistoryList()).findByRole('button', { name: '话题乙' })
    let finish!: () => void
    chatHistory.save.mockImplementationOnce(async (r: ChatHistoryRecord) => {
      await new Promise<void>(d => { finish = d })
      saved.set(r.id, structuredClone(r))
    })
    send('问D')
    await waitFor(() => expect(chatHistory.save).toHaveBeenCalledTimes(1))
    // While that save is in flight the page is busy; delete another, then
    // check the in-flight one does not resurrect a deleted entry either.
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    const id = proxy.mock.calls[0][0].body.session_id as string
    fireEvent.click(screen.getByRole('button', { name: '新对话' }))
    await within(historyList()).findByRole('button', { name: '问D' })
    fireEvent.click(within(historyList()).getByRole('button', { name: '删除对话：问D' }))
    await waitFor(() => expect(within(historyList()).queryByRole('button', { name: '问D' })).toBeNull())
    finish()
    await new Promise(r => setTimeout(r, 0))
    expect(within(historyList()).queryByRole('button', { name: '问D' })).toBeNull()
    expect(id).toMatch(/^chat_/)
  })

  it('clicking the open conversation drops another one still loading', async () => {
    saved.set('chat_a_1', stored('chat_a_1', '话题甲', '2026-11-07T10:00:02.000Z'))
    saved.set('chat_b_1', stored('chat_b_1', '话题乙', '2026-11-07T09:00:02.000Z'))
    renderBoth()
    fireEvent.click(await within(await findHistoryList()).findByRole('button', { name: '话题甲' }))
    await waitFor(() => expect(screen.getByText('答：话题甲')).toBeInTheDocument())
    let load!: (v: ChatHistoryRecord | null) => void
    chatHistory.get.mockImplementationOnce(() => new Promise(r => { load = r }))
    fireEvent.click(within(historyList()).getByRole('button', { name: '话题乙' }))
    fireEvent.click(within(historyList()).getByRole('button', { name: '话题甲' }))
    load(saved.get('chat_b_1')!)
    await new Promise(r => setTimeout(r, 0))
    expect(screen.getByText('答：话题甲')).toBeInTheDocument()
    expect(screen.queryByText('答：话题乙')).toBeNull()
  })

  it('a list read that started before a retried save was listed does not undo it', async () => {
    saved.set('chat_b_1', stored('chat_b_1', '话题乙', '2026-11-07T09:00:02.000Z'))
    // Both saves of the new conversation fail: nothing of it is on disk yet.
    failSave(1, 2)
    holdRetries()
    renderBoth()
    await within(await findHistoryList()).findByRole('button', { name: '话题乙' })
    send('问E')
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    await waitFor(() => expect(chatHistory.save).toHaveBeenCalledTimes(2))
    expect(saved.size).toBe(1)
    // Deleting B starts a list read, held here: its snapshot has no 问E.
    let listed!: (v: unknown) => void
    chatHistory.list.mockImplementationOnce(() => new Promise(r => { listed = r }))
    fireEvent.click(within(historyList()).getByRole('button', { name: '删除对话：话题乙' }))
    await waitFor(() => expect(chatHistory.list).toHaveBeenCalledTimes(2))
    // Meanwhile the reply's retry succeeds and lists 问E.
    await retry()
    await within(historyList()).findByRole('button', { name: '问E' })
    listed([])
    await new Promise(r => setTimeout(r, 0))
    expect(within(historyList()).getByRole('button', { name: '问E' })).toBeInTheDocument()
  })

  it('works without the history bridge: the chat goes on, nothing is listed', async () => {
    Object.defineProperty(window, 'agent24', { value: { backendProxy: proxy }, writable: true, configurable: true })
    renderBoth()
    send('你好')
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    expect(screen.queryByRole('region', { name: '历史对话' })).toBeNull()
  })
})

describe('renaming a conversation (UI-04)', () => {
  const rename = async (from: string) => {
    fireEvent.click(within(await findHistoryList()).getByRole('button', { name: `重命名对话：${from}` }))
    return within(historyList()).getByRole('textbox', { name: '对话标题' }) as HTMLInputElement
  }

  it('renames with ✎ and Enter; later saves of the conversation keep the new title', async () => {
    renderBoth()
    send('第一个问题')
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    const id = proxy.mock.calls[0][0].body.session_id
    await within(await findHistoryList()).findByRole('button', { name: '第一个问题' })
    const input = await rename('第一个问题')
    expect(input.value).toBe('第一个问题')
    fireEvent.change(input, { target: { value: '  周报  草稿 ' } })
    fireEvent.keyDown(input, { key: 'Enter' })
    await within(historyList()).findByRole('button', { name: '周报 草稿' })
    expect(chatHistory.setTitle).toHaveBeenCalledTimes(1)
    expect(chatHistory.setTitle).toHaveBeenCalledWith(id, '周报 草稿', 'user')
    await waitFor(() => expect(within(historyList()).getByRole('button', { name: '周报 草稿' })).toHaveFocus())
    // The next turn saves the first prompt's title again; the rename stays.
    proxy.mockResolvedValueOnce({ ok: true, status: 200, data: { message: { content: 'ack2' } } })
    send('第二个问题')
    await waitFor(() => expect(saved.get(id)?.messages).toHaveLength(4))
    await waitFor(() => expect(chatHistory.save.mock.calls.at(-1)?.[0].messages).toHaveLength(4))
    await new Promise(r => setTimeout(r, 0))
    expect(chatHistory.save.mock.calls.at(-1)?.[0]).toMatchObject({ title: '第一个问题', title_source: 'prompt' })
    expect(saved.get(id)).toMatchObject({ title: '周报 草稿', title_source: 'user' })
    expect(within(historyList()).getByRole('button', { name: '周报 草稿' })).toBeInTheDocument()
    expect(within(historyList()).queryByRole('button', { name: '第一个问题' })).toBeNull()
  })

  it('Escape, an empty title or the same title change nothing', async () => {
    saved.set('chat_old_1', stored('chat_old_1', '旧话题', '2026-11-07T10:00:02.000Z'))
    renderBoth()
    await within(await findHistoryList()).findByRole('button', { name: '旧话题' })
    let input = await rename('旧话题')
    fireEvent.change(input, { target: { value: '不要了' } })
    fireEvent.keyDown(input, { key: 'Escape' })
    expect(within(historyList()).queryByRole('textbox')).toBeNull()
    input = await rename('旧话题')
    fireEvent.change(input, { target: { value: '   ' } })
    fireEvent.keyDown(input, { key: 'Enter' })
    input = await rename('旧话题')
    fireEvent.keyDown(input, { key: 'Enter' })
    await new Promise(r => setTimeout(r, 0))
    expect(chatHistory.setTitle).not.toHaveBeenCalled()
    expect(within(historyList()).getByRole('button', { name: '旧话题' })).toBeInTheDocument()
  })

  it('a double click or F2 starts a rename; leaving the box saves it once', async () => {
    saved.set('chat_old_1', stored('chat_old_1', '旧话题', '2026-11-07T10:00:02.000Z'))
    saved.set('chat_old_2', stored('chat_old_2', '另一个', '2026-11-07T09:00:02.000Z'))
    renderBoth()
    const list = await findHistoryList()
    fireEvent.doubleClick(await within(list).findByRole('button', { name: '旧话题' }))
    let input = within(historyList()).getByRole('textbox', { name: '对话标题' })
    fireEvent.change(input, { target: { value: '改过的' } })
    fireEvent.keyDown(input, { key: 'Enter' })
    fireEvent.blur(input)
    await within(historyList()).findByRole('button', { name: '改过的' })
    fireEvent.keyDown(within(historyList()).getByRole('button', { name: '另一个' }), { key: 'F2' })
    input = within(historyList()).getByRole('textbox', { name: '对话标题' })
    fireEvent.change(input, { target: { value: '也改了' } })
    fireEvent.blur(input)
    await within(historyList()).findByRole('button', { name: '也改了' })
    expect(chatHistory.setTitle.mock.calls).toEqual([
      ['chat_old_1', '改过的', 'user'],
      ['chat_old_2', '也改了', 'user'],
    ])
  })

  it('keeps a rename to 80 characters without splitting one', async () => {
    saved.set('chat_old_1', stored('chat_old_1', '旧话题', '2026-11-07T10:00:02.000Z'))
    renderBoth()
    await within(await findHistoryList()).findByRole('button', { name: '旧话题' })
    const input = await rename('旧话题')
    fireEvent.change(input, { target: { value: `${'字'.repeat(79)}👍🏽多余` } })
    fireEvent.keyDown(input, { key: 'Enter' })
    await waitFor(() => expect(chatHistory.setTitle).toHaveBeenCalledWith('chat_old_1', `${'字'.repeat(79)}👍🏽`, 'user'))
  })

  it('Enter or Escape while an input method composes does not end the rename (Codex R1-2)', async () => {
    saved.set('chat_old_1', stored('chat_old_1', '旧话题', '2026-11-07T10:00:02.000Z'))
    renderBoth()
    await within(await findHistoryList()).findByRole('button', { name: '旧话题' })
    const input = await rename('旧话题')
    fireEvent.change(input, { target: { value: 'zhou' } })
    fireEvent.keyDown(input, { key: 'Enter', isComposing: true })
    fireEvent.keyDown(input, { key: 'Escape', keyCode: 229 })
    expect(within(historyList()).getByRole('textbox', { name: '对话标题' })).toBe(input)
    fireEvent.change(input, { target: { value: '周报' } })
    fireEvent.keyDown(input, { key: 'Enter' })
    await within(historyList()).findByRole('button', { name: '周报' })
    expect(chatHistory.setTitle.mock.calls).toEqual([['chat_old_1', '周报', 'user']])
  })

  it('a box left as it opened renames nothing, even after an earlier rename lands (Codex R1-1)', async () => {
    saved.set('chat_old_1', stored('chat_old_1', '旧话题', '2026-11-07T10:00:02.000Z'))
    renderBoth()
    await within(await findHistoryList()).findByRole('button', { name: '旧话题' })
    let answer!: () => void
    chatHistory.setTitle.mockImplementationOnce((id: string, title: string) => new Promise(r => {
      answer = () => { saved.set(id, { ...saved.get(id)!, title, title_source: 'user' }); r(summary(saved.get(id)!)) }
    }))
    let input = await rename('旧话题')
    fireEvent.change(input, { target: { value: '新名' } })
    fireEvent.keyDown(input, { key: 'Enter' })
    await waitFor(() => expect(chatHistory.setTitle).toHaveBeenCalledTimes(1))
    // Opened again while the rename is on its way: the box still says 旧话题.
    input = await rename('旧话题')
    answer()
    // The row is still being edited: its delete button shows the new title.
    await within(historyList()).findByRole('button', { name: '删除对话：新名' })
    fireEvent.blur(input)
    await new Promise(r => setTimeout(r, 0))
    expect(chatHistory.setTitle).toHaveBeenCalledTimes(1)
    expect(saved.get('chat_old_1')?.title).toBe('新名')
  })

  it('shortening a title longer than 80 characters to 80 is a rename (Codex R2-1)', async () => {
    saved.set('chat_old_1', stored('chat_old_1', '字'.repeat(81), '2026-11-07T10:00:02.000Z'))
    renderBoth()
    await within(await findHistoryList()).findByRole('button', { name: '字'.repeat(81) })
    const input = await rename('字'.repeat(81))
    fireEvent.change(input, { target: { value: '字'.repeat(80) } })
    fireEvent.keyDown(input, { key: 'Enter' })
    await waitFor(() => expect(chatHistory.setTitle).toHaveBeenCalledWith('chat_old_1', '字'.repeat(80), 'user'))
  })

  it('keeps 80 long emoji sequences whole (Codex R1-3)', async () => {
    saved.set('chat_old_1', stored('chat_old_1', '旧话题', '2026-11-07T10:00:02.000Z'))
    renderBoth()
    await within(await findHistoryList()).findByRole('button', { name: '旧话题' })
    const input = await rename('旧话题')
    expect(input).not.toHaveAttribute('maxlength')
    const family = '👨‍👩‍👧‍👦' // 11 UTF-16 units, one character
    fireEvent.change(input, { target: { value: family.repeat(81) } })
    fireEvent.keyDown(input, { key: 'Enter' })
    await waitFor(() => expect(chatHistory.setTitle).toHaveBeenCalledWith('chat_old_1', family.repeat(80), 'user'))
  })

  it('a rename answered after its conversation was deleted does not list it again', async () => {
    saved.set('chat_old_1', stored('chat_old_1', '旧话题', '2026-11-07T10:00:02.000Z'))
    renderBoth()
    await within(await findHistoryList()).findByRole('button', { name: '旧话题' })
    let answer!: (v: unknown) => void
    chatHistory.setTitle.mockImplementationOnce(() => new Promise(r => { answer = r }))
    const input = await rename('旧话题')
    fireEvent.change(input, { target: { value: '新名' } })
    fireEvent.keyDown(input, { key: 'Enter' })
    await waitFor(() => expect(chatHistory.setTitle).toHaveBeenCalled())
    fireEvent.click(within(historyList()).getByRole('button', { name: '删除对话：旧话题' }))
    await waitFor(() => expect(chatHistory.delete).toHaveBeenCalledWith('chat_old_1'))
    answer({ ...summary(saved.get('chat_old_1') ?? stored('chat_old_1', '旧话题', '2026-11-07T10:00:02.000Z')), title: '新名', title_source: 'user' })
    await new Promise(r => setTimeout(r, 0))
    expect(screen.queryByRole('button', { name: '新名' })).toBeNull()
    expect(screen.queryByRole('region', { name: '历史对话' })).toBeNull()
  })

  it('a list read that started before a rename does not undo it', async () => {
    saved.set('chat_a_1', stored('chat_a_1', '话题甲', '2026-11-07T10:00:02.000Z'))
    saved.set('chat_b_1', stored('chat_b_1', '话题乙', '2026-11-07T09:00:02.000Z'))
    renderBoth()
    await within(await findHistoryList()).findByRole('button', { name: '话题甲' })
    // Deleting B starts a list read, held here: its snapshot has the old title.
    const snapshot = await chatHistory.list()
    let listed!: (v: unknown) => void
    chatHistory.list.mockImplementationOnce(() => new Promise(r => { listed = r }))
    fireEvent.click(within(historyList()).getByRole('button', { name: '删除对话：话题乙' }))
    await waitFor(() => expect(chatHistory.list).toHaveBeenCalledTimes(3))
    const input = await rename('话题甲')
    fireEvent.change(input, { target: { value: '甲改名' } })
    fireEvent.keyDown(input, { key: 'Enter' })
    await within(historyList()).findByRole('button', { name: '甲改名' })
    listed(snapshot.filter((s: { id: string }) => s.id !== 'chat_b_1'))
    await new Promise(r => setTimeout(r, 0))
    expect(within(historyList()).getByRole('button', { name: '甲改名' })).toBeInTheDocument()
    expect(within(historyList()).queryByRole('button', { name: '话题甲' })).toBeNull()
  })

  it('a failed rename keeps the old title', async () => {
    saved.set('chat_old_1', stored('chat_old_1', '旧话题', '2026-11-07T10:00:02.000Z'))
    chatHistory.setTitle.mockRejectedValueOnce(new Error('EACCES'))
    renderBoth()
    await within(await findHistoryList()).findByRole('button', { name: '旧话题' })
    const input = await rename('旧话题')
    fireEvent.change(input, { target: { value: '新名' } })
    fireEvent.keyDown(input, { key: 'Enter' })
    await waitFor(() => expect(chatHistory.setTitle).toHaveBeenCalled())
    await new Promise(r => setTimeout(r, 0))
    expect(within(historyList()).getByRole('button', { name: '旧话题' })).toBeInTheDocument()
  })
})

describe('finding a conversation (UI-04 T3)', () => {
  const many = (n: number) => {
    for (let i = 0; i < n; i++) {
      const r = stored(`chat_n_${i}`, `话题${i}`, `2026-11-07T0${i % 10}:00:00.000Z`)
      saved.set(r.id, r)
    }
  }

  it('offers a search only with more than 8 conversations', async () => {
    many(8)
    const { unmount } = render(<ChatHistoryList showChat={showChat} />)
    await within(await findHistoryList()).findByRole('button', { name: '话题0' })
    expect(within(historyList()).queryByRole('searchbox')).toBeNull()
    unmount()
    resetHistoryStore()
    many(9)
    render(<ChatHistoryList showChat={showChat} />)
    expect(await within(await findHistoryList()).findByRole('searchbox', { name: '搜索对话' })).toBeInTheDocument()
  })

  it('matches the title or the first prompt, ignoring case and width; Escape clears', async () => {
    many(9)
    const r = stored('chat_x_1', '模型起的名', '2026-11-07T11:00:00.000Z')
    r.messages[0] = { ...r.messages[0], content: '帮我看看 Weekly Report 的格式' }
    saved.set(r.id, r)
    render(<ChatHistoryList showChat={showChat} />)
    const box = await within(await findHistoryList()).findByRole('searchbox', { name: '搜索对话' })
    const names = () => within(historyList()).getAllByRole('button').map(b => b.textContent).filter(n => n && n !== '清空' && n !== '✎' && n !== '×')
    fireEvent.change(box, { target: { value: 'ｗｅｅｋｌｙ　ｒｅｐｏｒｔ' } })
    expect(names()).toEqual(['模型起的名'])
    fireEvent.change(box, { target: { value: '话题3' } })
    expect(names()).toEqual(['话题3'])
    fireEvent.change(box, { target: { value: '没有这个' } })
    expect(names()).toEqual([])
    expect(within(historyList()).getByRole('status')).toHaveTextContent('没有匹配的对话')
    fireEvent.keyDown(box, { key: 'Escape' })
    expect(box).toHaveValue('')
    expect(names()).toHaveLength(10)
  })

  it('a search left typed stops filtering once the list is short enough to lose its box', async () => {
    many(9)
    render(<ChatHistoryList showChat={showChat} />)
    const box = await within(await findHistoryList()).findByRole('searchbox', { name: '搜索对话' })
    fireEvent.change(box, { target: { value: '话题8' } })
    fireEvent.click(within(historyList()).getByRole('button', { name: '删除对话：话题8' }))
    await waitFor(() => expect(within(historyList()).queryByRole('searchbox')).toBeNull())
    expect(within(historyList()).getAllByRole('button', { name: /^话题\d$/ })).toHaveLength(8)
  })

  it('conversations with the same title show their time; the tooltip adds the first prompt', async () => {
    const a = stored('chat_a_1', '周报', '2026-11-07T10:00:00.000Z')
    const b = stored('chat_b_1', '周报', '2026-11-08T09:30:00.000Z')
    b.messages[0] = { ...b.messages[0], content: '写本周周报' }
    const c = stored('chat_c_1', '请假', '2026-11-06T08:00:00.000Z')
    for (const r of [a, b, c]) saved.set(r.id, r)
    render(<ChatHistoryList showChat={showChat} />)
    const list = await findHistoryList()
    const later = await within(list).findByRole('button', { name: `周报 ${shortTime(b.updated_at)}` })
    expect(within(list).getByRole('button', { name: `周报 ${shortTime(a.updated_at)}` })).toBeInTheDocument()
    expect(within(list).getByRole('button', { name: '请假' })).toBeInTheDocument()
    expect(within(list).getByRole('button', { name: `删除对话：周报 ${shortTime(b.updated_at)}` })).toBeInTheDocument()
    expect(later).toHaveAttribute('title', `周报 · 写本周周报 · ${shortTime(b.updated_at)}`)
    // The first prompt is left out of the tooltip when it is the title.
    expect(within(list).getByRole('button', { name: '请假' })).toHaveAttribute('title', `请假 · ${shortTime(c.updated_at)}`)
  })

  it('writes the time as MM-DD HH:mm in local time', () => {
    const d = new Date(2026, 10, 8, 9, 5)
    expect(shortTime(d.toISOString())).toBe('11-08 09:05')
    expect(shortTime('不是时间')).toBe('')
  })
})
