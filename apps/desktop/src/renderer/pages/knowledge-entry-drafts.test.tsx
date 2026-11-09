// @vitest-environment jsdom
import { StrictMode, useContext } from 'react'
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import type {
  KnowledgeEntryDraftSummary,
  KnowledgeEntryDraftView,
  KnowledgeEntryFinding,
  KnowledgeEntryModelOutcome,
  KnowledgeEntrySubmission,
} from '../../shared/ipc-types'
import { CapabilitiesProvider, useCapabilityRoute } from '../capabilities/providers'
import { CapabilitiesContext } from '../capabilities/types'
import { setLocale } from '../i18n'
import {
  KnowledgeEntryDraftsBar,
  KnowledgeEntryDraftsPanel,
  closeDrafts,
  openDraft,
  openDraftList,
  useDraftsPanel,
} from './knowledge-entry-drafts'

const listDrafts = vi.fn<() => Promise<KnowledgeEntryDraftSummary[]>>()
const getDraft = vi.fn<(id: string) => Promise<KnowledgeEntryDraftView | null>>()
const updateDraft = vi.fn()
const redactDraft = vi.fn()
const withdrawDraft = vi.fn()
const submit = vi.fn()
const submitStatus = vi.fn()
const retractSubmission = vi.fn()
let confirmSpy: { mockRestore: () => void } | null = null

function summary(over: Partial<KnowledgeEntryDraftSummary> = {}): KnowledgeEntryDraftSummary {
  return {
    id: 'd1',
    kind: 'note',
    title: '甲',
    updated_at: '2026-01-01T00:00:00.000Z',
    target: 'same',
    findings: [],
    ...over,
  }
}

function view(over: Partial<KnowledgeEntryDraftView> = {}): KnowledgeEntryDraftView {
  return {
    ...summary(),
    body: '正文',
    conversation_id: 'chat_abc',
    message_from: 0,
    message_to: 2,
    created_at: '2026-01-01T00:00:00.000Z',
    content_sha256: 'abc',
    format: null,
    submission: { pending: false, stuck: false, candidate: null },
    knowledge_base_ids: [],
    ...over,
  }
}

function Frame(): JSX.Element {
  const open = useDraftsPanel() !== null
  return open ? <KnowledgeEntryDraftsPanel /> : <KnowledgeEntryDraftsBar />
}

async function shown(draft: KnowledgeEntryDraftView, extra?: { model: KnowledgeEntryModelOutcome; hints: string[] }): Promise<void> {
  getDraft.mockResolvedValue(draft)
  act(() => openDraft(draft.id, extra))
  expect(await screen.findByLabelText('标题')).toHaveValue(draft.title)
}

beforeEach(() => {
  vi.clearAllMocks()
  setLocale('zh')
  act(() => closeDrafts())
  listDrafts.mockResolvedValue([])
  getDraft.mockResolvedValue(null)
  Object.defineProperty(window, 'agent24', {
    configurable: true,
    value: { knowledgeEntry: { listDrafts, getDraft, updateDraft, redactDraft, withdrawDraft, submit, submitStatus, retractSubmission } },
  })
})

afterEach(() => {
  confirmSpy?.mockRestore()
  confirmSpy = null
  act(() => closeDrafts())
})

describe('the drafts list and the bar', () => {
  it('hides the bar at 0 drafts and shows the count otherwise', async () => {
    const { unmount } = render(<Frame />)
    await act(async () => {})
    expect(screen.queryByText(/知识条目草稿/)).toBeNull()
    expect(screen.queryByRole('button', { name: '查看' })).toBeNull()
    unmount()

    listDrafts.mockResolvedValue([summary({ id: 'a', title: '甲' }), summary({ id: 'b', title: '乙' })])
    render(<Frame />)
    expect(await screen.findByText('知识条目草稿：2 份')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '查看' }))
    expect(await screen.findByRole('dialog', { name: '知识条目草稿' })).toHaveTextContent('知识条目草稿：2 份')
  })

  it('lists kind labels and the changed, off, and findings badges', async () => {
    listDrafts.mockResolvedValue([
      summary({ id: '1', title: '甲', kind: 'sop', target: 'changed', findings: [] }),
      summary({ id: '2', title: '乙', kind: 'faq', target: 'none', findings: [] }),
      summary({ id: '3', title: '丙', kind: 'note', target: 'same', findings: ['email'] }),
    ])
    act(() => openDraftList())
    render(<KnowledgeEntryDraftsPanel />)
    expect(await screen.findByRole('button', { name: '甲 操作步骤 需要重新选目标库' })).toBeTruthy()
    expect(screen.getByRole('button', { name: '乙 问答 Knowledge 未开启' })).toBeTruthy()
    expect(screen.getByRole('button', { name: '丙 笔记 含疑似敏感内容' })).toBeTruthy()
    expect(screen.getByRole('button', { name: '关闭' })).toBeTruthy()
  })

  it('leaves out a malformed row, in the list and in the count', async () => {
    listDrafts.mockResolvedValue([summary({ id: 'a', title: '甲' }), { id: 'b' } as unknown as KnowledgeEntryDraftSummary])
    const { unmount } = render(<KnowledgeEntryDraftsBar />)
    expect(await screen.findByText('知识条目草稿：1 份')).toBeTruthy()
    unmount()
    act(() => openDraftList())
    render(<KnowledgeEntryDraftsPanel />)
    expect(await screen.findByText('知识条目草稿：1 份')).toBeTruthy()
    expect(screen.getAllByRole('button').map((b) => b.textContent)).toEqual(['甲 笔记', '关闭'])
  })

  it('hides the bar while the panel is open', async () => {
    listDrafts.mockResolvedValue([summary({ id: 'a', title: '甲' })])
    render(<KnowledgeEntryDraftsBar />)
    expect(await screen.findByRole('button', { name: '查看' })).toBeTruthy()
    act(() => openDraftList())
    expect(screen.queryByRole('button', { name: '查看' })).toBeNull()
  })

  it('refreshes the list and the bar after a draft is deleted', async () => {
    const rows = [summary({ id: 'd1', title: '甲' })]
    listDrafts.mockImplementation(async () => rows.slice())
    getDraft.mockResolvedValue(view({ id: 'd1', title: '甲' }))
    withdrawDraft.mockImplementation(async () => {
      rows.splice(0, rows.length)
      return true
    })
    confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true)
    render(<Frame />)
    expect(await screen.findByText('知识条目草稿：1 份')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '查看' }))
    fireEvent.click(await screen.findByRole('button', { name: '甲 笔记' }))
    expect(await screen.findByLabelText('标题')).toHaveValue('甲')
    fireEvent.click(screen.getByRole('button', { name: '删除草稿' }))
    expect(await screen.findByText('知识条目草稿：0 份')).toBeTruthy()
    expect(screen.queryByLabelText('标题')).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: '关闭' }))
    await waitFor(() => expect(screen.queryByText(/知识条目草稿/)).toBeNull())
    expect(listDrafts.mock.calls.length).toBeGreaterThanOrEqual(3)
  })
})

