// @vitest-environment jsdom
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { createChatHistoryStore } from '../../../main/chat-history'
import { CHAT_HISTORY_FORMAT, CHAT_HISTORY_VERSION, type ChatHistoryRecord } from '../../../shared/chat-history'
import type { RunEvent } from '../../../shared/ipc-types'
import { HandOver, TaskCards, approvalArgs, approvalText, elapsed, patchOriginals, resetHandOversForTests } from './TaskCard'
import { TASKS_NOT_STOPPED, clearConversations, deleteConversation, refreshHistory, resetHistoryStore, registerChatPage, saveConversation, setActiveConversation, setChatBusy } from './history'
import { resetRunStatusesForTests } from './run-status'
import { setLocale } from '../../i18n'
import ChatPage from '../Chat'

// M4 A4: a conversation's tasks as cards, against main's real store and a
// daemon the test plays, with its /events.

let dir: string
let store: ReturnType<typeof createChatHistoryStore>
const wrap = <A extends unknown[], R>(f: (...a: A) => R) => vi.fn(async (...a: A) => f(...a))
type Run = { status: string; started_at?: string; ended_at?: string | null; output?: { text: string } | null; knowledge?: unknown; error?: { code: string; message: string } | null }
let runs: Record<string, Run>
let approvals: { id: string; run_id: string; available_decisions: string[]; summary?: string }[]
let proxy: ReturnType<typeof vi.fn>
let approvalDecide: ReturnType<typeof vi.fn>
let emit: (event: RunEvent) => void
let bridge: Record<string, ReturnType<typeof vi.fn>>

const at = '2026-11-09T01:00:00.000Z'
function record(id: string, prompt: string): ChatHistoryRecord {
  return {
    format: CHAT_HISTORY_FORMAT, version: CHAT_HISTORY_VERSION, id, title: prompt, title_source: 'prompt',
    group_id: null, archived_at: null, created_at: at, updated_at: at, runs: [], pending: [], initiator: 'user', unseen: [],
    messages: [{ role: 'user', content: prompt, at, delivered: true }],
  }
}

beforeEach(() => {
  setLocale('zh')
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'task-card-'))
  store = createChatHistoryStore(dir)
  runs = {}
  approvals = []
  proxy = vi.fn(async (req: { method: string; path: string; body?: { prompt?: string } }) => {
    if (req.method === 'POST' && req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: 'sess_new' } }
    if (req.method === 'POST' && req.path === '/api/v1/runs') {
      runs.run_new = { status: 'queued', started_at: at }
      return { ok: true, status: 201, data: { id: 'run_new', status: 'queued' } }
    }
    const cancel = /^\/api\/v1\/runs\/([^/]+)\/cancel$/.exec(req.path)
    if (cancel) { runs[cancel[1]] = { ...runs[cancel[1]], status: 'cancelled', ended_at: at }; return { ok: true, status: 200, data: {} } }
    if (req.path === '/api/v1/approvals?status=pending') return { ok: true, status: 200, data: { approvals } }
    const listed = /^\/api\/v1\/runs\?status=(\w+)$/.exec(req.path)
    if (listed) return { ok: true, status: 200, data: { runs: Object.entries(runs).filter(([, r]) => r.status === listed[1]).map(([id, r]) => ({ id, ...r })) } }
    const id = decodeURIComponent(req.path.replace('/api/v1/runs/', ''))
    return id in runs ? { ok: true, status: 200, data: { id, ...runs[id] } } : { ok: false, status: 404, data: null }
  })
  approvalDecide = vi.fn(async () => ({ ok: true, status: 200, data: {} }))
  const listeners = new Set<(e: RunEvent) => void>()
  emit = event => { for (const l of listeners) l(event) }
  bridge = {
    list: wrap(() => store.list()), get: wrap((id: string) => store.get(id)), save: wrap((r: unknown) => store.save(r)),
    promoteRun: wrap((id: string, runId: string) => store.promoteRun(id, runId)),
    delete: wrap((id: string) => store.delete(id)), clear: wrap(() => store.clear()),
  }
  Object.defineProperty(window, 'agent24', {
    value: {
      chatHistory: bridge, backendProxy: proxy, approvalDecide,
      onRunEvent: (cb: (e: RunEvent) => void) => { listeners.add(cb); return () => listeners.delete(cb) },
    },
    writable: true, configurable: true,
  })
  resetHistoryStore()
  resetRunStatusesForTests()
  resetHandOversForTests()
})
afterEach(() => { vi.restoreAllMocks(); fs.rmSync(dir, { recursive: true, force: true }) })

const cards = () => screen.getByRole('list', { name: '这条对话里的任务' })
const card = (status: string) => within(cards()).getByRole('listitem', { name: `任务：${status}` })
const show = async (conversation = 'chat_a') => {
  await refreshHistory()
  return render(<TaskCards conversation={conversation} />)
}

