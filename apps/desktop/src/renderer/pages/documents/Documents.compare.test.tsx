// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen } from '@testing-library/react'
import DocumentsPage from './Documents'
import { resetImportStore } from './importStore'
import { setLocale } from '../../i18n'

const DOCX = 'application/vnd.openxmlformats-officedocument.wordprocessingml.document'
const row = (n: number, media: string) => ({
  document_id: `doc_01K74Z3QJ8V5N2W9RTX6YB4M${String(n).padStart(2, '0')}`, title: `合同 ${n}`, media_type: media,
  head_revision: 1, created_at: '2026-10-08T09:00:00.000Z', updated_at: '2026-10-08T09:00:00.000Z',
})
const docs = [row(1, DOCX), row(2, DOCX), row(3, 'application/pdf'), row(4, 'application/pdf')]
const comparison = {
  left: { document_id: docs[0]!.document_id, revision: 1, media_type: DOCX }, right: { document_id: docs[1]!.document_id, revision: 1, media_type: DOCX },
  identical: false, only_text: true, truncated: false, next_offset: null,
  summary: { total: 1, added: 1, removed: 0, changed: 0, unaligned: 0 },
  changes: [{ id: 'c1', kind: 'added', left: null, right: { block_ref: { paragraph_index: 2 }, text: '新增条款' }, spans: [] }],
}

function mount() {
  const request = vi.fn(async (req: { op: string }) => {
    if (req.op === 'capabilities') return { ok: true, status: 200, data: { storage: { state: 'ready' } } }
    if (req.op === 'list') return { ok: true, status: 200, data: { documents: docs, next_cursor: null } }
    if (req.op === 'compare') return { ok: true, status: 200, data: comparison }
    return { ok: false, status: 400, error: { code: 'invalid_request', message: 'x' } }
  })
  window.agent24 = { documents: { request, importFile: vi.fn(), onImportProgress: () => () => {}, onEvent: () => () => {} } } as never
  return request
}
const tick = (title: string) => screen.findByRole('checkbox', { name: new RegExp(`「${title}」`) })

describe('L6-S05: comparing two documents from the list', () => {
  beforeEach(() => { resetImportStore(); setLocale('zh') })

  it('offers 「对比所选」 for exactly two ticked documents and shows the comparison', async () => {
    const request = mount()
    render(<DocumentsPage />)
    fireEvent.click(await tick('合同 1'))
    expect(screen.queryByRole('button', { name: '对比所选的两份' })).toBeNull()
    fireEvent.click(await tick('合同 2'))
    fireEvent.click(screen.getByRole('button', { name: '对比所选的两份' }))
    await screen.findByText('新增条款')
    expect(request).toHaveBeenCalledWith({ op: 'compare', left: { documentId: docs[0]!.document_id }, right: { documentId: docs[1]!.document_id } })
  })

  it('merges only PDFs, and does not offer a Word document for merging', async () => {
    mount()
    render(<DocumentsPage />)
    fireEvent.click(await tick('合同 1'))
    fireEvent.click(await tick('合同 3'))
    fireEvent.click(await tick('合同 4'))
    expect(screen.getByRole('button', { name: '合并所选（2 份）' })).toBeInTheDocument()
  })
})
