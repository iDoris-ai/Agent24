// @vitest-environment jsdom
import { describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen } from '@testing-library/react'
import { setLocale } from '../../i18n'
import DocumentCompare from './DocumentCompare'
import * as focus from './doc-focus'

const DOC = 'doc_01M679KMY6D75P0EZACBS95JYA'
const para = (n: number, text: string) => ({ block_ref: { paragraph_index: n }, text })
const comparison = (over: object = {}) => ({
  left: { document_id: DOC, revision: 1, media_type: 'docx' }, right: { document_id: DOC, revision: 2, media_type: 'docx' },
  identical: false, only_text: true, truncated: false, next_offset: null,
  summary: { total: 3, added: 1, removed: 0, changed: 1, unaligned: 1 },
  changes: [
    { id: 'c1', kind: 'changed', left: para(1, '租金 5 万元'), right: para(1, '租金 8 万元'), spans: [{ op: 'equal', text: '租金 ' }, { op: 'delete', text: '5' }, { op: 'insert', text: '8' }, { op: 'equal', text: ' 万元' }] },
    { id: 'c2', kind: 'added', left: null, right: para(4, '另见附件'), spans: [] },
    { id: 'c3', kind: 'unaligned', left: para(7, '违约责任'), right: null, spans: [] },
  ],
  ...over,
})
const target = (revision: number) => ({ documentId: DOC, revision, label: `第 ${revision} 版` })

describe('L6-S05: the comparison view', () => {
  it('says how many changes of each kind, that only text is compared, and marks the words', async () => {
    setLocale('zh')
    const api = { request: vi.fn(async () => ({ ok: true, data: comparison() })) }
    render(<DocumentCompare api={api as never} left={target(1)} right={target(2)} onClose={() => {}} />)
    await screen.findByText('共 3 处改动：删 0 处、加 1 处、改 1 处、无法对齐 1 处 · 只比较文字')
    expect(api.request).toHaveBeenCalledWith({ op: 'compare', left: { documentId: DOC, revision: 1 }, right: { documentId: DOC, revision: 2 } })
    const li = document.getElementById('compare-c1')!
    expect(li.querySelector('del')!.textContent).toBe('5')
    expect(li.querySelector('ins')!.textContent).toBe('8')
    expect(document.getElementById('compare-c2')!.querySelector('ins')!.textContent).toBe('另见附件')
    expect(document.getElementById('compare-c3')!.textContent).toContain('无法对齐')
  })

  it('says there is no difference for identical text', async () => {
    setLocale('zh')
    const api = { request: vi.fn(async () => ({ ok: true, data: comparison({ identical: true, changes: [], summary: { total: 0, added: 0, removed: 0, changed: 0, unaligned: 0 } }) })) }
    render(<DocumentCompare api={api as never} left={target(1)} right={target(1)} onClose={() => {}} />)
    await screen.findByText('没有文字差异 · 只比较文字')
  })

  it('reads the next page of changes from next_offset', async () => {
    setLocale('zh')
    const api = { request: vi.fn(async (r: { offset?: number }) => ({ ok: true, data: r.offset
      ? comparison({ changes: [{ id: 'c4', kind: 'added', left: null, right: para(9, '后一页'), spans: [] }] })
      : comparison({ truncated: true, next_offset: 3 }) })) }
    render(<DocumentCompare api={api as never} left={target(1)} right={target(2)} onClose={() => {}} />)
    fireEvent.click(await screen.findByRole('button', { name: '再看后面的改动' }))
    await screen.findByText('后一页')
    expect(api.request).toHaveBeenLastCalledWith({ op: 'compare', left: { documentId: DOC, revision: 1 }, right: { documentId: DOC, revision: 2 }, offset: 3 })
    expect(screen.getByText('另见附件')).toBeInTheDocument()
  })

  it('jumps a PDF change to its page, and shows a Word change as its paragraph', async () => {
    setLocale('zh')
    const spy = vi.spyOn(focus, 'focusDocument')
    const block = { block_ref: { block_id: 'p2/b3', page: 2 }, text: '押金' }
    const api = { request: vi.fn(async () => ({ ok: true, data: comparison({ changes: [{ id: 'c1', kind: 'removed', left: block, right: null, spans: [] }, { id: 'c2', kind: 'added', left: null, right: para(4, '另见附件'), spans: [] }] }) })) }
    render(<DocumentCompare api={api as never} left={target(1)} right={target(2)} onClose={() => {}} />)
    fireEvent.click(await screen.findByRole('button', { name: '原文：第 2 页' }))
    expect(spy).toHaveBeenCalledWith({ documentId: DOC, page: 2, blockId: 'p2/b3' })
    expect(screen.getByText('新版：第 5 段')).toBeInTheDocument()
  })

  it('explains a failure plainly', async () => {
    setLocale('zh')
    const api = { request: vi.fn(async () => ({ ok: false, error: { code: 'unsupported_format', message: 'm' } })) }
    render(<DocumentCompare api={api as never} left={target(1)} right={target(2)} onClose={() => {}} />)
    await screen.findByText(/不能互相对比/)
  })
})
