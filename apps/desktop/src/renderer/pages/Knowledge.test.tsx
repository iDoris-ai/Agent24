// @vitest-environment jsdom
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import KnowledgePage from './Knowledge'
import { CapabilitiesContext } from '../capabilities/types'
import { setLocale } from '../i18n'
import type { KnowledgeEntryDraftSummary, KnowledgeViewCitation, KnowledgeViewConfirmAsk, KnowledgeViewState } from '../../shared/ipc-types'
import { requestEntry } from './chat/knowledge-entry'
import { closeDrafts, openDraft, openDraftList } from './knowledge-entry-drafts'

const show = vi.fn()
const bounds = vi.fn()
const hide = vi.fn()
const reload = vi.fn()
let stateListener: ((s: KnowledgeViewState) => void) | null = null
let citationListener: ((c: KnowledgeViewCitation) => void) | null = null
let confirmListener: ((ask: KnowledgeViewConfirmAsk) => void) | null = null
const answer = vi.fn()
const shownAsk = vi.fn()
// M4 A7: the conversation open in the main window, as the history list keeps it.
let activeId: string | null = null
vi.mock('./chat/history', async (original) => ({
  ...(await original<typeof import('./chat/history')>()),
  getHistoryState: () => ({ items: [], activeId, busy: false, groups: { groups: [] }, archiving: [] }),
}))
let resizeCallback: ResizeObserverCallback | null = null

beforeEach(() => {
  vi.clearAllMocks()
  stateListener = null
  citationListener = null
  confirmListener = null
  activeId = null
  setLocale('en')
  show.mockResolvedValue({ ok: true })
  reload.mockResolvedValue({ ok: true })
  bounds.mockResolvedValue(undefined)
  hide.mockResolvedValue(undefined)
  Object.defineProperty(window, 'agent24', {
    value: {
      knowledgeView: {
        show, bounds, hide, reload,
        onState: (cb: (s: KnowledgeViewState) => void) => { stateListener = cb; return () => { stateListener = null } },
        onCitation: (cb: (c: KnowledgeViewCitation) => void) => { citationListener = cb; return () => { citationListener = null } },
        onConfirm: (cb: (ask: KnowledgeViewConfirmAsk) => void) => { confirmListener = cb; return () => { confirmListener = null } },
        answer,
        shown: shownAsk,
      },
    },
    configurable: true,
  })
  globalThis.ResizeObserver = class {
    constructor(cb: ResizeObserverCallback) { resizeCallback = cb }
    observe() {}
    disconnect() {}
    unobserve() {}
  } as unknown as typeof ResizeObserver
})