describe('one draft', () => {
  it('shows the N11 line and keeps 提交审阅 disabled for each reason', async () => {
    // These drafts have a knowledge base, so only the reason under test blocks.
    render(<KnowledgeEntryDraftsPanel />)
    await shown(view({ id: 'c', title: '改投', target: 'changed', findings: [], knowledge_base_ids: ['kb1'] }))
    expect(screen.getByText('需要重新选目标库：Knowledge 现在绑定的不是建草稿时的知识库，这份草稿不会自动改投。')).toBeTruthy()
    expect(screen.getByText('来自对话第 1\u20133 条消息')).toBeTruthy()
    expect(screen.getByRole('button', { name: '提交审阅' })).toBeDisabled()
    expect(screen.getByText('需要重新选目标库')).toBeTruthy()
    expect(screen.queryByRole('button', { name: '一键脱敏' })).toBeNull()

    await shown(view({
      id: 'f',
      title: '敏感',
      target: 'same',
      findings: ['private_key', 'credential', 'email', 'phone', 'id_card', 'bank_card', 'path', 'internal_address'] as KnowledgeEntryFinding[],
      knowledge_base_ids: ['kb1'],
    }))
    expect(screen.getByText('含疑似敏感内容：私钥、凭据、邮箱、电话、证件号、银行卡号、路径、内网地址。提交前请删改，或点「一键脱敏」。')).toBeTruthy()
    expect(screen.getByRole('button', { name: '提交审阅' })).toBeDisabled()
    expect(screen.getByText('请先处理疑似敏感内容')).toBeTruthy()
    expect(screen.getByRole('button', { name: '一键脱敏' })).toBeTruthy()

    await shown(view({ id: 'n', title: '未开', target: 'none', findings: [] }))
    expect(screen.getByText('Knowledge 未开启或未绑定知识库。')).toBeTruthy()
    expect(screen.getByRole('button', { name: '提交审阅' })).toBeDisabled()
    expect(screen.getByText('请先开启 Knowledge 并绑定知识库')).toBeTruthy()
    expect(screen.queryByRole('button', { name: '一键脱敏' })).toBeNull()

    await shown(view({ id: 'b', title: '两者', target: 'changed', findings: ['email'], knowledge_base_ids: ['kb1'] }))
    expect(screen.getByText('需要重新选目标库：Knowledge 现在绑定的不是建草稿时的知识库，这份草稿不会自动改投。')).toBeTruthy()
    expect(screen.getByText(/含疑似敏感内容：邮箱/)).toBeTruthy()
    expect(screen.getByRole('button', { name: '提交审阅' })).toBeDisabled()
    expect(screen.getByText('需要重新选目标库')).toBeTruthy()
    expect(screen.queryByText('请先处理疑似敏感内容')).toBeNull()
  })

  it('shows model and hint notes only on the opening that just created the draft', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    const cases: Array<[KnowledgeEntryModelOutcome, string]> = [
      ['no_model', '没有可用的本机模型，已给出空模板，请自己填写。'],
      ['needs_confirmation', '本机模型需要先确认资源占用，已给出空模板。可以在对话里确认后再试。'],
      ['busy', '本机模型正忙，已给出空模板。'],
      ['failed', '本机模型没有答复，已给出空模板。'],
      ['unusable', '本机模型的答复无法使用，已给出空模板。'],
      ['held_back', '所选消息里有无法自动脱敏的内容，没有交给本机模型，已给出空模板。'],
    ]
    for (const [model, words] of cases) {
      await shown(view({ id: model, title: model }), { model, hints: [] })
      expect(screen.getByText(words)).toBeTruthy()
    }
    await shown(view({ id: 'hinted', title: '提示' }), { model: 'drafted', hints: ['甲', '乙'] })
    expect(screen.queryByText(/空模板/)).toBeNull()
    expect(screen.getByText('模型提示再看一眼（只是提示，不影响提交）：甲、乙')).toBeTruthy()

    listDrafts.mockResolvedValue([summary({ id: 'hinted', title: '提示' })])
    act(() => openDraftList())
    fireEvent.click(await screen.findByRole('button', { name: '提示 笔记' }))
    expect(await screen.findByLabelText('标题')).toHaveValue('提示')
    expect(screen.queryByText(/模型提示再看一眼/)).toBeNull()
    expect(screen.queryByText(/空模板/)).toBeNull()
  })

  it('says when the draft is already gone', async () => {
    getDraft.mockResolvedValue(null)
    act(() => openDraft('missing'))
    render(<KnowledgeEntryDraftsPanel />)
    expect(await screen.findByText('这份草稿已经不在了。')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '返回列表' }))
    expect(await screen.findByRole('dialog', { name: '知识条目草稿' })).toBeTruthy()
    expect(screen.queryByText('这份草稿已经不在了。')).toBeNull()
  })

  it('keeps the body as textarea text', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(view({ body: 'line\n<b>bold</b>' }))
    const area = screen.getByLabelText('正文') as HTMLTextAreaElement
    expect(area.value).toBe('line\n<b>bold</b>')
    expect(area.tagName).toBe('TEXTAREA')
    expect(document.querySelector('b')).toBeNull()
  })

  it('saves the current fields, and says each failure', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(view({ id: 'd1', kind: 'note', title: '甲', body: '正文' }))
    expect(screen.getByRole('button', { name: '保存' })).toBeDisabled()
    fireEvent.change(screen.getByLabelText('类型'), { target: { value: 'faq' } })
    fireEvent.change(screen.getByLabelText('标题'), { target: { value: '新标题' } })
    fireEvent.change(screen.getByLabelText('正文'), { target: { value: '新正文' } })
    let resolveSave: (value: unknown) => void = () => {}
    updateDraft.mockImplementation(() => new Promise((resolve) => { resolveSave = resolve }))
    fireEvent.click(screen.getByRole('button', { name: '保存' }))
    expect(screen.getByRole('button', { name: '保存' })).toBeDisabled()
    expect(updateDraft).toHaveBeenCalledTimes(1)
    expect(updateDraft).toHaveBeenCalledWith('d1', { kind: 'faq', title: '新标题', body: '新正文' })
    const saved = view({ id: 'd1', kind: 'faq', title: '新标题', body: '新正文' })
    await act(async () => resolveSave({ ok: true, draft: saved }))
    expect(screen.getByLabelText('标题')).toHaveValue('新标题')
    expect(screen.getByLabelText('正文')).toHaveValue('新正文')
    expect(screen.getByLabelText('类型')).toHaveValue('faq')
    expect(screen.getByRole('button', { name: '保存' })).toBeDisabled()

    const failures: Array<[unknown, string]> = [
      [{ ok: false, reason: 'invalid' }, '没能保存：标题不能为空，正文不能超过 64 KiB。'],
      [{ ok: false, reason: 'not_found' }, '这份草稿已经不在了。'],
      [{ ok: false, reason: 'not_saved' }, '草稿没能保存，请稍后再试。'],
      [{ ok: 'nope' }, '草稿没能保存，请稍后再试。'],
    ]
    for (const [reply, words] of failures) {
      updateDraft.mockReset()
      updateDraft.mockResolvedValueOnce(reply)
      fireEvent.change(screen.getByLabelText('标题'), { target: { value: `改${words.length}` } })
      fireEvent.click(screen.getByRole('button', { name: '保存' }))
      expect(await screen.findByRole('status')).toHaveTextContent(words)
      expect(updateDraft).toHaveBeenCalledTimes(1)
    }
    updateDraft.mockReset()
    updateDraft.mockRejectedValueOnce(new Error('ipc'))
    fireEvent.change(screen.getByLabelText('标题'), { target: { value: '再改' } })
    fireEvent.click(screen.getByRole('button', { name: '保存' }))
    expect(await screen.findByRole('status')).toHaveTextContent('草稿没能保存，请稍后再试。')
  })

  it('offers 一键脱敏 only while findings remain, and applies its result the same way', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(view({ findings: [] }))
    expect(screen.queryByRole('button', { name: '一键脱敏' })).toBeNull()
    await shown(view({ id: 'red', title: '敏感', body: 'sk-secret', findings: ['credential'] }))
    redactDraft.mockResolvedValue({ ok: true, draft: view({ id: 'red', title: '敏感', body: '[credential]', findings: [] }) })
    fireEvent.click(screen.getByRole('button', { name: '一键脱敏' }))
    await waitFor(() => expect(redactDraft).toHaveBeenCalledWith('red'))
    expect(await screen.findByLabelText('正文')).toHaveValue('[credential]')
    expect(screen.queryByRole('button', { name: '一键脱敏' })).toBeNull()
    expect(screen.queryByText(/含疑似敏感内容/)).toBeNull()

    await shown(view({ id: 'bad', findings: ['email'] }))
    redactDraft.mockResolvedValueOnce({ ok: false, reason: 'not_saved' })
    fireEvent.click(screen.getByRole('button', { name: '一键脱敏' }))
    expect(await screen.findByRole('status')).toHaveTextContent('草稿没能保存，请稍后再试。')
  })

  it('asks before redacting over unsaved edits, and keeps them on no', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(view({ id: 'edit', title: '敏感', body: 'sk-secret', findings: ['credential'] }))
    fireEvent.change(screen.getByLabelText('正文'), { target: { value: '我改过的正文' } })
    confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(false)
    fireEvent.click(screen.getByRole('button', { name: '一键脱敏' }))
    expect(window.confirm).toHaveBeenCalledWith('有未保存的修改，确定放弃？')
    expect(redactDraft).not.toHaveBeenCalled()
    expect(screen.getByLabelText('正文')).toHaveValue('我改过的正文')
    confirmSpy.mockRestore()
    confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true)
    redactDraft.mockResolvedValue({ ok: true, draft: view({ id: 'edit', title: '敏感', body: '[credential]', findings: [] }) })
    fireEvent.click(screen.getByRole('button', { name: '一键脱敏' }))
    await waitFor(() => expect(redactDraft).toHaveBeenCalledWith('edit'))
  })

  it('deletes the draft only after confirm, and says so when the reply is false', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(view({ id: 'd1', title: '甲' }))
    confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(false)
    fireEvent.click(screen.getByRole('button', { name: '删除草稿' }))
    expect(window.confirm).toHaveBeenCalledWith('确定从本机删除这份草稿？删除后不能恢复。已经提交审阅的内容不会因此撤回，需要的话请先撤回提交。')
    expect(withdrawDraft).not.toHaveBeenCalled()
    expect(screen.getByLabelText('标题')).toHaveValue('甲')

    confirmSpy.mockRestore()
    confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true)
    withdrawDraft.mockResolvedValueOnce(false)
    fireEvent.click(screen.getByRole('button', { name: '删除草稿' }))
    expect(await screen.findByRole('status')).toHaveTextContent('没能删除草稿，请稍后再试。')
    expect(withdrawDraft).toHaveBeenCalledWith('d1')
    expect(screen.getByLabelText('标题')).toHaveValue('甲')

    listDrafts.mockResolvedValue([])
    withdrawDraft.mockResolvedValueOnce(true)
    fireEvent.click(screen.getByRole('button', { name: '删除草稿' }))
    expect(await screen.findByText('知识条目草稿：0 份')).toBeTruthy()
    expect(screen.queryByLabelText('标题')).toBeNull()
  })

  it('asks before discarding unsaved edits', async () => {
    render(<Frame />)
    await shown(view({ id: 'd1', title: '甲', body: '正文' }))
    fireEvent.click(screen.getByRole('button', { name: '关闭' }))
    expect(screen.queryByRole('dialog')).toBeNull()

    await shown(view({ id: 'd1', title: '甲', body: '正文' }))
    fireEvent.change(screen.getByLabelText('标题'), { target: { value: '未保存' } })
    confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(false)
    fireEvent.click(screen.getByRole('button', { name: '返回列表' }))
    expect(window.confirm).toHaveBeenCalledWith('有未保存的修改，确定放弃？')
    expect(screen.getByLabelText('标题')).toHaveValue('未保存')

    confirmSpy.mockRestore()
    confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true)
    fireEvent.click(screen.getByRole('button', { name: '返回列表' }))
    expect(await screen.findByRole('dialog', { name: '知识条目草稿' })).toBeTruthy()
    expect(screen.queryByLabelText('标题')).toBeNull()

    await shown(view({ id: 'd1', title: '甲' }))
    fireEvent.change(screen.getByLabelText('正文'), { target: { value: '改过' } })
    fireEvent.click(screen.getByRole('button', { name: '关闭' }))
    expect(window.confirm).toHaveBeenCalledWith('有未保存的修改，确定放弃？')
    expect(screen.queryByRole('dialog')).toBeNull()
  })

  it('locks the fields while a save is in flight, so nothing typed meanwhile is overwritten (Codex C1)', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(view({ id: 'busy', title: '甲', body: '原文' }))
    let release: (v: unknown) => void = () => {}
    updateDraft.mockReturnValue(new Promise((resolve) => { release = resolve }))
    fireEvent.change(screen.getByLabelText('正文'), { target: { value: '改一' } })
    fireEvent.click(screen.getByRole('button', { name: '保存' }))
    expect(screen.getByLabelText('正文')).toBeDisabled()
    expect(screen.getByLabelText('标题')).toBeDisabled()
    expect(screen.getByLabelText('类型')).toBeDisabled()
    await act(async () => { release({ ok: true, draft: view({ id: 'busy', title: '甲', body: '改一' }) }) })
    expect(screen.getByLabelText('正文')).not.toBeDisabled()
    expect(screen.getByLabelText('正文')).toHaveValue('改一')
  })

  it('works under StrictMode: a reply after a save clears busy (Codex C3)', async () => {
    render(<StrictMode><KnowledgeEntryDraftsPanel /></StrictMode>)
    await shown(view({ id: 'strict', title: '甲', body: '原文' }))
    updateDraft.mockResolvedValue({ ok: true, draft: view({ id: 'strict', title: '甲', body: '改过' }) })
    fireEvent.change(screen.getByLabelText('正文'), { target: { value: '改过' } })
    fireEvent.click(screen.getByRole('button', { name: '保存' }))
    await waitFor(() => expect(screen.getByRole('button', { name: '删除草稿' })).not.toBeDisabled())
    expect(screen.getByLabelText('正文')).not.toBeDisabled()
  })

  it('asks before a new draft replaces one with unsaved edits, and stays on no (Codex C2)', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(view({ id: 'first', title: '甲' }))
    fireEvent.change(screen.getByLabelText('正文'), { target: { value: '没保存的' } })
    confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(false)
    getDraft.mockResolvedValue(view({ id: 'second', title: '乙' }))
    act(() => openDraft('second'))
    expect(window.confirm).toHaveBeenCalledWith('有未保存的修改，确定放弃？')
    expect(screen.getByLabelText('正文')).toHaveValue('没保存的')
    confirmSpy.mockRestore()
    confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true)
    act(() => openDraft('second'))
    expect(await screen.findByDisplayValue('乙')).toBeTruthy()
  })

  it('shows nothing of the last draft while the next one loads (Codex round 2 C1)', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(view({ id: 'first', title: '甲', body: '原文' }))
    fireEvent.change(screen.getByLabelText('正文'), { target: { value: '没保存的' } })
    confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true)
    let deliver: (v: KnowledgeEntryDraftView) => void = () => {}
    getDraft.mockReturnValue(new Promise((resolve) => { deliver = resolve }))
    act(() => openDraft('second'))
    expect(screen.queryByLabelText('正文')).toBeNull()
    expect(screen.queryByDisplayValue('没保存的')).toBeNull()
    expect(screen.queryByRole('button', { name: '保存' })).toBeNull()
    await act(async () => { deliver(view({ id: 'second', title: '乙', body: '二' })) })
    expect(screen.getByLabelText('标题')).toHaveValue('乙')
    expect(screen.getByLabelText('正文')).toHaveValue('二')
    expect(updateDraft).not.toHaveBeenCalled()
  })

  it('drops a reply about the last draft once the next one is open (Codex round 2 C1)', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(view({ id: 'first', title: '甲', body: '原文' }))
    let release: (v: unknown) => void = () => {}
    updateDraft.mockReturnValue(new Promise((resolve) => { release = resolve }))
    fireEvent.change(screen.getByLabelText('正文'), { target: { value: '改一' } })
    fireEvent.click(screen.getByRole('button', { name: '保存' }))
    confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true)
    getDraft.mockResolvedValue(view({ id: 'second', title: '乙', body: '二' }))
    act(() => openDraft('second'))
    expect(await screen.findByDisplayValue('乙')).toBeTruthy()
    expect(screen.getByRole('button', { name: '删除草稿' })).not.toBeDisabled()
    await act(async () => { release({ ok: true, draft: view({ id: 'first', title: '甲', body: '改一' }) }) })
    expect(screen.getByLabelText('标题')).toHaveValue('乙')
    expect(screen.getByLabelText('正文')).toHaveValue('二')
  })

  it('takes a hidden panel off screen even under a display rule (Codex round 2 C2)', async () => {
    listDrafts.mockResolvedValue([summary({ id: 'a', title: '甲' })])
    act(() => openDraftList())
    const { container, rerender } = render(<KnowledgeEntryDraftsPanel hidden />)
    await waitFor(() => expect(container.querySelector('li')).not.toBeNull())
    const panel = container.querySelector('section') as HTMLElement
    expect(panel).toHaveAttribute('hidden')
    expect(panel.style.display).toBe('none')
    rerender(<KnowledgeEntryDraftsPanel />)
    expect(panel).not.toHaveAttribute('hidden')
    expect(panel.style.display).toBe('')
  })

  it('says why when redacting would make the body too long (Codex C4)', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(view({ id: 'long', findings: ['credential'] }))
    redactDraft.mockResolvedValueOnce({ ok: false, reason: 'too_long' })
    fireEvent.click(screen.getByRole('button', { name: '一键脱敏' }))
    expect(await screen.findByRole('status')).toHaveTextContent('脱敏后正文会超过 64 KiB，请先手工删减。')
  })
})

