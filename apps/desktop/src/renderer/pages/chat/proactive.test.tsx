// @vitest-environment jsdom
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { createChatHistoryStore } from '../../../main/chat-history'
import { CHAT_HISTORY_FORMAT, CHAT_HISTORY_VERSION, type ChatHistoryRecord, type ChatHistorySummary } from '../../../shared/chat-history'
import type { RunEvent } from '../../../shared/ipc-types'
import { Sidebar } from '../../capabilities/Sidebar'
import { CapabilitiesContext, NavPrefsContext } from '../../capabilities/types'
import { CAPABILITIES } from '../../capabilities/registry'
import { setLocale } from '../../i18n'
import { getHistoryState, refreshHistory, registerChatPage, resetHistoryStore, setActiveConversation, setChatBusy } from './history'
import { ProactiveBanner } from './ProactiveBanner'
import { BANNER_CAP, bannerCount, bannerNumber, bannerTarget, mineCount, useSeeingOpenConversation } from './proactive'
import { resetRunStatusesForTests } from './run-status'

// M6 C2: the banner on top of the main chat and the sidebar's "Needs you"
// (docs/ui/ASSISTANT-C.md §7), against main's real store.

let dir: string
let store: ReturnType<typeof createChatHistoryStore>
/** The daemon's runs: their status, or absent (404). */
let runs: Record<string, string>
const wrap = <A extends unknown[], R>(f: (...a: A) => R) => vi.fn(async (...a: A) => f(...a))
let bridge: Record<string, ReturnType<typeof vi.fn>>
let emit: (event: RunEvent) => void

/** A conversation the user started, saved by the page. */
function userRecord(id: string, hour: number): ChatHistoryRecord {
  const at = `2026-11-09T${String(hour).padStart(2, '0')}:00:00.000Z`
  return {
    format: CHAT_HISTORY_FORMAT, version: CHAT_HISTORY_VERSION, id, title: id, title_source: 'prompt',
    group_id: null, archived_at: null, created_at: at, updated_at: at, runs: [], pending: [], initiator: 'user', unseen: [],
    messages: [{ role: 'user', content: id, at, delivered: true }],
  }
}
/** A schedule's conversation with these runs recorded, in order. */
function scheduleWith(...runIds: string[]): string {
  const { id } = store.create({ title: `日程 ${runIds.join(',')}`, title_source: 'schedule', initiator: 'schedule' })
  for (const r of runIds) store.appendRun(id, r, 'schedule')
  return id
}
const summary = (patch: Partial<ChatHistorySummary>): ChatHistorySummary => ({
  id: 'chat_x', title: 'x', title_source: 'schedule', group_id: null, archived_at: null,
  created_at: '2026-11-09T00:00:00.000Z', updated_at: '2026-11-09T00:00:00.000Z', pending: [],
  initiator: 'schedule', unseen: [], first_prompt: '', tasks: [], ...patch,
})
const task = (run_id: string, started_at: string) => ({ run_id, kind: 'schedule' as const, started_at })

beforeEach(() => {
  setLocale('zh')
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'proactive-'))
  store = createChatHistoryStore(dir)
  runs = {}
  const proxy = vi.fn(async (req: { method: string; path: string }) => {
    const id = decodeURIComponent(req.path.replace('/api/v1/runs/', ''))
    return id in runs ? { ok: true, status: 200, data: { id, status: runs[id] } } : { ok: false, status: 404, data: null }
  })
  bridge = {
    list: wrap(() => store.list()), get: wrap((id: string) => store.get(id)), save: wrap((r: unknown) => store.save(r)),
    delete: wrap((id: string) => store.delete(id)), clear: wrap(() => store.clear()),
    organize: wrap((id: string, p: unknown) => store.organize(id, p)), markSeen: wrap((id: string) => store.markSeen(id)),
  }
  const chatGroups = { get: wrap(() => store.groups()), setCollapsed: wrap((t: string, c: boolean) => store.setCollapsed(t, c)) }
  const listeners = new Set<(e: RunEvent) => void>()
  emit = event => { for (const l of listeners) l(event) }
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
afterEach(() => {
  vi.restoreAllMocks()
  fs.rmSync(dir, { recursive: true, force: true })
})