describe('task cards (M4 A4)', () => {
  it('shows no card for a conversation without tasks; answers get none', async () => {
    store.save({ ...record('chat_a', '问'), messages: [...record('chat_a', '问').messages, { role: 'assistant', content: '答', at, run_id: 'run_answer', latency: null }] })
    const { container } = await show()
    expect(container).toBeEmptyDOMElement()
    expect(proxy).not.toHaveBeenCalled()
  })

  it('shows each task\'s status and time, and comes back after the window does, asking the daemon again', async () => {
    store.save(record('chat_a', '周报'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'running', started_at: '2026-11-09T01:00:00.000Z' }
    const first = await show()
    await waitFor(() => expect(card('运行中')).toBeInTheDocument())
    expect(within(card('运行中')).getByRole('button', { name: '停止' })).toBeInTheDocument()
    first.unmount()
    // The window is back (the tray kept the app): the tasks are in the file.
    resetHistoryStore()
    resetRunStatusesForTests()
    runs.run_1 = { status: 'completed', started_at: '2026-11-09T01:00:00.000Z', ended_at: '2026-11-09T01:02:05.000Z', output: { text: '周报好了' } }
    await show()
    await waitFor(() => expect(card('已完成')).toBeInTheDocument())
    expect(within(card('已完成')).getByText('02:05')).toBeInTheDocument()
    expect(within(card('已完成')).queryByRole('button')).toBeNull()
  })

  it('follows the daemon\'s events: no polling', async () => {
    store.save(record('chat_a', '周报'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'queued', started_at: at }
    await show()
    await waitFor(() => expect(card('排队中')).toBeInTheDocument())
    const asked = proxy.mock.calls.length
    runs.run_1 = { status: 'running', started_at: at }
    await act(async () => { await new Promise(r => setTimeout(r, 50)) })
    expect(proxy).toHaveBeenCalledTimes(asked)
    act(() => emit({ type: 'run.started', run_id: 'run_1' }))
    await waitFor(() => expect(card('运行中')).toBeInTheDocument())
  })

  // General 0.6.89: 「停止」 on a task waiting on an approval.
  it('stops a task that waits on an approval with the cancel API', async () => {
    store.save(record('chat_a', '改写'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'awaiting_approval', started_at: at }
    approvals = [{ id: 'ap_1', run_id: 'run_1', available_decisions: ['approve', 'deny'] }]
    await show()
    fireEvent.click(await within(await waitFor(() => card('等你确认'))).findByRole('button', { name: '停止' }))
    await waitFor(() => expect(proxy).toHaveBeenCalledWith({ method: 'POST', path: '/api/v1/runs/run_1/cancel' }))
  })

  it('says so when stopping a task that waits on an approval fails', async () => {
    store.save(record('chat_a', '改写'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'awaiting_approval', started_at: at }
    approvals = [{ id: 'ap_1', run_id: 'run_1', available_decisions: ['approve', 'deny'] }]
    const base = proxy.getMockImplementation() as (req: { method: string; path: string }) => Promise<unknown>
    proxy.mockImplementation(async (req: { method: string; path: string }) =>
      req.method === 'POST' && req.path === '/api/v1/runs/run_1/cancel'
        ? { ok: false, status: 502, data: { error: { code: 'proxy_failed', message: 'daemon did not answer' } } }
        : base(req))
    await show()
    const c = await waitFor(() => card('等你确认'))
    fireEvent.click(await within(c).findByRole('button', { name: '停止' }))
    await waitFor(() => expect(within(c).getByRole('alert').textContent).toContain('daemon did not answer'))
  })

  it('offers 「停止」 even while another action of the card is under way', async () => {
    store.save(record('chat_a', '改写'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'awaiting_approval', started_at: at }
    approvals = [{ id: 'ap_1', run_id: 'run_1', available_decisions: ['approve', 'deny'] }]
    // A confirm that never answers (the native box still open).
    approvalDecide.mockImplementation(() => new Promise(() => {}))
    await show()
    const c = await waitFor(() => card('等你确认'))
    fireEvent.click(await within(c).findByRole('button', { name: '确认' }))
    await waitFor(() => expect(approvalDecide).toHaveBeenCalled())
    expect(within(c).getByRole('button', { name: '停止' })).toBeEnabled()
    fireEvent.click(within(c).getByRole('button', { name: '停止' }))
    await waitFor(() => expect(proxy).toHaveBeenCalledWith({ method: 'POST', path: '/api/v1/runs/run_1/cancel' }))
  })

  it('stops a task with the cancel API', async () => {
    store.save(record('chat_a', '周报'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'running', started_at: at }
    await show()
    fireEvent.click(await within(await waitFor(() => card('运行中'))).findByRole('button', { name: '停止' }))
    await waitFor(() => expect(card('已停止')).toBeInTheDocument())
    expect(proxy).toHaveBeenCalledWith({ method: 'POST', path: '/api/v1/runs/run_1/cancel' })
  })

  it('confirms the approval its run waits on with `approve` only, through main\'s dialog (Knowledge\'s review of #1700)', async () => {
    store.save(record('chat_a', '周报'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'awaiting_approval', started_at: at }
    // The daemon's own decisions (openapi `Decision`), the broader grants and
    // abort first: none of them is what "confirm" means.
    approvals = [
      { id: 'ap_other', run_id: 'run_2', available_decisions: ['approve'] },
      { id: 'ap_1', run_id: 'run_1', available_decisions: ['abort', 'approve_for_session', 'approve_for_target', 'deny', 'approve'] },
    ]
    await show()
    fireEvent.click(await within(await waitFor(() => card('等你确认'))).findByRole('button', { name: '确认' }))
    await waitFor(() => expect(approvalDecide).toHaveBeenCalledWith('ap_1', { type: 'approve' }))
    expect(approvalDecide).toHaveBeenCalledTimes(1)
  })

  // PR-Daemon #1987 Medium 1: two patch approvals in one run. 「确认」 goes to
  // the one the card shows; when that one was decided meanwhile the card says
  // so and does not approve the other one in its place.
  it('decides the approval the card shows, never another one of the same run', async () => {
    store.save(record('chat_a', '改写'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'awaiting_approval', started_at: at }
    approvals = [
      { id: 'ap_A', run_id: 'run_1', available_decisions: ['approve', 'deny'] },
      { id: 'ap_B', run_id: 'run_1', available_decisions: ['approve', 'deny'] },
    ]
    await show()
    const c = await waitFor(() => card('等你确认'))
    // A was decided elsewhere; only B is pending now.
    approvals = [{ id: 'ap_B', run_id: 'run_1', available_decisions: ['approve', 'deny'] }]
    approvalDecide.mockImplementation(async (id: string) => id === 'ap_A'
      ? { ok: false, status: 409, data: { error: { code: 'conflict', message: 'not pending' } } }
      : { ok: true, status: 200, data: {} })
    fireEvent.click(await within(c).findByRole('button', { name: '确认' }))
    await waitFor(() => expect(within(c).getByRole('alert').textContent).toContain('这条已处理'))
    expect(approvalDecide).toHaveBeenCalledTimes(1)
    expect(approvalDecide).toHaveBeenCalledWith('ap_A', { type: 'approve' })
  })

  // M1 (c), PM 11-21: a patch aimed at the wrong paragraph goes back to the
  // model before the user is asked; one aimed right waits for the user.
  it.each([
    ['押金为两个月', 5, false],
    ['押金为两个月', 2, true],
  ])('a patch with anchor %s at index %i is sent back: %s', async (anchorText, index, sentBack) => {
    store.save(record('chat_a', '改写'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'awaiting_approval', started_at: at }
    approvals = [{
      id: 'ap_1', run_id: 'run_1', available_decisions: ['approve', 'deny'],
      summary: `documents.patch: {"document_id":"doc_1","edits":[{"anchor":"${anchorText}","index":${index},"text":"新文字"}]}`,
    }]
    const request = vi.fn(async () => ({ ok: true, data: { paragraphs: [
      { index: 2, text: '乙方：张三', in_table: false },
      { index: 5, text: '第三条 押金为两个月租金，合同期满无违约时全额退还。', in_table: false },
    ] } }))
    ;(window.agent24 as unknown as { documents: unknown }).documents = { request }
    await show()
    const c = await waitFor(() => card('等你确认'))
    if (sentBack) {
      await waitFor(() => expect(approvalDecide).toHaveBeenCalledWith('ap_1', expect.objectContaining({ type: 'deny' })))
      const reason = (approvalDecide.mock.calls[0] as unknown as [string, { reason: string }])[1].reason
      expect(reason).toContain('乙方：张三')
      expect(within(c).queryByRole('button', { name: '确认' })).toBeNull()
      expect(within(c).queryByText(/documents\.patch: \{/)).toBeNull()
    } else {
      await waitFor(() => expect(within(c).getByText(/第 6 段：「第三条 押金为两个月租金/)).toBeInTheDocument())
      await waitFor(() => expect(within(c).getByRole('button', { name: '确认' })).toBeEnabled())
      expect(approvalDecide).not.toHaveBeenCalled()
    }
  })

  // PR-Daemon #1986 A: a patch whose edits cannot all be read (no payload,
  // summary cut) is not offered for approval; with its payload it is.
  it.each([
    ['without a payload', false],
    ['with its payload', true],
  ])('a cut patch summary %s: confirm offered %s', async (_case, withPayload) => {
    store.save(record('chat_a', '改写'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'awaiting_approval', started_at: at }
    const payload = { document_id: 'doc_1', edits: [12, 13, 14].map(index => ({ anchor: '押金为两个月租金，合同', index, text: '新的条款文字，较长一些。', tracked: true })) }
    const full = `documents.patch: ${JSON.stringify(payload)}`
    approvals = [{
      id: 'ap_1', run_id: 'run_1', available_decisions: ['approve', 'deny'],
      summary: `${full.slice(0, 200)}… [truncated ${full.length - 200} bytes]`,
      ...(withPayload ? { payload } : {}),
    } as never]
    const request = vi.fn(async () => ({ ok: true, data: { paragraphs: [12, 13, 14].map(index => ({ index, text: '押金为两个月租金，合同期满无违约时全额退还。', in_table: false })) } }))
    ;(window.agent24 as unknown as { documents: unknown }).documents = { request }
    await show()
    const c = await waitFor(() => card('等你确认'))
    await waitFor(() => expect(within(c).getByRole('button', { name: '拒绝' })).toBeInTheDocument())
    if (withPayload) {
      await waitFor(() => expect(within(c).getByRole('button', { name: '确认' })).toBeEnabled())
      expect(within(c).getByText(/3 段/)).toBeInTheDocument()
    } else {
      await new Promise(r => setTimeout(r, 50))
      expect(within(c).queryByRole('button', { name: '确认' })).toBeNull()
    }
    expect(approvalDecide).not.toHaveBeenCalled()
  })

  it.each([
    [['approve_for_session', 'deny', 'abort']],
    [['approve_for_target', 'deny', 'abort']],
    [['deny', 'abort']],
  ])('offers no "confirm" for an approval without `approve` (%j), only "deny"', async decisions => {
    store.save(record('chat_a', '周报'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'awaiting_approval', started_at: at }
    approvals = [{ id: 'ap_1', run_id: 'run_1', available_decisions: decisions }]
    await show()
    expect(await within(await waitFor(() => card('等你确认'))).findByRole('button', { name: '拒绝' })).toBeInTheDocument()
    expect(within(card('等你确认')).queryByRole('button', { name: '确认' })).toBeNull()
    expect(approvalDecide).not.toHaveBeenCalled()
  })

  it('points to the terminal for an approval it can neither confirm nor deny', async () => {
    store.save(record('chat_a', '周报'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'awaiting_approval', started_at: at }
    approvals = [{ id: 'ap_1', run_id: 'run_1', available_decisions: ['approve_for_session', 'abort'] }]
    await show()
    expect(await within(await waitFor(() => card('等你确认'))).findByRole('note')).toHaveTextContent('agent24 tui')
    expect(within(card('等你确认')).queryByRole('button', { name: '确认' })).toBeNull()
    expect(within(card('等你确认')).queryByRole('button', { name: '拒绝' })).toBeNull()
    expect(within(card('等你确认')).getByRole('button', { name: '停止' })).toBeInTheDocument()
  })

  it('denies with the reason given in the card (M4 A6)', async () => {
    store.save(record('chat_a', '周报'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'awaiting_approval', started_at: at }
    approvals = [{ id: 'ap_1', run_id: 'run_1', available_decisions: ['approve', 'approve_for_session', 'deny', 'abort'] }]
    await show()
    fireEvent.click(await within(await waitFor(() => card('等你确认'))).findByRole('button', { name: '拒绝' }))
    const reason = within(card('等你确认')).getByRole('textbox', { name: '拒绝的理由（可不填，会告诉助理）' })
    const send = () => within(card('等你确认')).getAllByRole('button', { name: '拒绝' }).find(b => b.getAttribute('type') === 'submit')!
    // While it is asked for, the card offers no "confirm".
    expect(within(card('等你确认')).queryByRole('button', { name: '确认' })).toBeNull()
    fireEvent.change(reason, { target: { value: ' 不要删这个目录 ' } })
    fireEvent.click(send())
    await waitFor(() => expect(approvalDecide).toHaveBeenCalledWith('ap_1', { type: 'deny', reason: '不要删这个目录' }))
    expect(approvalDecide).toHaveBeenCalledTimes(1)
  })

  // Loop 3 FU (General 11-19): the second 「拒绝」 did nothing until a reason was typed, and nothing said so.
  it('denies with a plain reason when none is given', async () => {
    store.save(record('chat_a', '周报'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'awaiting_approval', started_at: at }
    approvals = [{ id: 'ap_1', run_id: 'run_1', available_decisions: ['approve', 'deny'] }]
    await show()
    fireEvent.click(await within(await waitFor(() => card('等你确认'))).findByRole('button', { name: '拒绝' }))
    const send = within(card('等你确认')).getAllByRole('button', { name: '拒绝' }).find(b => b.getAttribute('type') === 'submit')!
    expect(send).not.toBeDisabled()
    fireEvent.click(send)
    await waitFor(() => expect(approvalDecide).toHaveBeenCalledWith('ap_1', { type: 'deny', reason: '用户拒绝了这一步。' }))
  })

  it('goes back from asking a reason without denying', async () => {
    store.save(record('chat_a', '周报'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'awaiting_approval', started_at: at }
    approvals = [{ id: 'ap_1', run_id: 'run_1', available_decisions: ['approve', 'deny'] }]
    await show()
    fireEvent.click(await within(await waitFor(() => card('等你确认'))).findByRole('button', { name: '拒绝' }))
    fireEvent.click(within(card('等你确认')).getByRole('button', { name: '取消' }))
    expect(within(card('等你确认')).queryByRole('textbox')).toBeNull()
    expect(within(card('等你确认')).getByRole('button', { name: '确认' })).toBeInTheDocument()
    expect(approvalDecide).not.toHaveBeenCalled()
  })

  it('says so when the approval it shows was decided elsewhere', async () => {
    store.save(record('chat_a', '周报'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'awaiting_approval', started_at: at }
    approvals = [{ id: 'ap_1', run_id: 'run_1', available_decisions: ['approve', 'deny'] }]
    await show()
    const button = await within(await waitFor(() => card('等你确认'))).findByRole('button', { name: '确认' })
    // Decided elsewhere (the approvals page) meanwhile: the daemon says the
    // one the card shows is not pending (409); the card says so.
    approvals = []
    approvalDecide.mockResolvedValue({ ok: false, status: 409, data: { error: { code: 'conflict', message: 'not pending' } } })
    fireEvent.click(button)
    expect(await screen.findByRole('alert')).toHaveTextContent('这条已处理')
    expect(approvalDecide).toHaveBeenCalledWith('ap_1', { type: 'approve' })
  })

  it('shows a completed task\'s answer with only the sources its run read (Knowledge\'s review)', async () => {
    store.save(record('chat_a', '报销'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = {
      status: 'completed', started_at: at, ended_at: at,
      output: { text: '流程见 doc-7#chk-71 与 doc-9#chk-99' },
      knowledge: { searched: true, read_chunks: [{ knowledge_id: 'doc-7', chunk_id: 'chk-71' }] },
    }
    await show()
    await waitFor(() => expect(card('已完成')).toHaveTextContent('流程见'))
    expect(within(card('已完成')).getByText('经知识库检索回答（agent 模式）')).toBeInTheDocument()
    expect(within(card('已完成')).getByRole('note')).toHaveTextContent('doc-9#chk-99')
    expect(within(card('已完成')).getByRole('note')).not.toHaveTextContent('doc-7#chk-71')
  })

  it('marks sources an answer names when its run had no Knowledge at all', async () => {
    store.save(record('chat_a', '报销'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'completed', started_at: at, ended_at: at, output: { text: '见 doc-7#chk-71' }, knowledge: null }
    await show()
    await waitFor(() => expect(card('已完成')).toHaveTextContent('见'))
    expect(within(card('已完成')).getByRole('note')).toHaveTextContent('doc-7#chk-71')
    // No Knowledge record (e.g. Knowledge Off): nothing said about searching.
    expect(card('已完成').querySelectorAll('.task-card-knowledge')).toHaveLength(1)
  })

  it('shows a card for a schedule\'s run in the schedule\'s conversation (M4 A5)', async () => {
    const { id } = store.create({ title: '日程：周报', title_source: 'schedule', initiator: 'schedule' })
    store.appendRun(id, 'run_s1', 'schedule')
    runs.run_s1 = { status: 'queued', started_at: at }
    await show(id)
    await waitFor(() => expect(card('排队中')).toBeInTheDocument())
    runs.run_s1 = { status: 'running', started_at: at }
    act(() => emit({ type: 'run.started', run_id: 'run_s1' }))
    await waitFor(() => expect(card('运行中')).toBeInTheDocument())
  })

  it('says a task the daemon does not have is not found', async () => {
    store.save(record('chat_a', '周报'))
    store.promoteRun('chat_a', 'run_gone')
    await show()
    await waitFor(() => expect(card('找不到')).toBeInTheDocument())
    // Nothing to stop or decide on a run the daemon does not have (QA #1690 N2).
    expect(within(card('找不到')).queryByRole('button')).toBeNull()
  })
})

describe('hand it to the assistant (M4 A4)', () => {
  it('starts the prompt as a run in a new session and records it as the conversation\'s task', async () => {
    store.save(record('chat_a', '整理周报'))
    await refreshHistory()
    render(<><HandOver conversation="chat_a" prompt="整理周报" /><TaskCards conversation="chat_a" /></>)
    fireEvent.click(screen.getByRole('button', { name: '交给助理办' }))
    await waitFor(() => expect(card('排队中')).toBeInTheDocument())
    expect(proxy).toHaveBeenCalledWith({ method: 'POST', path: '/api/v1/sessions', body: { title: '', channel: 'desktop' } })
    expect(proxy).toHaveBeenCalledWith({ method: 'POST', path: '/api/v1/runs', body: { prompt: '整理周报', session_id: 'sess_new' } })
    expect(store.get('chat_a')?.runs).toEqual([expect.objectContaining({ run_id: 'run_new', kind: 'task' })])
  })

  it('does not leave a run it could not record running unseen', async () => {
    store.save(record('chat_a', '整理周报'))
    await refreshHistory()
    bridge.promoteRun.mockResolvedValueOnce(null)
    render(<HandOver conversation="chat_a" prompt="整理周报" />)
    fireEvent.click(screen.getByRole('button', { name: '交给助理办' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('没能交给助理')
    expect(proxy).toHaveBeenCalledWith({ method: 'POST', path: '/api/v1/runs/run_new/cancel' })
  })

  it('starts no run when the daemon gives it no session', async () => {
    store.save(record('chat_a', '整理周报'))
    await refreshHistory()
    const answer = proxy.getMockImplementation() as (req: unknown) => Promise<unknown>
    proxy.mockImplementation(async (req: { path: string }) =>
      (req.path === '/api/v1/sessions' ? { ok: false, status: 503, data: { error: { code: 'backend_not_ready' } } } : answer(req)))
    render(<HandOver conversation="chat_a" prompt="整理周报" />)
    fireEvent.click(screen.getByRole('button', { name: '交给助理办' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('没能交给助理')
    expect(proxy).not.toHaveBeenCalledWith(expect.objectContaining({ method: 'POST', path: '/api/v1/runs' }))
  })

  it('does not record the run it stopped once main can write again (Codex A4 r2 C1)', async () => {
    store.save(record('chat_a', '整理周报'))
    await refreshHistory()
    bridge.promoteRun.mockRejectedValueOnce(new Error('disk full'))
    render(<HandOver conversation="chat_a" prompt="整理周报" />)
    fireEvent.click(screen.getByRole('button', { name: '交给助理办' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('没能交给助理')
    expect(await saveConversation(record('chat_a', '整理周报'))).toBe(true)
    expect(bridge.promoteRun).toHaveBeenCalledTimes(1)
    expect(store.get('chat_a')?.runs).toEqual([])
  })
})

describe('hand it to the assistant, shown again on its way (Codex A4 C2)', () => {
  it('does not start the same prompt twice while the first is still on its way', async () => {
    store.save(record('chat_a', '整理周报'))
    await refreshHistory()
    const gate = { open: () => {} }
    const answer = proxy.getMockImplementation() as (req: unknown) => Promise<unknown>
    proxy.mockImplementation(async (req: { method: string; path: string }) => {
      if (req.path === '/api/v1/sessions') await new Promise<void>(resolve => { gate.open = resolve })
      return answer(req)
    })
    const first = render(<HandOver conversation="chat_a" prompt="整理周报" />)
    fireEvent.click(screen.getByRole('button', { name: '交给助理办' }))
    // The next turn starts: the button goes, and comes back once it is done.
    first.unmount()
    render(<><HandOver conversation="chat_a" prompt="整理周报" /><TaskCards conversation="chat_a" /></>)
    expect(screen.getByRole('button', { name: '交给助理办' })).toBeDisabled()
    fireEvent.click(screen.getByRole('button', { name: '交给助理办' }))
    await act(async () => { gate.open() })
    await waitFor(() => expect(card('排队中')).toBeInTheDocument())
    expect(screen.getByRole('button', { name: '交给助理办' })).toBeEnabled()
    expect(proxy.mock.calls.filter(([req]) => req.path === '/api/v1/sessions')).toHaveLength(1)
    expect(proxy.mock.calls.filter(([req]) => req.method === 'POST' && req.path === '/api/v1/runs')).toHaveLength(1)
  })
})

describe('a card is not part of the conversation (QA #1690 N3)', () => {
  it('sends neither the card nor its answer with the next turn', async () => {
    const chat: { messages: { role: string; content: string }[] }[] = []
    const answer = proxy.getMockImplementation() as (req: unknown) => Promise<unknown>
    proxy.mockImplementation(async (req: { method: string; path: string; body?: { messages: { role: string; content: string }[] } }) => {
      if (req.path === '/api/v1/chat') {
        chat.push(req.body!)
        const last = req.body!.messages.at(-1)!.content
        return { ok: true, status: 200, data: { message: { content: `reply to ${last}` } } }
      }
      if (req.method === 'POST' && req.path === '/api/v1/runs') {
        const res = await answer(req)
        runs.run_new = { status: 'completed', started_at: at, ended_at: at, output: { text: 'CARD-ANSWER' }, knowledge: null }
        return res
      }
      return answer(req)
    })
    render(<ChatPage />)
    const send = (text: string) => {
      const textarea = screen.getByPlaceholderText(/输入消息/)
      fireEvent.change(textarea, { target: { value: text } })
      fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    }
    bridge.setTitle = wrap((id: string, title: string, source: string) => store.setTitle(id, title, source))
    send('整理周报')
    await screen.findByText('reply to 整理周报')
    fireEvent.click(screen.getByRole('button', { name: '交给助理办' }))
    await waitFor(() => expect(card('已完成')).toHaveTextContent('CARD-ANSWER'))
    send('谢谢')
    await screen.findByText('reply to 谢谢')
    const turn = chat.find(body => body.messages.at(-1)?.content === '谢谢')!
    expect(turn.messages.map(m => m.content)).toEqual(['整理周报', 'reply to 整理周报', '谢谢'])
    // Nor any other request made of the conversation (its title, say).
    for (const body of chat) expect(JSON.stringify(body)).not.toContain('CARD-ANSWER')
    expect(JSON.stringify(turn)).not.toContain('已完成')
  })
})

describe('deleting a conversation stops its tasks still going on (M4 A6, PR-Daemon #1700 L2)', () => {
  const cancelled = () => proxy.mock.calls.map(([req]) => req.path as string).filter(path => path.endsWith('/cancel'))

  it('stops each task of the conversation deleted, asking no status first (Codex A6 C4), and no others', async () => {
    store.save(record('chat_a', '周报'))
    store.save(record('chat_b', '对账'))
    for (const id of ['run_1', 'run_2', 'run_3', 'run_done']) store.promoteRun('chat_a', id)
    store.promoteRun('chat_b', 'run_b')
    runs.run_1 = { status: 'running', started_at: at }
    runs.run_b = { status: 'running', started_at: at }
    await refreshHistory()
    proxy.mockClear()
    await deleteConversation('chat_a')
    expect(cancelled().sort()).toEqual(['run_1', 'run_2', 'run_3', 'run_done'].map(id => `/api/v1/runs/${id}/cancel`))
    expect(proxy.mock.calls.filter(([req]) => req.method !== 'POST')).toEqual([])
    expect(store.get('chat_a')).toBeNull()
    expect(store.get('chat_b')).not.toBeNull()
  })

  it('stops the tasks of every conversation when all are deleted', async () => {
    store.save(record('chat_a', '周报'))
    store.save(record('chat_b', '对账'))
    store.promoteRun('chat_a', 'run_1')
    store.promoteRun('chat_b', 'run_b')
    await refreshHistory()
    await clearConversations()
    expect(cancelled().sort()).toEqual(['/api/v1/runs/run_1/cancel', '/api/v1/runs/run_b/cancel'])
    expect(store.list()).toEqual([])
  })

  it('takes a run the daemon no longer has as stopped', async () => {
    store.save(record('chat_a', '周报'))
    store.promoteRun('chat_a', 'run_gone')
    await refreshHistory()
    proxy.mockResolvedValue({ ok: false, status: 404, data: null })
    await deleteConversation('chat_a')
    expect(store.get('chat_a')).toBeNull()
  })

  it.each([
    ['refuses', async () => ({ ok: false, status: 503, data: { error: { code: 'backend_not_ready' } } })],
    ['cannot be reached', async () => { throw new Error('offline') }],
  ])('deletes nothing when the daemon %s a task\'s cancel, and says so (Codex A6 C1)', async (_, answer) => {
    store.save(record('chat_a', '周报'))
    store.save(record('chat_b', '对账'))
    store.promoteRun('chat_a', 'run_1')
    store.promoteRun('chat_a', 'run_2')
    await refreshHistory()
    proxy.mockImplementation(async (req: { path: string }) => (req.path.includes('run_2') ? answer() : { ok: true, status: 202, data: {} }))
    await expect(deleteConversation('chat_a')).rejects.toThrow(TASKS_NOT_STOPPED)
    await expect(clearConversations()).rejects.toThrow(TASKS_NOT_STOPPED)
    expect(store.get('chat_a')).not.toBeNull()
    expect(store.get('chat_b')).not.toBeNull()
  })

  it('starts the new chat of a clearing only once the tasks are stopped (Codex A6 C2)', async () => {
    store.save(record('chat_a', '周报'))
    store.promoteRun('chat_a', 'run_1')
    await refreshHistory()
    const starter = vi.fn()
    registerChatPage(() => {}, starter)
    let stop!: () => void
    proxy.mockImplementation(() => new Promise(resolve => { stop = () => resolve({ ok: true, status: 202, data: {} }) }))
    const clearing = clearConversations()
    await vi.waitFor(() => expect(stop).toBeTypeOf('function'))
    expect(starter).not.toHaveBeenCalled()
    expect(bridge.clear).not.toHaveBeenCalled()
    stop()
    await clearing
    expect(starter).toHaveBeenCalledTimes(1)
    expect(store.list()).toEqual([])
  })

  it.each(['delete', 'clear'])('does not %s the open conversation when a reply starts in it while its tasks are stopped', async (what) => {
    store.save(record('chat_a', '周报'))
    store.promoteRun('chat_a', 'run_1')
    await refreshHistory()
    setActiveConversation('chat_a')
    let stop!: () => void
    proxy.mockImplementation(() => new Promise(resolve => { stop = () => resolve({ ok: true, status: 202, data: {} }) }))
    const going = what === 'delete' ? deleteConversation('chat_a') : clearConversations()
    await vi.waitFor(() => expect(stop).toBeTypeOf('function'))
    setChatBusy(true)
    stop()
    await going
    expect(store.get('chat_a')).not.toBeNull()
    setChatBusy(false)
  })

  it('asks the daemon nothing for a conversation without tasks', async () => {
    store.save(record('chat_a', '问'))
    await refreshHistory()
    await deleteConversation('chat_a')
    expect(proxy).not.toHaveBeenCalled()
  })
})

describe('elapsed', () => {
  it.each([[0, '00:00'], [65_000, '01:05'], [3_725_000, '1:02:05'], [-5, '00:00']])('%i ms is %s', (ms, text) => {
    expect(elapsed(ms)).toBe(text)
  })
})

describe('approvalText (D2b)', () => {
  const t = (key: string, vars?: Record<string, string>) => `${key}${vars ? JSON.stringify(vars) : ''}`
  it('says what a documents tool will do', () => {
    expect(approvalText({ summary: 'documents.list: {"limit":10}' }, t)).toBe('task.ask.list')
    expect(approvalText({ summary: 'documents.compose: {"parts":[{"document_id":"a.pdf","pages":"1"}],"title":"A1"}' }, t))
      .toBe('task.ask.compose{"title":"A1","parts":"task.ask.part{\\"doc\\":\\"a.pdf\\",\\"pages\\":\\"1\\"}"}')
    // M3/PM 11-21: never raw JSON; the summary goes under 「详情」.
    expect(approvalText({ summary: 'shell_exec: {"argv":["ls"]}' }, t)).toBe('task.ask.tool{"tool":"执行命令"}')
  })
  it('says what a fill or a patch will write (E1b/D5a)', () => {
    expect(approvalText({ summary: 'documents.fill: {"document_id":"模板.docx","values":{"甲方":"明远","租金":"8,500"}}' }, t))
      .toBe('task.ask.fill{"doc":"模板.docx","values":"甲方=明远，租金=8,500"}')
    expect(approvalText({ summary: 'documents.patch: {"document_id":"a.docx","edits":[{"index":3,"text":"x","tracked":true}]}' }, t))
      .toBe('task.ask.patchTracked{"doc":"a.docx","n":"1"}\ntask.ask.patchUnread{"n":"4","to":"x"}')
    expect(approvalText({ summary: 'documents.patch: {"document_id":"a.docx","edits":[{"index":3,"text":"x"}]}' }, t))
      .toBe('task.ask.patch{"doc":"a.docx","n":"1"}\ntask.ask.patchUnread{"n":"4","to":"x"}')
  })
  it('shows the paragraph the document has at that index, not the model\'s expect (M1, 0.6.75)', () => {
    const summary = 'documents.patch: {"document_id":"合同.docx","edits":[{"index":5,"text":"押金为两个月租金。","expect":"押金为两个月租金"}]}'
    const originals = new Map([[5, '第三条 押金为两个月租金，合同期满无违约时全额退还。']])
    expect(approvalText({ summary }, t, originals))
      .toBe('task.ask.patch{"doc":"合同.docx","n":"1"}\ntask.ask.patchEdit{"n":"6","from":"第三条 押金为两个月租金，合同期满无违约时全额退还。","to":"押金为两个月租金。"}')
  })
  // M1 (0.6.78): the daemon cuts the summary at about 200 bytes; the card
  // showed raw JSON and read no paragraph.
  it('reads a patch off a summary the daemon cut short', async () => {
    const summary = 'documents.patch: {"document_id":"合同-链接与脚注.docx","edits":[{"expect":"原有原文内容","index":2,"text":"经双方协商一致，本合同项下权利义务自双方签字盖章之日起生效，具有… [truncated 85 bytes]'
    const originals = new Map([[2, '乙方：张三']])
    expect(approvalText({ summary }, t, originals))
      .toBe('task.ask.patch{"doc":"合同-链接与脚注.docx","n":"task.ask.atLeast{\\"n\\":\\"1\\"}"}\ntask.ask.patchEdit{"n":"3","from":"乙方：张三","to":"经双方协商一致，本合同项下权利义务自双方签字盖章之日起生效，具有…"}')
    const request = vi.fn(async (req: { op: string }) => req.op === 'list'
      ? { ok: true, data: { documents: [{ document_id: 'doc_9', title: '合同-链接与脚注.docx', head_revision: 1, created_at: '' }] } }
      : { ok: true, data: { paragraphs: [{ index: 2, text: '乙方：张三', in_table: false }] } })
    expect((await patchOriginals({ request } as never, { summary }))?.get(2)).toBe('乙方：张三')
  })
  // PR-Daemon #1986 A: the summary is cut at about 200 bytes; the card reads
  // the approval's full payload. Three tracked edits, index 12, CJK anchors.
  it('reads a patch from the approval payload, not the cut summary', () => {
    const edits = [12, 13, 14].map(index => ({ anchor: '第十二条 押金为两个月租金，合同', index, text: '押金金额为两个月的租金，租赁期满且无违约行为时全额返还。', tracked: true }))
    const payload = { document_id: '合同.docx', edits }
    const full = `documents.patch: ${JSON.stringify(payload)}`
    const summary = `${full.slice(0, 200)}… [truncated ${full.length - 200} bytes]`
    const originals = new Map([[12, 'a'], [13, 'b'], [14, 'c']])
    const text = approvalText({ summary, payload }, t, originals)
    expect(text.split('\n')[0]).toBe('task.ask.patchTracked{"doc":"合同.docx","n":"3"}')
    expect(text.split('\n').slice(1).map(l => JSON.parse(l.slice('task.ask.patchEdit'.length)).n)).toEqual(['13', '14', '15'])
    expect(approvalArgs({ summary, payload }).complete).toBe(true)
    // Without the payload: what is there, said as 「at least」, and not complete.
    const cut = approvalArgs({ summary })
    expect(cut.complete).toBe(false)
    expect(approvalText({ summary }, t)).toContain('task.ask.atLeast')
    // PR-Daemon #1987 Low 2: a patch payload without its edits is not whole.
    expect(approvalArgs({ summary, payload: { document_id: '合同.docx' } }).complete).toBe(false)
  })

  it('patchOriginals reads the paragraphs of the document the model named by title', async () => {
    const request = vi.fn(async (req: { op: string }) => req.op === 'list'
      ? { ok: true, data: { documents: [{ document_id: 'doc_9', title: '合同.docx', head_revision: 1, created_at: '' }] } }
      : { ok: true, data: { paragraphs: [{ index: 5, text: '第三条 押金…', in_table: false }] } })
    const got = await patchOriginals({ request } as never, { summary: 'documents.patch: {"document_id":"《合同.docx》","edits":[]}' })
    expect(got?.get(5)).toBe('第三条 押金…')
    expect(request).toHaveBeenCalledWith({ op: 'paragraphs', documentId: 'doc_9' })
    expect(await patchOriginals({ request } as never, { summary: 'documents.fill: {}' })).toBeNull()
  })
  it('shows what each rewritten paragraph says now and will say (M1, 0.6.72)', () => {
    // The run that would have put 第三条's text over 乙方's line.
    const summary = 'documents.patch: {"document_id":"合同.docx","edits":[{"index":2,"text":"第三条 押金为两个月租金。","expect":"乙方：张三","tracked":true}]}'
    expect(approvalText({ summary }, t))
      .toBe('task.ask.patchTracked{"doc":"合同.docx","n":"1"}\ntask.ask.patchUnread{"n":"3","to":"第三条 押金为两个月租金。"}')
    // Read: the paragraph as the document has it, not the model's expect.
    expect(approvalText({ summary }, t, new Map([[2, '乙方：张三']])))
      .toBe('task.ask.patchTracked{"doc":"合同.docx","n":"1"}\ntask.ask.patchEdit{"n":"3","from":"乙方：张三","to":"第三条 押金为两个月租金。"}')
  })
})
