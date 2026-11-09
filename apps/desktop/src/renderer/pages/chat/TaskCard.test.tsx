// @vitest-environment jsdom
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { createChatHistoryStore } from '../../../main/chat-history'
import { CHAT_HISTORY_FORMAT, CHAT_HISTORY_VERSION, type ChatHistoryRecord } from '../../../shared/chat-history'
import type { RunEvent } from '../../../shared/ipc-types'
import { HandOver, TaskCards, elapsed, resetHandOversForTests } from './TaskCard'
import { refreshHistory, resetHistoryStore, saveConversation } from './history'
import { resetRunStatusesForTests } from './run-status'
import { setLocale } from '../../i18n'

// M4 A4: a conversation's tasks as cards, against main's real store and a
// daemon the test plays, with its /events.

let dir: string
let store: ReturnType<typeof createChatHistoryStore>
const wrap = <A extends unknown[], R>(f: (...a: A) => R) => vi.fn(async (...a: A) => f(...a))
type Run = { status: string; started_at?: string; ended_at?: string | null; output?: { text: string } | null; knowledge?: unknown; error?: { code: string; message: string } | null }
let runs: Record<string, Run>
let approvals: { id: string; run_id: string; available_decisions: string[] }[]
let proxy: ReturnType<typeof vi.fn>
let approvalDecide: ReturnType<typeof vi.fn>
let emit: (event: RunEvent) => void
let bridge: Record<string, ReturnType<typeof vi.fn>>

const at = '2026-11-09T01:00:00.000Z'
function record(id: string, prompt: string): ChatHistoryRecord {
  return {
    format: CHAT_HISTORY_FORMAT, version: CHAT_HISTORY_VERSION, id, title: prompt, title_source: 'prompt',
    group_id: null, archived_at: null, created_at: at, updated_at: at, runs: [], pending: [],
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
    const id = decodeURIComponent(req.path.replace('/api/v1/runs/', ''))
    return id in runs ? { ok: true, status: 200, data: { id, ...runs[id] } } : { ok: false, status: 404, data: null }
  })
  approvalDecide = vi.fn(async () => ({ ok: true, status: 200, data: {} }))
  const listeners = new Set<(e: RunEvent) => void>()
  emit = event => { for (const l of listeners) l(event) }
  bridge = {
    list: wrap(() => store.list()), get: wrap((id: string) => store.get(id)), save: wrap((r: unknown) => store.save(r)),
    promoteRun: wrap((id: string, runId: string) => store.promoteRun(id, runId)),
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

  it('stops a task with the cancel API', async () => {
    store.save(record('chat_a', '周报'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'running', started_at: at }
    await show()
    fireEvent.click(await within(await waitFor(() => card('运行中'))).findByRole('button', { name: '停止' }))
    await waitFor(() => expect(card('已停止')).toBeInTheDocument())
    expect(proxy).toHaveBeenCalledWith({ method: 'POST', path: '/api/v1/runs/run_1/cancel' })
  })

  it('confirms the approval its run waits on, with its first decision other than deny, through main\'s dialog', async () => {
    store.save(record('chat_a', '周报'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'awaiting_approval', started_at: at }
    approvals = [{ id: 'ap_other', run_id: 'run_2', available_decisions: ['approve'] }, { id: 'ap_1', run_id: 'run_1', available_decisions: ['deny', 'allow_once', 'allow_always'] }]
    await show()
    fireEvent.click(await within(await waitFor(() => card('等你确认'))).findByRole('button', { name: '确认' }))
    await waitFor(() => expect(approvalDecide).toHaveBeenCalledWith('ap_1', { type: 'allow_once' }))
  })

  it('says so when there is nothing to confirm any more', async () => {
    store.save(record('chat_a', '周报'))
    store.promoteRun('chat_a', 'run_1')
    runs.run_1 = { status: 'awaiting_approval', started_at: at }
    await show()
    fireEvent.click(await within(await waitFor(() => card('等你确认'))).findByRole('button', { name: '确认' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('现在没有要确认的事')
    expect(approvalDecide).not.toHaveBeenCalled()
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

  it('says a task the daemon does not have is not found', async () => {
    store.save(record('chat_a', '周报'))
    store.promoteRun('chat_a', 'run_gone')
    await show()
    await waitFor(() => expect(card('找不到')).toBeInTheDocument())
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

describe('elapsed', () => {
  it.each([[0, '00:00'], [65_000, '01:05'], [3_725_000, '1:02:05'], [-5, '00:00']])('%i ms is %s', (ms, text) => {
    expect(elapsed(ms)).toBe(text)
  })
})