describe('what the two entries count (M6 C2)', () => {
  it('bannerCount: the unseen runs of the conversations the user did not start, archived ones left out (Q1)', () => {
    expect(bannerCount([])).toBe(0)
    expect(bannerCount([
      summary({ id: 'chat_a', unseen: ['r1', 'r2'] }),
      summary({ id: 'chat_b', initiator: 'assistant', unseen: ['r3'] }),
      summary({ id: 'chat_c', archived_at: '2026-11-09T01:00:00.000Z', unseen: ['r4', 'r5'] }),
      summary({ id: 'chat_d', initiator: 'user', unseen: [] }),
    ])).toBe(3)
  })

  it.each([[0, '0'], [1, '1'], [BANNER_CAP, '99'], [BANNER_CAP + 1, '99+'], [4321, '99+']])('bannerNumber(%i) is %s (Q3)', (n, shown) => {
    expect(bannerNumber(n)).toBe(shown)
  })

  it('bannerTarget: the conversation whose earliest unseen run came first; a tie goes to the smaller id (Q2)', () => {
    const early = '2026-11-09T01:00:00.000Z'
    const mid = '2026-11-09T02:00:00.000Z'
    const late = '2026-11-09T03:00:00.000Z'
    expect(bannerTarget([])).toBeNull()
    expect(bannerTarget([summary({ id: 'chat_a', tasks: [task('r1', early)] })])).toBeNull()
    const items = [
      summary({ id: 'chat_new', tasks: [task('r9', late)], unseen: ['r9'] }),
      // Its first run is the earliest of all, but seen: its unseen one is later.
      summary({ id: 'chat_seen', tasks: [task('r0', '2026-11-09T00:00:00.000Z'), task('r5', late)], unseen: ['r5'] }),
      summary({ id: 'chat_old', tasks: [task('r2', mid)], unseen: ['r2'] }),
      // Earlier still, but archived, or the user's: never the target.
      summary({ id: 'chat_arch', archived_at: late, tasks: [task('r3', early)], unseen: ['r3'] }),
    ]
    expect(bannerTarget(items)).toBe('chat_old')
    expect(bannerTarget([summary({ id: 'chat_b', tasks: [task('r1', mid)], unseen: ['r1'] }), summary({ id: 'chat_a', tasks: [task('r2', mid)], unseen: ['r2'] })])).toBe('chat_a')
    // An unseen run not among its tasks still reaches it, as of updated_at.
    expect(bannerTarget([summary({ id: 'chat_z', updated_at: mid, unseen: ['r7'] })])).toBe('chat_z')
  })

  it('mineCount: the conversations not archived under "Needs you" (T-1, as the list counts)', () => {
    const status = (id: string) => ({ r_wait: 'awaiting_approval', r_run: 'running' } as Record<string, string>)[id]
    const pending = [{ id: 'p1', kind: 'confirm' as const, label: '确认', at: '2026-11-09T00:00:00.000Z' }]
    expect(mineCount([
      summary({ id: 'chat_a', initiator: 'user', pending }),
      summary({ id: 'chat_b', tasks: [task('r_wait', '2026-11-09T00:00:00.000Z')] }),
      summary({ id: 'chat_c', tasks: [task('r_run', '2026-11-09T00:00:00.000Z')] }),
      summary({ id: 'chat_d', archived_at: '2026-11-09T01:00:00.000Z', pending }),
      // Unseen is not "Needs you" (T-1: counted apart).
      summary({ id: 'chat_e', unseen: ['r_x'], tasks: [task('r_x', '2026-11-09T00:00:00.000Z')] }),
    ], status)).toBe(2)
  })
})