describe('KnowledgePage', () => {
  it('asks main to show the view over its area, with the shell theme, and hides it on unmount', async () => {
    const { unmount } = render(<div className="app light"><KnowledgePage /></div>)
    await waitFor(() => expect(show).toHaveBeenCalledTimes(1))
    const [rect, theme] = show.mock.calls[0]
    expect(Object.keys(rect).sort()).toEqual(['height', 'width', 'x', 'y'])
    expect(theme).toBe('light')
    act(() => { resizeCallback?.([], {} as ResizeObserver) })
    expect(bounds).toHaveBeenCalled()
    unmount()
    expect(hide).toHaveBeenCalledTimes(1)
    expect(stateListener).toBeNull()
  })

  it('sends only bounds and theme: no address, partition or token', async () => {
    render(<KnowledgePage />)
    await waitFor(() => expect(show).toHaveBeenCalled())
    expect(show.mock.calls[0]).toHaveLength(2)
    expect(show.mock.calls[0][1]).toBe('dark')
  })

  it('not connected: says so in plain words and offers Settings, not Reload', async () => {
    show.mockResolvedValue({ ok: false, code: 'knowledge_off' })
    const open = vi.fn()
    render(<CapabilitiesContext.Provider value={{ defs: [], open, approvalsWaiting: 0 }}><KnowledgePage /></CapabilitiesContext.Provider>)
    expect(await screen.findByText(/Knowledge is not connected yet/)).toBeTruthy()
    expect(screen.queryByRole('button', { name: 'Reload' })).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: 'Open Settings' }))
    expect(open).toHaveBeenCalledWith('settings/connections', 'other')
  })

  it('no Settings button once the page is up or merely reloadable', async () => {
    render(<KnowledgePage />)
    await waitFor(() => expect(show).toHaveBeenCalled())
    act(() => stateListener?.({ state: 'degraded', code: 'load_failed' }))
    expect(screen.getByRole('button', { name: 'Reload' })).toBeTruthy()
    expect(screen.queryByRole('button', { name: 'Open Settings' })).toBeNull()
  })

  it('offers reload after a crash loop, and reload asks main for a new instance', async () => {
    render(<KnowledgePage />)
    await waitFor(() => expect(show).toHaveBeenCalled())
    act(() => stateListener?.({ state: 'closed', code: 'crash_loop' }))
    fireEvent.click(await screen.findByRole('button', { name: 'Reload' }))
    await waitFor(() => expect(reload).toHaveBeenCalledTimes(1))
    await waitFor(() => expect(screen.queryByText(/keeps crashing/)).toBeNull())
  })

  it('shows nothing over the page once it is ready', async () => {
    render(<KnowledgePage />)
    act(() => stateListener?.({ state: 'loading' }))
    expect(screen.getByText(/Opening the knowledge page/)).toBeTruthy()
    act(() => stateListener?.({ state: 'ready' }))
    expect(screen.queryByRole('status')).toBeNull()
  })

  it('renders a citation as text, not HTML', async () => {
    const { container } = render(<KnowledgePage />)
    await waitFor(() => expect(show).toHaveBeenCalled())
    act(() => citationListener?.({
      title: 'From the page',
      text: 'line\n<b>bold</b>',
      ref: { knowledge_base_id: 'kb', knowledge_id: 'k', chunk_id: 'c' },
    }))
    expect(screen.getByRole('complementary', { name: 'Citation' })).toBeTruthy()
    expect(screen.getByText('From the page')).toBeTruthy()
    const text = container.querySelector('.knowledge-citation-text')
    expect(text?.textContent).toBe('line\n<b>bold</b>')
    expect(container.querySelector('.knowledge-citation b')).toBeNull()
    expect(container.querySelector('.knowledge-page .knowledge-host')).toBeTruthy()
  })

  it('dismiss clears the strip', async () => {
    render(<KnowledgePage />)
    await waitFor(() => expect(show).toHaveBeenCalled())
    act(() => citationListener?.({ title: 'T', text: 'body', ref: { knowledge_base_id: 'kb', knowledge_id: 'k', chunk_id: 'c' } }))
    fireEvent.click(screen.getByRole('button', { name: 'Close' }))
    expect(screen.queryByText('body')).toBeNull()
    expect(document.querySelector('.knowledge-citation')).toBeNull()
  })

  it('a null citation clears the strip', async () => {
    render(<KnowledgePage />)
    await waitFor(() => expect(show).toHaveBeenCalled())
    act(() => citationListener?.({ title: 'T', text: 'body', ref: { knowledge_base_id: 'kb', knowledge_id: 'k', chunk_id: 'c' } }))
    expect(screen.getByText('body')).toBeTruthy()
    act(() => citationListener?.(null))
    expect(screen.queryByText('body')).toBeNull()
  })

  it('an untitled citation shows the fallback', async () => {
    render(<KnowledgePage />)
    await waitFor(() => expect(show).toHaveBeenCalled())
    act(() => citationListener?.({ title: null, text: 'body', ref: { knowledge_base_id: 'kb', knowledge_id: 'k', chunk_id: 'c' } }))
    expect(screen.getByText('Untitled')).toBeTruthy()
    expect(screen.getByRole('complementary', { name: 'Citation' })).toBeTruthy()
  })

  const ASK = { id: 'a'.repeat(22), count: 2, titles: ['Alpha', null] as Array<string | null> }

  it('the confirmation panel shows the count and the titles', async () => {
    const { container } = render(<KnowledgePage />)
    await waitFor(() => expect(show).toHaveBeenCalled())
    act(() => confirmListener?.(ASK))
    expect(screen.getByRole('dialog')).toBeTruthy()
    expect(screen.getByText('2 selected')).toBeTruthy()
    expect(screen.getByText('Alpha')).toBeTruthy()
    expect(screen.getByText('Untitled')).toBeTruthy()
    expect(container.querySelector('.knowledge-page > .knowledge-confirm')).toBeTruthy()
    expect(container.querySelector('.knowledge-host .knowledge-confirm')).toBeNull()
  })

  it('M4 A7: a dialog that comes up tells main which conversation is open; a null one does not', async () => {
    activeId = 'chat_open1'
    render(<KnowledgePage />)
    await waitFor(() => expect(show).toHaveBeenCalled())
    act(() => confirmListener?.(ASK))
    expect(shownAsk).toHaveBeenCalledTimes(1)
    expect(shownAsk).toHaveBeenCalledWith(ASK.id, 'chat_open1')
    act(() => confirmListener?.(null))
    expect(shownAsk).toHaveBeenCalledTimes(1)
    activeId = null
    act(() => confirmListener?.(ASK))
    expect(shownAsk).toHaveBeenLastCalledWith(ASK.id, null)
  })

  it('accept answers true and opens the main chat, in that order', async () => {
    const open = vi.fn()
    render(<CapabilitiesContext.Provider value={{ defs: [], open, approvalsWaiting: 0 }}><KnowledgePage /></CapabilitiesContext.Provider>)
    await waitFor(() => expect(show).toHaveBeenCalled())
    act(() => confirmListener?.(ASK))
    fireEvent.click(screen.getByRole('button', { name: 'Add to conversation' }))
    expect(answer).toHaveBeenCalledWith(ASK.id, true)
    expect(open).toHaveBeenCalledWith('assistant', 'other')
    expect(answer.mock.invocationCallOrder[0]).toBeLessThan(open.mock.invocationCallOrder[0])
    expect(screen.queryByRole('dialog')).toBeNull()
  })

  it('decline answers false and leaves the chat where it is', async () => {
    const open = vi.fn()
    render(<CapabilitiesContext.Provider value={{ defs: [], open, approvalsWaiting: 0 }}><KnowledgePage /></CapabilitiesContext.Provider>)
    await waitFor(() => expect(show).toHaveBeenCalled())
    act(() => confirmListener?.(ASK))
    fireEvent.click(screen.getByRole('button', { name: 'Not now' }))
    expect(answer).toHaveBeenCalledWith(ASK.id, false)
    expect(open).not.toHaveBeenCalled()
    expect(screen.queryByRole('dialog')).toBeNull()
  })

  it('a null confirm closes the panel', async () => {
    render(<KnowledgePage />)
    await waitFor(() => expect(show).toHaveBeenCalled())
    act(() => confirmListener?.(ASK))
    expect(screen.getByRole('dialog')).toBeTruthy()
    act(() => confirmListener?.(null))
    expect(screen.queryByRole('dialog')).toBeNull()
  })

  it('hides the citation strip while the confirmation panel is showing', async () => {
    render(<KnowledgePage />)
    await waitFor(() => expect(show).toHaveBeenCalled())
    act(() => citationListener?.({ title: 'T', text: 'body', ref: { knowledge_base_id: 'kb', knowledge_id: 'k', chunk_id: 'c' } }))
    expect(screen.getByRole('complementary', { name: 'Citation' })).toBeTruthy()
    act(() => confirmListener?.(ASK))
    expect(screen.getByRole('dialog')).toBeTruthy()
    expect(screen.queryByRole('complementary', { name: 'Citation' })).toBeNull()
    expect(screen.queryByText('body')).toBeNull()
    act(() => confirmListener?.(null))
    expect(screen.queryByRole('dialog')).toBeNull()
    expect(screen.getByRole('complementary', { name: 'Citation' })).toBeTruthy()
  })
})

