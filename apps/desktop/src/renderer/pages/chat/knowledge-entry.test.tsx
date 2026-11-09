// @vitest-environment jsdom
import { act, fireEvent, render, renderHook, screen, waitFor, within } from '@testing-library/react'
import { StrictMode } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { ReactNode } from 'react'

import KnowledgePage from '../Knowledge'
import { CapabilitiesContext, type CapabilitiesApi } from '../../capabilities/types'
import { setLocale } from '../../i18n'
import type { KnowledgeEntryAdmission, KnowledgeEntryAdmitAnswer, KnowledgeEntryDraftCreated, KnowledgeEntryDraftView, KnowledgeEntryMessagePreview } from '../../../shared/ipc-types'
import { KNOWLEDGE_PAGE, KnowledgeEntryPanel, requestEntry, useEntryMenu, usePendingEntry } from './knowledge-entry'
import { closeDrafts, KnowledgeEntryDraftsPanel } from '../knowledge-entry-drafts'
import { CAPABILITIES } from '../../capabilities/registry'

const admit = vi.fn<(id: string) => Promise<KnowledgeEntryAdmission>>()
const createDraft = vi.fn<(ticket: string, range: { from: number; to: number }) => Promise<KnowledgeEntryDraftCreated>>()
const status = vi.fn<() => Promise<{ active: boolean }>>()

