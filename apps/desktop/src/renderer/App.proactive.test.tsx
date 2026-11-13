// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { App } from './App'
import { setLocale } from './i18n'
import { resetHistoryStore } from './pages/chat/history'
import { resetRunStatusesForTests } from './pages/chat/run-status'
import { CHAT_HISTORY_FORMAT, CHAT_HISTORY_VERSION, chatHistorySummary, type ChatHistoryRecord, type ChatHistorySummary } from '../shared/chat-history'

// M6 C2: the app puts the banner on top of the main chat and marks the open
// conversation seen while the chat shows and the window has the focus
// (docs/ui/ASSISTANT-C.md §7). Its parts are tested in
// pages/chat/proactive.test.tsx; this is the wiring in App.tsx.

const at = '2026-11-09T01:00:00.000Z'
let records: Map<string, ChatHistoryRecord>
let changed: ((summary: ChatHistorySummary) => void) | null
const chatHistory = {
  list: vi.fn(async () => [...records.values()].map(chatHistorySummary)),
  get: vi.fn(async (id: string) => records.get(id) ?? null),
  save: vi.fn(async (r: ChatHistoryRecord) => chatHistorySummary(r)),
  markSeen: vi.fn(async (id: string) => {
    const r = records.get(id)
    if (!r) return null
    records.set(id, { ...r, unseen: [] })
    return chatHistorySummary(records.get(id)!)
  }),
  onChanged: vi.fn((cb: (summary: ChatHistorySummary) => void) => { changed = cb; return () => { changed = null } }),
}
/** A schedule's conversation with these runs, all unseen. */
function scheduleRecord(id: string, ...runIds: string[]): ChatHistoryRecord {
  return {
    format: CHAT_HISTORY_FORMAT, version: CHAT_HISTORY_VERSION, id, title: '日程：周报', title_source: 'schedule',
    group_id: null, archived_at: null, created_at: at, updated_at: at, messages: [], pending: [],
    runs: runIds.map(run_id => ({ run_id, kind: 'schedule' as const, started_at: at })),
    initiator: 'schedule', unseen: [...runIds],
  }
}

beforeEach(() => {
  setLocale('zh')
  vi.clearAllMocks()
  records = new Map()
  changed = null
  resetHistoryStore()
  resetRunStatusesForTests()
  vi.spyOn(document, 'hasFocus').mockReturnValue(true)
  Object.defineProperty(window, 'agent24', {
    value: {
      backendProxy: vi.fn().mockResolvedValue({ ok: true, status: 200, data: { status: 'ok', ts: Date.now(), schedules: [], runs: [] } }),
      getAppVersion: vi.fn().mockResolvedValue('1.0.0'),
      omlxDetect: vi.fn().mockResolvedValue({ ok: true, models: ['Qwen3-8B-4bit'] }),
      omlxStart: vi.fn(),
      omlxModels: vi.fn(),
      backendEndpoint: vi.fn().mockResolvedValue({ port: 60128 }),
      getNavPrefs: vi.fn().mockResolvedValue({ pinned: [], slotCount: 3, showOverview: false }),
      setNavPrefs: vi.fn().mockResolvedValue(undefined),
      agentearSnapshot: vi.fn().mockResolvedValue([]),
      modelCallSnapshot: vi.fn().mockResolvedValue([]),
      onAgentEarEvent: vi.fn(() => () => {}),
      onModelCallEvent: vi.fn(() => () => {}),
      onRunEvent: vi.fn(() => () => {}),
      chatHistory,
      documents: {
        request: vi.fn(() => Promise.resolve({ ok: true, status: 200, data: { documents: [], next_cursor: null } })),
        onImportProgress: () => () => {},
        onEvent: () => () => {},
      },
    },
    writable: true,
    configurable: true,
  })
})
afterEach(() => { vi.restoreAllMocks() })

