// @vitest-environment jsdom
import { StrictMode, useContext } from 'react'
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import type { KnowledgeEntryDraftSummary, KnowledgeEntryDraftView, KnowledgeEntryFinding, KnowledgeEntryModelOutcome } from '../../shared/ipc-types'
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
    value: { knowledgeEntry: { listDrafts, getDraft, updateDraft, redactDraft, withdrawDraft } },
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

  it('refreshes the list and the bar after a draft is withdrawn', async () => {
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
    fireEvent.click(screen.getByRole('button', { name: '撤回' }))
    expect(await screen.findByText('知识条目草稿：0 份')).toBeTruthy()
    expect(screen.queryByLabelText('标题')).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: '关闭' }))
    await waitFor(() => expect(screen.queryByText(/知识条目草稿/)).toBeNull())
    expect(listDrafts.mock.calls.length).toBeGreaterThanOrEqual(3)
  })
})

describe('one draft', () => {
  it('shows the N11 line and keeps 提交审阅 disabled for each reason', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(view({ id: 'c', title: '改投', target: 'changed', findings: [] }))
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
    }))
    expect(screen.getByText('含疑似敏感内容：私钥、凭据、邮箱、电话、证件号、银行卡号、路径、内网地址。提交前请删改，或点「一键脱敏」。')).toBeTruthy()
    expect(screen.getByRole('button', { name: '提交审阅' })).toBeDisabled()
    expect(screen.getByText('请先处理疑似敏感内容')).toBeTruthy()
    expect(screen.getByRole('button', { name: '一键脱敏' })).toBeTruthy()

    await shown(view({ id: 'n', title: '未开', target: 'none', findings: [] }))
    expect(screen.getByText('Knowledge 未开启或未绑定知识库。')).toBeTruthy()
    expect(screen.getByRole('button', { name: '提交审阅' })).toBeDisabled()
    expect(screen.getByText('提交审阅将在后续版本提供')).toBeTruthy()
    expect(screen.queryByRole('button', { name: '一键脱敏' })).toBeNull()

    await shown(view({ id: 'b', title: '两者', target: 'changed', findings: ['email'] }))
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

  it('withdraws only after confirm, and says so when the reply is false', async () => {
    render(<KnowledgeEntryDraftsPanel />)
    await shown(view({ id: 'd1', title: '甲' }))
    confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(false)
    fireEvent.click(screen.getByRole('button', { name: '撤回' }))
    expect(window.confirm).toHaveBeenCalledWith('撤回后这份草稿会从本机删除，不能恢复。确定撤回？')
    expect(withdrawDraft).not.toHaveBeenCalled()
    expect(screen.getByLabelText('标题')).toHaveValue('甲')

    confirmSpy.mockRestore()
    confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true)
    withdrawDraft.mockResolvedValueOnce(false)
    fireEvent.click(screen.getByRole('button', { name: '撤回' }))
    expect(await screen.findByRole('status')).toHaveTextContent('没能撤回，请稍后再试。')
    expect(withdrawDraft).toHaveBeenCalledWith('d1')
    expect(screen.getByLabelText('标题')).toHaveValue('甲')

    listDrafts.mockResolvedValue([])
    withdrawDraft.mockResolvedValueOnce(true)
    fireEvent.click(screen.getByRole('button', { name: '撤回' }))
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
    await waitFor(() => expect(screen.getByRole('button', { name: '撤回' })).not.toBeDisabled())
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
    expect(screen.getByRole('button', { name: '撤回' })).not.toBeDisabled()
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