describe('the banner on top of the main chat (M6 C2)', () => {
  it('is not there while nothing is unseen, nor for the user\'s own conversations', async () => {
    store.save(userRecord('chat_mine', 1))
    await refreshHistory()
    render(<ProactiveBanner />)
    expect(screen.queryByRole('button', { name: /助理/ })).toBeNull()
  })

  it('says how many runs are unseen, and opens the conversation whose earliest unseen run came first', async () => {
    const first = scheduleWith('run_a1', 'run_a2')
    scheduleWith('run_b1')
    const open = vi.fn()
    registerChatPage(open, () => {})
    await refreshHistory()
    render(<ProactiveBanner />)
    const banner = await screen.findByRole('button', { name: '助理有 3 件事你还没看，打开最早的一件' })
    expect(banner).toHaveTextContent('助理有话说 (3)')
    expect(banner.tagName).toBe('BUTTON')
    expect(banner).not.toBeDisabled()
    fireEvent.click(banner)
    expect(open).toHaveBeenCalledWith(first)
  })

  it.each([[100], [120]])('caps what it shows at 99+, and keeps the exact number in its name and title (Q3): n = %i', async n => {
    scheduleWith(...Array.from({ length: n }, (_, i) => `run_${i}`))
    await refreshHistory()
    render(<ProactiveBanner />)
    const label = `助理有 ${n} 件事你还没看，打开最早的一件`
    const banner = await screen.findByRole('button', { name: label })
    expect(banner).toHaveTextContent('助理有话说 (99+)')
    expect(banner).toHaveAttribute('title', label)
  })

  it('leaves archived conversations out (Q1)', async () => {
    const id = scheduleWith('run_a1')
    store.organize(id, { archived: true })
    scheduleWith('run_b1')
    await refreshHistory()
    render(<ProactiveBanner />)
    expect(await screen.findByRole('button', { name: /助理有 1 件事/ })).toHaveTextContent('助理有话说 (1)')
  })

  it('is shown but does nothing while a reply is in flight', async () => {
    scheduleWith('run_a1')
    const open = vi.fn()
    registerChatPage(open, () => {})
    await refreshHistory()
    setChatBusy(true)
    render(<ProactiveBanner />)
    const banner = await screen.findByRole('button', { name: /助理有 1 件事/ })
    expect(banner).toBeDisabled()
    fireEvent.click(banner)
    expect(open).not.toHaveBeenCalled()
  })

  it('follows the data: a new unseen run raises n, opening the conversation takes it away', async () => {
    const id = scheduleWith('run_a1')
    await refreshHistory()
    render(<ProactiveBanner />)
    await screen.findByRole('button', { name: /助理有 1 件事/ })
    store.appendRun(id, 'run_a2', 'schedule')
    await act(() => refreshHistory())
    await screen.findByRole('button', { name: /助理有 2 件事/ })
    store.markSeen(id)
    await act(() => refreshHistory())
    await waitFor(() => expect(screen.queryByRole('button', { name: /助理有/ })).toBeNull())
  })
})

describe('the sidebar\'s "Needs you" (M6 C2, T-1)', () => {
  function renderSidebar() {
    return render(
      <CapabilitiesContext.Provider value={{ defs: CAPABILITIES, open: vi.fn() }}>
        <NavPrefsContext.Provider value={{ prefs: { pinned: [], slotCount: 3, showOverview: false }, setPrefs: vi.fn() }}>
          <Sidebar activeId="assistant" navigate={vi.fn()} />
        </NavPrefsContext.Provider>
      </CapabilitiesContext.Provider>,
    )
  }
  const assistant = () => screen.getAllByRole('button').find(b => b.textContent?.startsWith('助理'))!

  it('counts the conversations not archived with something waiting on the user or a task waiting on an approval; not what is unseen', async () => {
    const waiting = scheduleWith('run_wait')
    runs.run_wait = 'awaiting_approval'
    // QA N6 under the new count: an approval of a run no conversation has a
    // card for is put in the assistant's own conversation by main (M4 A6,
    // main.ts approval.required / catch-up), and counts from there.
    const cli = store.create({ title: '助理发起', title_source: 'assistant', initiator: 'assistant' }).id
    store.appendRun(cli, 'run_cli', 'task')
    runs.run_cli = 'awaiting_approval'
    scheduleWith('run_done')
    runs.run_done = 'completed'
    store.save(userRecord('chat_confirm', 2))
    store.setPending('chat_confirm', { id: 'p1', kind: 'confirm', label: '要不要记下来？', at: '2026-11-09T02:00:00.000Z' })
    store.save(userRecord('chat_archived', 3))
    store.setPending('chat_archived', { id: 'p2', kind: 'confirm', label: '还要吗？', at: '2026-11-09T03:00:00.000Z' })
    store.organize('chat_archived', { archived: true })
    await refreshHistory()
    renderSidebar()
    await waitFor(() => expect(assistant()).toHaveTextContent(/^助理3$/))
    // Decided, confirmed: none left. Seeing what is unseen changes nothing.
    runs.run_wait = 'running'
    runs.run_cli = 'completed'
    store.markSeen(waiting)
    store.clearPending('chat_confirm', 'p1')
    await act(() => refreshHistory())
    act(() => emit({ type: 'approval.resolved', run_id: 'run_wait' }))
    act(() => emit({ type: 'run.completed', run_id: 'run_cli' }))
    await waitFor(() => expect(assistant()).toHaveTextContent(/^助理$/))
  })

  it('counts a conversation once, however many of its tasks wait on an approval (§7.2a, Codex C2 R1 C2)', async () => {
    const cli = store.create({ title: '助理发起', title_source: 'assistant', initiator: 'assistant' }).id
    store.appendRun(cli, 'run_cli1', 'task')
    store.appendRun(cli, 'run_cli2', 'task')
    runs.run_cli1 = 'awaiting_approval'
    runs.run_cli2 = 'awaiting_approval'
    await refreshHistory()
    renderSidebar()
    await waitFor(() => expect(assistant()).toHaveTextContent(/^助理1$/))
    // One decided, the other still waits: still one.
    runs.run_cli1 = 'running'
    act(() => emit({ type: 'approval.resolved', run_id: 'run_cli1' }))
    await act(async () => { await new Promise(r => setTimeout(r, 50)) })
    expect(assistant()).toHaveTextContent(/^助理1$/)
    runs.run_cli2 = 'completed'
    act(() => emit({ type: 'run.completed', run_id: 'run_cli2' }))
    await waitFor(() => expect(assistant()).toHaveTextContent(/^助理$/))
  })

  it('asks the daemon no more for pending approvals: the badge is the list\'s (no /api/v1/approvals poll)', async () => {
    await refreshHistory()
    renderSidebar()
    const proxy = window.agent24.backendProxy as unknown as ReturnType<typeof vi.fn>
    expect(proxy.mock.calls.some(([req]) => String(req.path).startsWith('/api/v1/approvals'))).toBe(false)
  })
})

