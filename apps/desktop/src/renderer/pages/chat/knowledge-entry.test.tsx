// @vitest-environment jsdom
import { act, fireEvent, render, renderHook, screen, waitFor } from '@testing-library/react'
import { StrictMode } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { ReactNode } from 'react'

import KnowledgePage from '../Knowledge'
import { CapabilitiesContext, type CapabilitiesApi } from '../../capabilities/types'
import { setLocale } from '../../i18n'
import type { KnowledgeEntryAdmission } from '../../../shared/ipc-types'
import { KNOWLEDGE_PAGE, KnowledgeEntryPanel, requestEntry, useEntryMenu, usePendingEntry } from './knowledge-entry'
import { CAPABILITIES } from '../../capabilities/registry'

const admit = vi.fn<(id: string) => Promise<KnowledgeEntryAdmission>>()
const status = vi.fn<() => Promise<{ active: boolean }>>()

beforeEach(() => {
  vi.clearAllMocks()
  setLocale('zh')
  status.mockResolvedValue({ active: true })
  act(() => requestEntry(null))
  Object.defineProperty(window, 'agent24', {
    value: {
      knowledgeEntry: { admit },
      knowledge: { status },
      knowledgeView: {
        show: vi.fn(async () => ({ ok: true })), bounds: vi.fn(), hide: vi.fn(), reload: vi.fn(),
        onState: () => () => {}, onCitation: () => () => {}, onConfirm: () => () => {}, answer: vi.fn(),
      },
    },
    configurable: true,
  })
  globalThis.ResizeObserver = class { observe() {} disconnect() {} unobserve() {} } as unknown as typeof ResizeObserver
})

describe('the history list action', () => {
  const wrapper = (open = vi.fn()) => function Wrapper({ children }: { children: ReactNode }) {
    return <CapabilitiesContext.Provider value={{ defs: [], open, approvalsWaiting: 0 } as CapabilitiesApi}>{children}</CapabilitiesContext.Provider>
  }

  it('is one item, keyed knowledge-entry, that hands the conversation to the Knowledge page', async () => {
    const open = vi.fn()
    const { result } = renderHook(() => ({ item: useEntryMenu(), pending: usePendingEntry() }), { wrapper: wrapper(open) })
    await waitFor(() => expect(result.current.item('chat_abc')).toHaveLength(1))
    const [entry] = result.current.item('chat_abc')
    expect(entry).toMatchObject({ kind: 'item', key: 'knowledge-entry', label: '沉淀为知识条目', focuses: true })
    act(() => { void (entry as { onSelect: () => void }).onSelect() })
    expect(open).toHaveBeenCalledWith('knowledgeBase', 'other')
    expect(result.current.pending).toMatchObject({ id: 'chat_abc' })
    expect(admit).toHaveBeenCalledTimes(1)
  })

  it('does not appear while Knowledge is off, unbound, not yet known or not answering', async () => {
    const answers = [() => Promise.resolve({ active: false }), () => Promise.reject(new Error('ipc')), () => new Promise<{ active: boolean }>(() => {})]
    for (const answer of answers) {
      status.mockImplementationOnce(answer)
      const { result, unmount } = renderHook(() => useEntryMenu(), { wrapper: wrapper() })
      await act(async () => {})
      expect(result.current('chat_abc')).toEqual([])
      unmount()
    }
    Object.defineProperty(window, 'agent24', { value: { knowledgeEntry: { admit } }, configurable: true })
    const { result } = renderHook(() => useEntryMenu(), { wrapper: wrapper() })
    await act(async () => {})
    expect(result.current('chat_abc')).toEqual([])
  })

  it('asks again on each press, so a menu opened after Knowledge turned off has no item', async () => {
    const { result } = renderHook(() => useEntryMenu(), { wrapper: wrapper() })
    await waitFor(() => expect(result.current('chat_abc')).toHaveLength(1))
    status.mockResolvedValue({ active: false })
    fireEvent.pointerDown(document.body)
    await waitFor(() => expect(result.current('chat_abc')).toEqual([]))
    status.mockResolvedValue({ active: true })
    fireEvent.contextMenu(document.body)
    await waitFor(() => expect(result.current('chat_abc')).toHaveLength(1))
  })
})