beforeEach(() => {
  vi.clearAllMocks()
  setLocale('zh')
  status.mockResolvedValue({ active: true })
  act(() => {
    requestEntry(null)
    closeDrafts()
  })
  Object.defineProperty(window, 'agent24', {
    value: {
      knowledgeEntry: { admit, createDraft },
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
    // No Knowledge bridge, and no preload bridge at all (as in other pages' tests).
    for (const bridge of [{ knowledgeEntry: { admit } }, undefined]) {
      Object.defineProperty(window, 'agent24', { value: bridge, configurable: true })
      const { result, unmount } = renderHook(() => useEntryMenu(), { wrapper: wrapper() })
      await act(async () => {})
      expect(result.current('chat_abc')).toEqual([])
      unmount()
    }
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
    expect(result.current).toEqual({ id: 'chat_abc', state: 'checking' })
    // The page under StrictMode (mounted twice) shows it; nothing asks again.
    render(<StrictMode><KnowledgePage /></StrictMode>)
    const panel = await screen.findByRole('dialog', { name: '知识条目' })
    expect(panel).toHaveTextContent('正在检查这条对话')
    expect(within(panel).queryByRole('button')).toBeNull()
    expect(admit).toHaveBeenCalledTimes(1)
    expect(admit).toHaveBeenCalledWith('chat_abc')
    await act(async () => answer({
      ok: true,
      ticket: 'tkt',
      messages: [{ role: 'user', preview: '你好' }, { role: 'assistant', preview: '好的' }],
    } as KnowledgeEntryAdmission))
    expect(screen.getByRole('dialog', { name: '知识条目' })).toHaveTextContent('选择要整理的消息')
    expect(admit).toHaveBeenCalledTimes(1)
  })

  it('says each refusal in plain words, with no way to go on anyway (N10c–e)', () => {
    const cases: Array<[Extract<KnowledgeEntryAdmission, { ok: false }>, string]> = [
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

const PREVIEWS: KnowledgeEntryMessagePreview[] = [
  { role: 'user', preview: '你好' },
  { role: 'assistant', preview: '好的' },
  { role: 'user', preview: '再写一条' },
]

function draftView(over: Partial<KnowledgeEntryDraftView> = {}): KnowledgeEntryDraftView {
  return {
    id: 'draft-1',
    kind: 'note',
    title: '整理好的标题',
    body: '整理好的正文',
    updated_at: '2026-01-01T00:00:00.000Z',
    target: 'same',
    findings: [],
    conversation_id: 'chat_abc',
    message_from: 0,
    message_to: 1,
    created_at: '2026-01-01T00:00:00.000Z',
    content_sha256: 'abc',
    format: null,
    submission: { pending: false, stuck: false, candidate: null },
    knowledge_base_ids: [],
    ...over,
  }
}

function Stage(): JSX.Element {
  const entry = usePendingEntry()
  return (
    <>
      {entry ? <KnowledgeEntryPanel ask={entry} /> : null}
      <KnowledgeEntryDraftsPanel />
    </>
  )
}

async function reachChoose(): Promise<void> {
  let resolve: (value: KnowledgeEntryAdmitAnswer) => void = () => {}
  admit.mockImplementationOnce(() => new Promise((settle) => { resolve = settle as (value: KnowledgeEntryAdmitAnswer) => void }))
  act(() => requestEntry('chat_abc'))
  await act(async () => resolve({ ok: true, ticket: 'tkt', messages: PREVIEWS }))
}

describe('choosing a range and drafting', () => {
  beforeEach(() => {
    createDraft.mockReset()
  })

  it('creates one draft for the chosen range and opens it with the model notes', async () => {
    const draft = draftView()
    const getDraft = vi.fn(async () => draft)
    const current = window.agent24
    Object.defineProperty(window, 'agent24', {
      configurable: true,
      value: { ...current, knowledgeEntry: { ...current.knowledgeEntry, admit, createDraft, getDraft } },
    })
    createDraft.mockResolvedValue({ ok: true, draft, model: 'no_model', hints: ['日期', '范围'] })
    render(<Stage />)
    await reachChoose()
    expect(screen.getAllByRole('option', { name: '1. 你：你好' })).toHaveLength(2)
    expect(screen.getAllByRole('option', { name: '2. 助手：好的' })).toHaveLength(2)
    expect(screen.getByLabelText('从')).toHaveValue('0')
    expect(screen.getByLabelText('到')).toHaveValue('2')
    fireEvent.change(screen.getByLabelText('到'), { target: { value: '1' } })
    fireEvent.click(screen.getByRole('button', { name: '生成草稿' }))
    expect(createDraft).toHaveBeenCalledTimes(1)
    expect(createDraft).toHaveBeenCalledWith('tkt', { from: 0, to: 1 })
    expect(await screen.findByText('没有可用的本机模型，已给出空模板，请自己填写。')).toBeTruthy()
    expect(screen.queryByRole('dialog', { name: '知识条目' })).toBeNull()
    expect(screen.getByText('模型提示再看一眼（只是提示，不影响提交）：日期、范围')).toBeTruthy()
    expect(screen.getByLabelText('标题')).toHaveValue('整理好的标题')
    expect(screen.getByText('来自对话第 1\u20132 条消息')).toBeTruthy()
  })

  it('opens the template main gave when it held the messages back from the model', async () => {
    const draft = draftView()
    const current = window.agent24
    Object.defineProperty(window, 'agent24', {
      configurable: true,
      value: { ...current, knowledgeEntry: { ...current.knowledgeEntry, admit, createDraft, getDraft: vi.fn(async () => draft) } },
    })
    createDraft.mockResolvedValue({ ok: true, draft, model: 'held_back', hints: [] })
    render(<Stage />)
    await reachChoose()
    fireEvent.click(screen.getByRole('button', { name: '生成草稿' }))
    expect(await screen.findByText('所选消息里有无法自动脱敏的内容，没有交给本机模型，已给出空模板。')).toBeTruthy()
  })

  it('disables 生成草稿 when from is after to', async () => {
    render(<Stage />)
    await reachChoose()
    expect(screen.getByRole('button', { name: '生成草稿' })).toBeEnabled()
    fireEvent.change(screen.getByLabelText('从'), { target: { value: '2' } })
    fireEvent.change(screen.getByLabelText('到'), { target: { value: '0' } })
    const generate = screen.getByRole('button', { name: '生成草稿' })
    expect(generate).toBeDisabled()
    fireEvent.click(generate)
    expect(createDraft).not.toHaveBeenCalled()
  })

  it('says each createDraft failure, and treats a bad or rejected reply as a failure', async () => {
    const cases: Array<[KnowledgeEntryDraftCreated, string]> = [
      [{ ok: false, reason: 'ticket_expired' }, '这次确认已过期，请在历史对话里重新选「沉淀为知识条目」。'],
      [{ ok: false, reason: 'invalid_range' }, '选的消息范围不对，请重新选。'],
      [{ ok: false, reason: 'too_long' }, '选的消息太长，请少选几条。'],
      [{ ok: false, reason: 'too_many' }, '本地草稿已满 50 份，请先撤回一些。'],
      [{ ok: false, reason: 'knowledge_off' }, 'Knowledge 未开启或未绑定知识库，请先在设置里开启。'],
      [{ ok: false, reason: 'busy' }, '另一份草稿正在生成，请稍候。'],
      [{ ok: false, reason: 'not_saved' }, '草稿没能保存，请稍后再试。'],
      [{ ok: true } as unknown as KnowledgeEntryDraftCreated, '暂时无法检查这条对话，请稍后再试。'],
    ]
    render(<Stage />)
    for (const [reply, words] of cases) {
      createDraft.mockReset()
      createDraft.mockResolvedValueOnce(reply)
      await reachChoose()
      fireEvent.click(screen.getByRole('button', { name: '生成草稿' }))
      await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent(words))
      expect(screen.getAllByRole('button').map((button) => button.textContent)).toEqual(['关闭'])
      expect(createDraft).toHaveBeenCalledTimes(1)
      fireEvent.click(screen.getByRole('button', { name: '关闭' }))
      expect(screen.queryByRole('dialog', { name: '知识条目' })).toBeNull()
    }
    createDraft.mockReset()
    createDraft.mockRejectedValueOnce(new Error('ipc'))
    await reachChoose()
    fireEvent.click(screen.getByRole('button', { name: '生成草稿' }))
    await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('暂时无法检查这条对话，请稍后再试。'))
    createDraft.mockReset()
    createDraft.mockImplementationOnce(() => { throw new Error('no bridge') })
    fireEvent.click(screen.getByRole('button', { name: '关闭' }))
    await reachChoose()
    fireEvent.click(screen.getByRole('button', { name: '生成草稿' }))
    await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('暂时无法检查这条对话，请稍后再试。'))
  })

  it('ignores a draft reply that arrives after cancel', async () => {
    let resolveDraft: (value: KnowledgeEntryDraftCreated) => void = () => {}
    createDraft.mockImplementation(() => new Promise((resolve) => { resolveDraft = resolve }))
    render(<Stage />)
    await reachChoose()
    fireEvent.click(screen.getByRole('button', { name: '生成草稿' }))
    expect(screen.getByRole('status')).toHaveTextContent('正在本机整理草稿…（只用本机模型）')
    expect(screen.queryByRole('button')).toBeNull()
    act(() => requestEntry(null))
    await act(async () => resolveDraft({ ok: true, draft: draftView(), model: 'no_model', hints: ['日期'] }))
    expect(screen.queryByRole('dialog')).toBeNull()
    expect(screen.queryByText('没有可用的本机模型')).toBeNull()
  })

  it('ignores an admission that arrives after cancel', async () => {
    let resolveAdmit: (value: KnowledgeEntryAdmission) => void = () => {}
    admit.mockImplementation(() => new Promise((resolve) => { resolveAdmit = resolve }))
    const { result } = renderHook(() => usePendingEntry())
    act(() => requestEntry('chat_abc'))
    act(() => requestEntry(null))
    await act(async () => resolveAdmit({ ok: true, ticket: 'tkt', messages: PREVIEWS } as KnowledgeEntryAdmission))
    expect(result.current).toBeNull()
  })

  it('取消 closes the choose panel without drafting', async () => {
    render(<Stage />)
    await reachChoose()
    fireEvent.click(screen.getByRole('button', { name: '取消' }))
    expect(screen.queryByRole('dialog', { name: '知识条目' })).toBeNull()
    expect(createDraft).not.toHaveBeenCalled()
  })
})
