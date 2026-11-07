// @vitest-environment jsdom
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import KnowledgePage from './Knowledge'
import { setLocale } from '../i18n'
import type { KnowledgeViewState } from '../../shared/ipc-types'

const show = vi.fn()
const bounds = vi.fn()
const hide = vi.fn()
const reload = vi.fn()
let stateListener: ((s: KnowledgeViewState) => void) | null = null
let resizeCallback: ResizeObserverCallback | null = null

beforeEach(() => {
  vi.clearAllMocks()
  stateListener = null
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

  it('says why when Knowledge is off, without a reload button', async () => {
    show.mockResolvedValue({ ok: false, code: 'knowledge_off' })
    render(<KnowledgePage />)
    expect(await screen.findByText(/Knowledge is off/)).toBeTruthy()
    expect(screen.queryByRole('button')).toBeNull()
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
})
