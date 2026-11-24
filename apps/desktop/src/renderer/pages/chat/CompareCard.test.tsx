// @vitest-environment jsdom
import { describe, expect, it, vi } from 'vitest'
import { act, fireEvent, render, screen } from '@testing-library/react'
import { CompareCard } from './CompareCard'
import ComparePickup from '../documents/ComparePickup'
import { clearCompareFocus, pendingCompareFocus } from '../documents/doc-focus'
import { setLocale } from '../../i18n'
import { CapabilitiesContext } from '../../capabilities/types'

const DOC = 'doc_01M679KMY6D75P0EZACBS95JYA'
const card = {
  left: { documentId: DOC, revision: 1, label: '第 1 版' }, right: { documentId: DOC, revision: 2, label: '第 2 版' },
  lines: [{ id: 'c1', text: 'c1 第 2 段 修改：租金〔5→8〕万元' }, { id: 'c2', text: 'c2 第 5 段 新增：「附件」' }],
}

describe('L6-S05: the comparison card in the conversation', () => {
  it('opens the documents page at the change that was clicked, and at the start from the button', () => {
    clearCompareFocus()
    const open = vi.fn()
    render(<CapabilitiesContext.Provider value={{ defs: [], open }}><CompareCard card={card} /></CapabilitiesContext.Provider>)
    fireEvent.click(screen.getByRole('button', { name: /c2 第 5 段/ }))
    expect(pendingCompareFocus()).toEqual({ left: card.left, right: card.right, focusId: 'c2' })
    expect(open).toHaveBeenCalledWith('documents', 'other')
    fireEvent.click(screen.getByRole('button', { name: '打开对比视图' }))
    expect(pendingCompareFocus()?.focusId).toBeUndefined()
  })
})

describe('L6-S05: the documents page takes a comparison from the conversation', () => {
  it('shows the comparison view for it, then lets it go', async () => {
    setLocale('zh')
    const api = { request: vi.fn(async () => ({ ok: true, data: { left: { document_id: DOC, revision: 1, media_type: 'd' }, right: { document_id: DOC, revision: 2, media_type: 'd' }, identical: true, only_text: true, truncated: false, next_offset: null, summary: { total: 0, added: 0, removed: 0, changed: 0, unaligned: 0 }, changes: [] } })) }
    clearCompareFocus()
    render(<ComparePickup api={api as never} />)
    expect(screen.queryByRole('region')).toBeNull()
    const { openCompare } = await import('../documents/doc-focus')
    act(() => openCompare({ left: card.left, right: card.right }))
    await screen.findByText('没有文字差异 · 只比较文字')
    expect(pendingCompareFocus()).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: '关闭对比' }))
    expect(screen.queryByText('没有文字差异 · 只比较文字')).toBeNull()
  })
})
