// @vitest-environment jsdom
import { act, fireEvent, render, renderHook, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { ReactNode } from 'react'

import KnowledgePage from '../Knowledge'
import { CapabilitiesContext, type CapabilitiesApi } from '../../capabilities/types'
import { setLocale } from '../../i18n'
import type { KnowledgeEntryAdmission } from '../../../shared/ipc-types'
import { KnowledgeEntryPanel, requestEntry, useEntryMenu, usePendingEntry } from './knowledge-entry'

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
    expect(open).toHaveBeenCalledWith('knowledge', 'other')
    expect(result.current.pending).toBe('chat_abc')
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

describe('KnowledgeEntryPanel', () => {
  it('asks main about the conversation by id only and says the answer', async () => {
    admit.mockResolvedValue({ ok: true })
    render(<KnowledgeEntryPanel id="chat_abc" />)
    expect(screen.getByRole('status')).toHaveTextContent('正在检查这条对话')
    expect(screen.queryByRole('button')).toBeNull()
    await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('可以整理成知识条目'))
    expect(admit).toHaveBeenCalledWith('chat_abc')
  })

  it('says each refusal in plain words, with no way to go on anyway (N10c–e)', async () => {
    const cases: Array<[KnowledgeEntryAdmission, string]> = [
      [{ ok: false, reason: 'restricted' }, '受限资料'],
      [{ ok: false, reason: 'unknown' }, '无法确认'],
      [{ ok: false, reason: 'floor_missing' }, '没有这条对话回复来源的记录'],
      [{ ok: false, reason: 'not_confirmed' }, '没有做任何操作'],
      [{ ok: false, reason: 'knowledge_off' }, '未开启或未绑定'],
    ]
    for (const [answer, words] of cases) {
      admit.mockResolvedValueOnce(answer)
      const { unmount } = render(<KnowledgeEntryPanel id="chat_abc" />)
      await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent(words))
      expect(screen.getAllByRole('button').map((b) => b.textContent)).toEqual(['关闭'])
      unmount()
    }
    admit.mockRejectedValueOnce(new Error('ipc'))
    render(<KnowledgeEntryPanel id="chat_abc" />)
    await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('暂时无法检查'))
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