describe('M4 A7: leaving the page with unsaved edits', () => {
  function Pages(): JSX.Element {
    const { route } = useCapabilityRoute()
    const { open } = useContext(CapabilitiesContext)
    return <>
      <Frame />
      <button onClick={() => open('documents', 'switcher')}>documents</button>
      <output aria-label="Route">{route.kind === 'page' ? route.page : route.id}</output>
    </>
  }

  it('asks once, in the draft panel\'s words, and stays on no; no edits, no question', async () => {
    Object.defineProperty(window, 'agent24', {
      configurable: true,
      value: { knowledgeEntry: { listDrafts, getDraft, updateDraft, redactDraft, withdrawDraft }, backendProxy: vi.fn().mockResolvedValue({ ok: false }) },
    })
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false)
    confirmSpy = confirm
    render(<CapabilitiesProvider><Pages /></CapabilitiesProvider>)
    await shown(view())
    const go = () => fireEvent.click(screen.getByRole('button', { name: 'documents' }))
    const at = () => screen.getByLabelText('Route').textContent
    const before = at()

    fireEvent.change(screen.getByLabelText('标题'), { target: { value: '甲改' } })
    go()
    expect(confirm).toHaveBeenCalledTimes(1)
    expect(confirm).toHaveBeenCalledWith('有未保存的修改，确定放弃？')
    expect(at()).toBe(before)

    fireEvent.change(screen.getByLabelText('标题'), { target: { value: '甲' } })
    go()
    expect(confirm).toHaveBeenCalledTimes(1)
    expect(at()).toBe('documents')
  })
})