describe('the Knowledge page id', () => {
  it('is the registered Knowledge capability', () => {
    expect(CAPABILITIES.some((c) => c.id === KNOWLEDGE_PAGE)).toBe(true)
  })
})

describe('KnowledgeEntryPanel', () => {
  it('asks main once, from the request, by id only, and says the answer', async () => {
    let answer: (a: KnowledgeEntryAdmission) => void = () => {}
    admit.mockImplementation(() => new Promise((resolve) => { answer = resolve }))
    const { result } = renderHook(() => usePendingEntry())
    act(() => requestEntry('chat_abc'))
    // StrictMode mounts twice; the question is still asked once.
    render(<StrictMode><KnowledgeEntryPanel ask={result.current!} /></StrictMode>)
    expect(screen.getByRole('status')).toHaveTextContent('正在检查这条对话')
    expect(screen.queryByRole('button')).toBeNull()
    expect(admit).toHaveBeenCalledTimes(1)
    expect(admit).toHaveBeenCalledWith('chat_abc')
    await act(async () => answer({ ok: true }))
    expect(result.current).toEqual({ id: 'chat_abc', state: 'done', answer: { ok: true } })
  })

  it('says each refusal in plain words, with no way to go on anyway (N10c–e)', () => {
    const cases: Array<[KnowledgeEntryAdmission, string]> = [
      [{ ok: false, reason: 'restricted' }, '受限资料'],
      [{ ok: false, reason: 'unknown' }, '无法确认'],
      [{ ok: false, reason: 'floor_missing' }, '没有这条对话回复来源的记录'],
      [{ ok: false, reason: 'not_confirmed' }, '没有做任何操作'],
      [{ ok: false, reason: 'knowledge_off' }, '未开启或未绑定'],
    ]
    for (const [answer, words] of cases) {
      const { unmount } = render(<KnowledgeEntryPanel ask={{ id: 'c', state: 'done', answer }} />)
      expect(screen.getByRole('status')).toHaveTextContent(words)
      expect(screen.getAllByRole('button').map((b) => b.textContent)).toEqual(['关闭'])
      unmount()
    }
    render(<KnowledgeEntryPanel ask={{ id: 'c', state: 'failed' }} />)
    expect(screen.getByRole('status')).toHaveTextContent('暂时无法检查')
  })

  it('an answer main never gives reads as a failure, not as an admission', async () => {
    const { result } = renderHook(() => usePendingEntry())
    for (const bad of [undefined, { ok: 'yes' }, { ok: false, reason: 'toString' }, { ok: false, reason: 'weird' }]) {
      admit.mockResolvedValueOnce(bad as unknown as KnowledgeEntryAdmission)
      act(() => requestEntry('chat_abc'))
      await waitFor(() => expect(result.current?.state).toBe('failed'))
    }
    admit.mockImplementationOnce(() => { throw new Error('no bridge') })
    act(() => requestEntry('chat_abc'))
    await waitFor(() => expect(result.current?.state).toBe('failed'))
  })

  it('a late answer to an earlier question does not overwrite a later one', async () => {
    const answers: Array<(a: KnowledgeEntryAdmission) => void> = []
    admit.mockImplementation(() => new Promise((resolve) => { answers.push(resolve) }))
    const { result } = renderHook(() => usePendingEntry())
    act(() => requestEntry('chat_a'))
    act(() => requestEntry('chat_b'))
    await act(async () => answers[1]({ ok: false, reason: 'restricted' }))
    await act(async () => answers[0]({ ok: true }))
    expect(result.current).toEqual({ id: 'chat_b', state: 'done', answer: { ok: false, reason: 'restricted' } })
  })

  it('shows on the Knowledge page while asked about, and closes', async () => {
    admit.mockResolvedValue({ ok: false, reason: 'restricted' })
    render(<KnowledgePage />)
    expect(screen.queryByRole('dialog', { name: '知识条目' })).toBeNull()
    act(() => requestEntry('chat_abc'))
    await waitFor(() => expect(screen.getByRole('dialog', { name: '知识条目' })).toHaveTextContent('受限资料'))
    fireEvent.click(screen.getByRole('button', { name: '关闭' }))
    expect(screen.queryByRole('dialog', { name: '知识条目' })).toBeNull()
  })
})