describe('the proactive entries in the app (M6 C2)', () => {
  it('a press on the banner marks the conversation seen when it is the one open already, after a mark that failed (Codex C2 R1 C1)', async () => {
    const asked = { ...scheduleRecord('chat_sch_1', 'run_1'), unseen: [] as string[],
      messages: [{ role: 'user' as const, content: '这周的呢', at, delivered: true }] }
    records.set('chat_sch_1', { ...asked, unseen: ['run_1'] })
    await act(async () => { render(<App />) })
    await act(async () => { fireEvent.click(await screen.findByRole('button', { name: /助理有 1 件事/ })) })
    await waitFor(() => expect(screen.getByText('这周的呢')).toBeInTheDocument())
    await waitFor(() => expect(screen.queryByRole('button', { name: /助理有 \d+ 件事/ })).toBeNull())
    // A run comes while it is open; marking it seen fails once.
    chatHistory.markSeen.mockRejectedValueOnce(new Error('EIO'))
    const before = chatHistory.markSeen.mock.calls.length
    records.set('chat_sch_1', { ...scheduleRecord('chat_sch_1', 'run_1', 'run_2'), messages: asked.messages, unseen: ['run_2'] })
    await act(async () => { changed?.(chatHistorySummary(records.get('chat_sch_1')!)) })
    await waitFor(() => expect(chatHistory.markSeen).toHaveBeenCalledTimes(before + 1))
    const banner = await screen.findByRole('button', { name: /助理有 1 件事/ })
    // The banner's target is the one open: a press marks it seen.
    await act(async () => { fireEvent.click(banner) })
    await waitFor(() => expect(chatHistory.markSeen).toHaveBeenCalledTimes(before + 2))
    await waitFor(() => expect(screen.queryByRole('button', { name: /助理有 \d+ 件事/ })).toBeNull())
  })

  it('shows the banner on top of the main chat; a press opens the conversation, which is then seen', async () => {
    records.set('chat_sch_1', scheduleRecord('chat_sch_1', 'run_1', 'run_2'))
    await act(async () => { render(<App />) })
    const banner = await screen.findByRole('button', { name: '助理有 2 件事你还没看，打开最早的一件' })
    expect(banner).toHaveTextContent('助理有话说 (2)')
    await act(async () => { fireEvent.click(banner) })
    await waitFor(() => expect(chatHistory.markSeen).toHaveBeenCalledWith('chat_sch_1'))
    await waitFor(() => expect(screen.queryByRole('button', { name: /助理有 \d+ 件事/ })).toBeNull())
  })

  it('marks the open conversation seen when a run comes while the chat shows, not while another page does', async () => {
    records.set('chat_sch_1', scheduleRecord('chat_sch_1', 'run_1'))
    await act(async () => { render(<App />) })
    await act(async () => { fireEvent.click(await screen.findByRole('button', { name: /助理有 1 件事/ })) })
    await waitFor(() => expect(screen.queryByRole('button', { name: /助理有 \d+ 件事/ })).toBeNull())
    const seen = chatHistory.markSeen.mock.calls.length

    // A run comes while it is open and the chat shows: seen at once.
    records.set('chat_sch_1', { ...scheduleRecord('chat_sch_1', 'run_1', 'run_2'), unseen: ['run_2'] })
    await act(async () => { changed?.(chatHistorySummary(records.get('chat_sch_1')!)) })
    await waitFor(() => expect(chatHistory.markSeen).toHaveBeenCalledTimes(seen + 1))

    // Another page: a run that comes waits.
    fireEvent.click(screen.getByRole('button', { name: '能力中心' }))
    records.set('chat_sch_1', { ...scheduleRecord('chat_sch_1', 'run_1', 'run_2', 'run_3'), unseen: ['run_3'] })
    await act(async () => { changed?.(chatHistorySummary(records.get('chat_sch_1')!)) })
    await act(async () => { await new Promise(r => setTimeout(r, 0)) })
    expect(chatHistory.markSeen).toHaveBeenCalledTimes(seen + 1)
  })
})