describe('H2: submitting a draft for review', () => {
  type KnowledgeEntryCandidateView = NonNullable<KnowledgeEntrySubmission['candidate']>
  function candidate(over: Partial<KnowledgeEntryCandidateView> = {}): KnowledgeEntryCandidateView {
    return {
      status: 'submitted',
      target_knowledge_base_id: 'kb1',
      edited: false,
      return_reason: null,
      note: null,
      submitted_at: '2026-01-02T03:00:00.000Z',
      checked_at: '2026-01-02T03:04:05.000Z',
      ...over,
    }
  }
  function sub(over: Partial<KnowledgeEntrySubmission> = {}): KnowledgeEntrySubmission {
    return { pending: false, stuck: false, candidate: null, ...over }
  }
  const ready = (over: Partial<KnowledgeEntryDraftView> = {}): KnowledgeEntryDraftView =>
    view({ id: 'd1', title: '甲', knowledge_base_ids: ['kb1'], ...over })
  const button = (name: string) => screen.queryByRole('button', { name })

  it('submits to the one bound knowledge base and then shows it under review', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(ready())
    expect(screen.queryByLabelText('知识库')).toBeNull()
    submit.mockResolvedValueOnce({ ok: true, submission: sub({ candidate: candidate() }) })
    fireEvent.click(screen.getByRole('button', { name: '提交审阅' }))
    expect(await screen.findByText('已提交到 kb1 等待审阅。')).toBeTruthy()
    expect(submit).toHaveBeenCalledWith('d1', 'kb1')
    expect(button('提交审阅')).toBeNull()
    expect(button('撤回提交')).toBeTruthy()
    expect(button('查看状态')).toBeTruthy()
    expect(screen.getByText(/^上次查看：.+。$/)).toBeTruthy()
    expect(screen.queryByRole('status')).toBeNull()
  })

  it('lets the user choose among several bound knowledge bases', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(ready({ knowledge_base_ids: ['kb1', 'kb2'] }))
    fireEvent.change(screen.getByLabelText('知识库'), { target: { value: 'kb2' } })
    submit.mockResolvedValueOnce({ ok: true, submission: sub({ candidate: candidate({ target_knowledge_base_id: 'kb2' }) }) })
    fireEvent.click(screen.getByRole('button', { name: '提交审阅' }))
    expect(await screen.findByText('已提交到 kb2 等待审阅。')).toBeTruthy()
    expect(submit).toHaveBeenCalledWith('d1', 'kb2')
  })

  it('keeps 提交审阅 disabled with the reason: format, unsaved edits, no knowledge base', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(ready({ format: { kind: 'raw_html', line: 3 } }))
    expect(screen.getByRole('button', { name: '提交审阅' })).toBeDisabled()
    expect(screen.getByText(/^第 3 行用了知识库不接受的格式/)).toBeTruthy()

    await shown(ready({ id: 'e', title: '乙' }))
    expect(screen.getByRole('button', { name: '提交审阅' })).not.toBeDisabled()
    fireEvent.change(screen.getByLabelText('正文'), { target: { value: '改过' } })
    expect(screen.getByRole('button', { name: '提交审阅' })).toBeDisabled()
    expect(screen.getByText('请先保存修改')).toBeTruthy()

    confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true)
    await shown(ready({ id: 'k', title: '丙', knowledge_base_ids: [] }))
    expect(screen.getByRole('button', { name: '提交审阅' })).toBeDisabled()
    expect(screen.getByText('请先开启 Knowledge 并绑定知识库')).toBeTruthy()
    expect(submit).not.toHaveBeenCalled()
  })

  it('offers a resend for a pending submit, and nothing for a stuck one', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(ready({ submission: sub({ pending: true }) }))
    expect(screen.getByText(/^已发出，但还不知道是否送达。/)).toBeTruthy()
    expect(button('提交审阅')).toBeNull()
    expect(button('重新提交')).not.toBeDisabled()
    submit.mockResolvedValueOnce({ ok: true, submission: sub({ candidate: candidate() }) })
    fireEvent.click(screen.getByRole('button', { name: '重新提交' }))
    expect(await screen.findByText('已提交到 kb1 等待审阅。')).toBeTruthy()
    expect(button('重新提交')).toBeNull()

    await shown(ready({ id: 'c', title: '乙', target: 'changed', submission: sub({ pending: true }) }))
    expect(button('重新提交')).toBeDisabled()

    await shown(ready({ id: 's', title: '丙', submission: sub({ pending: true, stuck: true }) }))
    expect(screen.getByText(/^已经无法确认这次提交是否送达/)).toBeTruthy()
    expect(button('提交审阅')).toBeNull()
    expect(button('重新提交')).toBeNull()
  })

  it('shows approved, returned with reason and note, and withdrawn', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(ready({ submission: sub({ candidate: candidate({ status: 'approved', edited: true }) }) }))
    expect(screen.getByText('审阅已通过。')).toBeTruthy()
    expect(screen.getByText('提交之后改动过。')).toBeTruthy()
    expect(button('提交审阅')).toBeNull()
    expect(button('撤回提交')).toBeNull()
    expect(button('查看状态')).toBeTruthy()

    await shown(ready({ id: 'r', title: '乙', submission: sub({ candidate: candidate({ status: 'returned', return_reason: 'conflicted', note: '和第 3 条重复' }) }) }))
    expect(screen.getByText('审阅人已退回：与已有条目冲突。')).toBeTruthy()
    expect(screen.getByText('审阅人备注：和第 3 条重复')).toBeTruthy()
    expect(button('提交审阅')).not.toBeDisabled()
    expect(button('撤回提交')).toBeNull()

    await shown(ready({ id: 'o', title: '丙', submission: sub({ candidate: candidate({ status: 'returned', return_reason: 'toString', note: null }) }) }))
    expect(screen.getByText('审阅人已退回：其他原因。')).toBeTruthy()

    await shown(ready({ id: 'w', title: '丁', submission: sub({ candidate: candidate({ status: 'withdrawn' }) }) }))
    expect(screen.getByText('你已撤回这次提交，可以重新提交这份草稿。')).toBeTruthy()
    expect(button('提交审阅')).not.toBeDisabled()
  })

  it('checks the status and shows what came back', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(ready({ submission: sub({ candidate: candidate() }) }))
    submitStatus.mockResolvedValueOnce({ ok: true, submission: sub({ candidate: candidate({ status: 'approved' }) }) })
    fireEvent.click(screen.getByRole('button', { name: '查看状态' }))
    expect(await screen.findByText('审阅已通过。')).toBeTruthy()
    expect(submitStatus).toHaveBeenCalledWith('d1')
    expect(screen.queryByText('已提交到 kb1 等待审阅。')).toBeNull()
  })

  it('withdraws a submission only after confirm and keeps the draft', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(ready({ submission: sub({ candidate: candidate() }) }))
    confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(false)
    fireEvent.click(screen.getByRole('button', { name: '撤回提交' }))
    expect(window.confirm).toHaveBeenCalledWith('确定撤回这次提交？草稿仍保留在本机。')
    expect(retractSubmission).not.toHaveBeenCalled()

    confirmSpy.mockRestore()
    confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true)
    retractSubmission.mockResolvedValueOnce({ ok: true, submission: sub({ candidate: candidate({ status: 'withdrawn' }) }) })
    fireEvent.click(screen.getByRole('button', { name: '撤回提交' }))
    expect(await screen.findByText('你已撤回这次提交，可以重新提交这份草稿。')).toBeTruthy()
    expect(retractSubmission).toHaveBeenCalledWith('d1')
    expect(withdrawDraft).not.toHaveBeenCalled()
    expect(screen.getByLabelText('标题')).toHaveValue('甲')
  })

  it('deleting the draft never talks to the knowledge base', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(ready({ submission: sub({ candidate: candidate() }) }))
    confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true)
    withdrawDraft.mockResolvedValueOnce(true)
    fireEvent.click(screen.getByRole('button', { name: '删除草稿' }))
    await waitFor(() => expect(withdrawDraft).toHaveBeenCalledWith('d1'))
    expect(retractSubmission).not.toHaveBeenCalled()
    expect(submit).not.toHaveBeenCalled()
    expect(submitStatus).not.toHaveBeenCalled()
  })

  const LEAK = 'sk-live-ABCDEF body: <script>'
  const kinds: Array<[Record<string, unknown>, Record<'zh' | 'en' | 'th', string>]> = [
    [{ code: 'forbidden', unknown: false }, { zh: '知识库拒绝了：这个密钥无权向它提交条目。请检查 Knowledge 设置。', en: 'The knowledge base refused: this key may not submit entries to it. Check Knowledge’s settings.', th: 'คลังความรู้ปฏิเสธ: คีย์นี้ไม่มีสิทธิ์ส่งรายการไปที่นั่น โปรดตรวจการตั้งค่า Knowledge' }],
    [{ code: 'not_ready', unknown: false }, { zh: '知识库现在不接收提交，请稍后再试。', en: 'The knowledge base is not taking submissions right now. Try again later.', th: 'คลังความรู้ยังไม่รับการส่งในตอนนี้ โปรดลองใหม่ภายหลัง' }],
    [{ code: 'stale', unknown: false }, { zh: '知识库没有接受这次请求。请先查看草稿状态，再重试。', en: 'The knowledge base did not accept this request. Check the draft status, then try again.', th: 'คลังความรู้ไม่รับคำขอนี้ โปรดดูสถานะฉบับร่างก่อนแล้วลองใหม่' }],
    [{ code: 'internal', unknown: false }, { zh: '和知识库通信时出错，没有做任何改动，请稍后再试。', en: 'Something went wrong talking to the knowledge base. Nothing was changed; try again in a moment.', th: 'เกิดข้อผิดพลาดระหว่างติดต่อคลังความรู้ ไม่มีอะไรเปลี่ยนแปลง โปรดลองอีกครั้งในอีกสักครู่' }],
    [{ code: 'internal', unknown: true }, { zh: '没有收到明确答复，提交可能送达也可能没有。重新提交是安全的，发出的是同一份提交。', en: 'No clear answer came back, so the submission may or may not have arrived. Trying again is safe: it sends the same submission.', th: 'ไม่ได้รับคำตอบที่ชัดเจน การส่งอาจถึงหรือไม่ถึงก็ได้ การส่งอีกครั้งปลอดภัย เพราะจะส่งรายการเดิม' }],
  ]
  const submitLabel = { zh: '提交审阅', en: 'Submit for review', th: 'ส่งให้ตรวจ' } as const
  const failed = kinds[3][1]
  // QA H2 Medium: the write reasons get their own advice, each only under the code WeKnora sends it with.
  const reasons: Array<[Record<string, unknown>, Record<'zh' | 'en' | 'th', string>]> = [
    [{ code: 'forbidden', serverReason: 'quota_exceeded' }, { zh: '你待审的条目已满，等审阅人处理一些之后再提交。', en: 'Too many of your entries are waiting for review. Submit again once the reviewers have dealt with some.', th: 'รายการของคุณที่รอตรวจเต็มแล้ว โปรดส่งอีกครั้งหลังผู้ตรวจจัดการไปบางส่วน' }],
    [{ code: 'forbidden', serverReason: 'empty_scope' }, { zh: '这个密钥没有可以提交的知识库范围，请检查 Knowledge 设置。', en: 'This key has no knowledge base it may submit to. Check Knowledge’s settings.', th: 'คีย์นี้ไม่มีคลังความรู้ที่ส่งได้ โปรดตรวจการตั้งค่า Knowledge' }],
    [{ code: 'invalid_request', serverReason: 'unsupported_format' }, { zh: '知识库不接受正文里的部分格式（例如 HTML、图片、脚注，或不是普通 https 地址的链接），请修改后再提交。', en: 'The knowledge base does not accept some formatting in the body (such as HTML, images, footnotes or a link that is not a plain https address). Edit it before submitting.', th: 'คลังความรู้ไม่รับรูปแบบบางอย่างในเนื้อหา (เช่น HTML รูปภาพ เชิงอรรถ หรือลิงก์ที่ไม่ใช่ที่อยู่ https ธรรมดา) โปรดแก้ไขก่อนส่ง' }],
    [{ code: 'invalid_request', serverReason: 'too_large' }, { zh: '正文超过知识库接受的长度（64 KiB），请删减后再提交。', en: 'The body is longer than the knowledge base accepts (64 KiB). Shorten it before submitting.', th: 'เนื้อหายาวเกินกว่าที่คลังความรู้รับ (64 KiB) โปรดตัดให้สั้นลงก่อนส่ง' }],
    // Main's own "answer too long to read" is not the body: a plain failure.
    [{ code: 'internal', serverReason: 'too_large' }, failed],
    // Under another code a reason is not taken: forbidden stays a permission problem.
    [{ code: 'forbidden', serverReason: 'too_large' }, kinds[0][1]],
    [{ code: 'invalid_request', serverReason: 'quota_exceeded' }, kinds[2][1]],
    // An unknown outcome is said first, whatever reason came with it.
    [{ code: 'forbidden', serverReason: 'quota_exceeded', unknown: true }, kinds[4][1]],
  ]

  for (const locale of ['zh', 'en', 'th'] as const) {
    it(`tells the five kinds of server answer apart in ${locale}, echoing nothing of the answer`, async () => {
      setLocale(locale)
      for (const [answer, words] of kinds) {
        const { unmount } = render(<KnowledgeEntryDraftsPanel />)
        getDraft.mockResolvedValue(ready({ body: '正文' }))
        act(() => openDraft('d1'))
        const go = await screen.findByRole('button', { name: submitLabel[locale] })
        submit.mockResolvedValueOnce({ ok: false, reason: 'server', serverReason: LEAK, ...answer, submission: sub({ pending: answer.unknown === true }) })
        fireEvent.click(go)
        expect(await screen.findByRole('status')).toHaveTextContent(words[locale])
        expect(document.body.textContent).not.toContain('sk-live')
        expect(document.body.textContent).not.toContain('<script>')
        unmount()
        act(() => closeDrafts())
      }
    })
  }

  for (const locale of ['zh', 'en', 'th'] as const) {
    it(`gives quota, empty scope, format and size their own words in ${locale}, by code and reason together`, async () => {
      setLocale(locale)
      const { unmount } = render(<KnowledgeEntryDraftsPanel />)
      getDraft.mockResolvedValue(ready())
      act(() => openDraft('d1'))
      const go = await screen.findByRole('button', { name: submitLabel[locale] })
      for (const [answer, words] of reasons) {
        submit.mockResolvedValueOnce({ ok: false, reason: 'server', unknown: false, ...answer, submission: sub() })
        fireEvent.click(go)
        await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent(words[locale]))
        expect(document.body.textContent).not.toContain(String(answer.serverReason))
      }
      // A reason that is not a string fails the check: a plain failure, nothing applied.
      submit.mockResolvedValueOnce({ ok: false, reason: 'server', code: 'forbidden', unknown: false, serverReason: 7, submission: sub({ candidate: candidate({ status: 'approved' }) }) })
      fireEvent.click(go)
      await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent(failed[locale]))
      expect(screen.getByRole('button', { name: submitLabel[locale] })).toBeTruthy()
      unmount()
    })
  }

  it('says each local refusal in fixed words, and a malformed or failed call as a failure', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(ready())
    const cases: Array<[unknown, string]> = [
      [{ ok: false, reason: 'busy' }, '这份草稿正在提交中。'],
      [{ ok: false, reason: 'local_storage' }, '本机没能读写提交记录，这次没有发给知识库。请稍后再试。'],
      [{ ok: false, reason: 'already_submitted' }, '这份草稿已在审阅中或已通过。'],
      [{ ok: false, reason: 'invalid_target' }, '请选择 Knowledge 已绑定的知识库之一。'],
      [{ ok: false, reason: 'unsupported_format', problem: { kind: 'image', line: 7 } }, '第 7 行用了知识库不接受的格式'],
      [{ ok: false, reason: 'server', code: 'x' }, '和知识库通信时出错'],
      [{ ok: 'yes' }, '和知识库通信时出错'],
      // Codex H2 C1: a refusal outside the known set, or a malformed problem, is a failure.
      [{ ok: false, reason: 'unexpected' }, '和知识库通信时出错'],
      [{ ok: false, reason: 'unsupported_format', problem: { kind: 'image', line: '任意文本' } }, '和知识库通信时出错'],
      [{ ok: false, reason: 'unsupported_format', problem: { kind: 'image', line: 0 } }, '和知识库通信时出错'],
    ]
    for (const [reply, words] of cases) {
      submit.mockResolvedValueOnce(reply)
      fireEvent.click(screen.getByRole('button', { name: '提交审阅' }))
      await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent(words))
    }
    // Nothing of an answer that failed the check is applied, not even its submission.
    submit.mockResolvedValueOnce({ ok: false, reason: 'unexpected', submission: sub({ candidate: candidate({ status: 'approved' }) }) })
    fireEvent.click(screen.getByRole('button', { name: '提交审阅' }))
    await waitFor(() => expect(submit).toHaveBeenCalledTimes(cases.length + 1))
    await waitFor(() => expect(screen.getByRole('button', { name: '提交审阅' })).not.toBeDisabled())
    expect(screen.queryByText('审阅已通过。')).toBeNull()
    submit.mockRejectedValueOnce(new Error('ipc'))
    fireEvent.click(screen.getByRole('button', { name: '提交审阅' }))
    await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('和知识库通信时出错，没有做任何改动，请稍后再试。'))
    expect(screen.getByLabelText('标题')).toHaveValue('甲')
  })

  it('tells a local_storage answer apart: nothing sent, or done on the server but not recorded (Codex H2 C2)', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(ready({ submission: sub({ candidate: candidate() }) }))
    const notRecorded = '知识库已经处理了这次请求，但本机没能记下结果。稍后再操作一次就能补上，不会重复提交或重复撤回。'
    const notSent = '本机没能读写提交记录，这次没有发给知识库。请稍后再试。'
    submitStatus.mockResolvedValueOnce({ ok: false, reason: 'local_storage', submission: sub({ candidate: candidate() }) })
    fireEvent.click(screen.getByRole('button', { name: '查看状态' }))
    expect(await screen.findByRole('status')).toHaveTextContent(notRecorded)
    confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true)
    retractSubmission.mockResolvedValueOnce({ ok: false, reason: 'local_storage', submission: sub({ candidate: candidate() }) })
    fireEvent.click(screen.getByRole('button', { name: '撤回提交' }))
    await waitFor(() => expect(retractSubmission).toHaveBeenCalledTimes(1))
    expect(await screen.findByRole('status')).toHaveTextContent(notRecorded)
    submitStatus.mockResolvedValueOnce({ ok: false, reason: 'local_storage' })
    fireEvent.click(screen.getByRole('button', { name: '查看状态' }))
    await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent(notSent))
    expect(screen.queryByText(/没能记下这次提交/)).toBeNull()
  })

  it('locks the submit buttons while an answer is outstanding', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(ready())
    let release: (v: unknown) => void = () => {}
    submit.mockReturnValue(new Promise((resolve) => { release = resolve }))
    fireEvent.click(screen.getByRole('button', { name: '提交审阅' }))
    expect(screen.getByRole('button', { name: '提交审阅' })).toBeDisabled()
    fireEvent.click(screen.getByRole('button', { name: '提交审阅' }))
    expect(submit).toHaveBeenCalledTimes(1)
    await act(async () => { release({ ok: true, submission: sub({ candidate: candidate() }) }) })
    expect(screen.getByRole('button', { name: '撤回提交' })).not.toBeDisabled()
  })

  it('leaves out a draft whose submission is malformed', async () => {
    getDraft.mockResolvedValue({ ...ready(), submission: { pending: 'yes' } } as unknown as KnowledgeEntryDraftView)
    act(() => openDraft('d1'))
    render(<KnowledgeEntryDraftsPanel />)
    expect(await screen.findByText('这份草稿已经不在了。')).toBeTruthy()
  })
})