const ONE: KnowledgeEntryDraftSummary = {
  id: 'd1', kind: 'note', title: 'Draft one', updated_at: '2026-01-01T00:00:00.000Z', target: 'same', findings: [],
}

/** Elements with this text that are not inside a hidden panel. */
const visible = (text: RegExp): HTMLElement[] => screen.queryAllByText(text).filter((el) => !el.closest('[hidden]'))

describe('knowledge entry drafts on the page', () => {
  afterEach(() => {
    act(() => {
      requestEntry(null)
      closeDrafts()
    })
  })

  function withEntry(knowledgeEntry: object): void {
    const current = window.agent24
    Object.defineProperty(window, 'agent24', {
      configurable: true,
      value: { ...current, knowledgeEntry },
    })
  }

  it('shows the drafts bar when nothing else is open, and nothing when the bridge has no knowledgeEntry', async () => {
    const first = render(<KnowledgePage />)
    await waitFor(() => expect(show).toHaveBeenCalled())
    expect(visible(/Knowledge entry drafts/)).toEqual([])
    expect(screen.queryByRole('button', { name: 'View' })).toBeNull()
    first.unmount()

    withEntry({ listDrafts: vi.fn(async () => [ONE, { ...ONE, id: 'd2', title: 'Draft two' }]) })
    render(<KnowledgePage />)
    expect(await screen.findByText('Knowledge entry drafts: 2')).toBeTruthy()
    expect(screen.getByRole('button', { name: 'View' })).toBeTruthy()
  })

  it('hides the drafts panel and the bar while an entry, a confirmation, or a citation is open', async () => {
    withEntry({
      listDrafts: vi.fn(async () => [ONE]),
      admit: vi.fn(() => new Promise(() => {})),
    })
    openDraftList()
    render(<KnowledgePage />)
    expect(await screen.findByRole('dialog', { name: 'Knowledge entry drafts' })).toBeTruthy()
    expect(screen.queryByRole('button', { name: 'View' })).toBeNull()

    act(() => requestEntry('chat_abc'))
    expect(await screen.findByRole('dialog', { name: 'Knowledge entry' })).toBeTruthy()
    expect(screen.queryByRole('dialog', { name: 'Knowledge entry drafts' })).toBeNull()
    expect(visible(/Knowledge entry drafts/)).toEqual([])

    act(() => requestEntry(null))
    expect(await screen.findByRole('dialog', { name: 'Knowledge entry drafts' })).toBeTruthy()
    act(() => closeDrafts())
    expect(await screen.findByText('Knowledge entry drafts: 1')).toBeTruthy()

    const ask = { id: 'a'.repeat(22), count: 2, titles: ['Alpha', null] as Array<string | null> }
    act(() => confirmListener?.(ask))
    expect(screen.getByText('2 selected')).toBeTruthy()
    expect(visible(/Knowledge entry drafts/)).toEqual([])
    expect(screen.queryByRole('dialog', { name: 'Knowledge entry drafts' })).toBeNull()
    act(() => confirmListener?.(null))
    expect(await screen.findByText('Knowledge entry drafts: 1')).toBeTruthy()

    act(() => citationListener?.({ title: 'T', text: 'body', ref: { knowledge_base_id: 'kb', knowledge_id: 'k', chunk_id: 'c' } }))
    expect(screen.getByRole('complementary', { name: 'Citation' })).toBeTruthy()
    expect(visible(/Knowledge entry drafts/)).toEqual([])
    expect(screen.queryByRole('dialog', { name: 'Knowledge entry drafts' })).toBeNull()
  })

  it('hides an open drafts panel while a confirmation or a citation is open, and shows it again after', async () => {
    withEntry({ listDrafts: vi.fn(async () => [ONE]) })
    openDraftList()
    render(<KnowledgePage />)
    expect(await screen.findByRole('dialog', { name: 'Knowledge entry drafts' })).toBeTruthy()
    act(() => confirmListener?.({ id: 'b'.repeat(22), count: 1, titles: ['Alpha'] }))
    expect(screen.getByText('1 selected')).toBeTruthy()
    expect(screen.queryByRole('dialog', { name: 'Knowledge entry drafts' })).toBeNull()
    act(() => confirmListener?.(null))
    expect(await screen.findByRole('dialog', { name: 'Knowledge entry drafts' })).toBeTruthy()
    act(() => citationListener?.({ title: 'T', text: 'body', ref: { knowledge_base_id: 'kb', knowledge_id: 'k', chunk_id: 'c' } }))
    expect(screen.queryByRole('dialog', { name: 'Knowledge entry drafts' })).toBeNull()
  })

  it('keeps unsaved edits in a draft while a citation covers it (Codex C2)', async () => {
    const draft = { ...ONE, body: 'original', message_from: 0, message_to: 1, conversation_id: 'chat_abc', created_at: ONE.updated_at, content_sha256: 'a'.repeat(64) }
    withEntry({ listDrafts: vi.fn(async () => [ONE]), getDraft: vi.fn(async () => draft) })
    act(() => openDraft(ONE.id))
    render(<KnowledgePage />)
    const body = await screen.findByLabelText('Body')
    fireEvent.change(body, { target: { value: 'my unsaved edit' } })
    act(() => citationListener?.({ title: 'T', text: 'body', ref: { knowledge_base_id: 'kb', knowledge_id: 'k', chunk_id: 'c' } }))
    expect(screen.queryByRole('dialog', { name: 'Knowledge entry drafts' })).toBeNull()
    act(() => citationListener?.(null))
    expect(await screen.findByRole('dialog', { name: 'Knowledge entry drafts' })).toBeTruthy()
    expect(screen.getByLabelText('Body')).toHaveValue('my unsaved edit')
  })
})