describe('the open conversation is seen while the chat shows and the window has the focus (M6 C2 Q4)', () => {
  function Seeing({ shown }: { shown: boolean }) {
    useSeeingOpenConversation(shown)
    return null
  }
  const focus = (has: boolean) => vi.spyOn(document, 'hasFocus').mockReturnValue(has)

  it('marks it seen at once when it has unseen runs, the chat shows and the window has the focus', async () => {
    const id = scheduleWith('run_a1')
    await refreshHistory()
    setActiveConversation(id)
    focus(true)
    render(<Seeing shown />)
    await waitFor(() => expect(bridge.markSeen).toHaveBeenCalledWith(id))
    await waitFor(() => expect(getHistoryState().items.find(i => i.id === id)?.unseen).toEqual([]))
  })

  it('and when a run comes while it is open (the case C1 left open)', async () => {
    const id = scheduleWith()
    await refreshHistory()
    setActiveConversation(id)
    focus(true)
    render(<Seeing shown />)
    await act(async () => { await new Promise(r => setTimeout(r, 0)) })
    expect(bridge.markSeen).not.toHaveBeenCalled()
    store.appendRun(id, 'run_new', 'schedule')
    await act(() => refreshHistory())
    await waitFor(() => expect(bridge.markSeen).toHaveBeenCalledWith(id))
  })

  it('not while another page shows; once the chat shows again, yes', async () => {
    const id = scheduleWith('run_a1')
    await refreshHistory()
    setActiveConversation(id)
    focus(true)
    const { rerender } = render(<Seeing shown={false} />)
    await act(async () => { await new Promise(r => setTimeout(r, 0)) })
    expect(bridge.markSeen).not.toHaveBeenCalled()
    rerender(<Seeing shown />)
    await waitFor(() => expect(bridge.markSeen).toHaveBeenCalledWith(id))
  })

  it('not while the window has not the focus; once it gets it, yes; after a blur, a new run waits', async () => {
    const id = scheduleWith('run_a1')
    await refreshHistory()
    setActiveConversation(id)
    focus(false)
    render(<Seeing shown />)
    await act(async () => { await new Promise(r => setTimeout(r, 0)) })
    expect(bridge.markSeen).not.toHaveBeenCalled()
    act(() => { fireEvent.focus(window) })
    await waitFor(() => expect(bridge.markSeen).toHaveBeenCalledTimes(1))
    await waitFor(() => expect(getHistoryState().items.find(i => i.id === id)?.unseen).toEqual([]))
    act(() => { fireEvent.blur(window) })
    store.appendRun(id, 'run_a2', 'schedule')
    await act(() => refreshHistory())
    await act(async () => { await new Promise(r => setTimeout(r, 0)) })
    expect(bridge.markSeen).toHaveBeenCalledTimes(1)
    expect(getHistoryState().items.find(i => i.id === id)?.unseen).toEqual(['run_a2'])
  })

  it('nothing to do for a conversation with nothing unseen, or none open', async () => {
    store.save(userRecord('chat_mine', 1))
    scheduleWith('run_a1')
    await refreshHistory()
    focus(true)
    const { rerender } = render(<Seeing shown />)
    await act(async () => { await new Promise(r => setTimeout(r, 0)) })
    setActiveConversation('chat_mine')
    rerender(<Seeing shown />)
    await act(async () => { await new Promise(r => setTimeout(r, 0)) })
    expect(bridge.markSeen).not.toHaveBeenCalled()
  })
})
