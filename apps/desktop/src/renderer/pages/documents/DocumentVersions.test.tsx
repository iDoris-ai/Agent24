// @vitest-environment jsdom
import { describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { setLocale } from '../../i18n'
import DocumentVersions, { versionFileTitle } from './DocumentVersions'

const DOC = 'doc_01M679KMY6D75P0EZACBS95JYA'
const list = (head: number) => ({ ok: true, data: { document_id: DOC, head_revision: head, revisions: Array.from({ length: head }, (_, i) => head - i).map((n) => ({
  revision: n, content_sha256: 'sha256:x', size: 1, media_type: 'docx', origin: n === 1 ? 'import' : 'commit', created_at: '2026-11-23T02:00:00.000Z',
})) } })

function api(restored: unknown) {
  let head = 2
  return {
    request: vi.fn(async (req: { op: string }) => {
      if (req.op === 'revisions') return list(head)
      if (req.op === 'restoreRevision') { const r = restored as { ok: boolean; data?: { revision: number } }; if (r.ok) head = r.data!.revision; return r }
      return { ok: false, error: { code: 'x', message: 'x' } }
    }),
    saveDownload: vi.fn(async () => ({ ok: true, docxName: '合同（第 1 版）.docx' })),
    revealDownload: vi.fn(async () => true),
  }
}

describe('L6-F5: a document\'s versions on its page', () => {
  it('lists them, the current one marked, and restores an old one over the version shown', async () => {
    setLocale('zh')
    const a = api({ ok: true, data: { document_id: DOC, title: '合同.docx', revision: 3, previous_revision: 2, committed: true } })
    const changed = vi.fn()
    render(<DocumentVersions api={a as never} documentId={DOC} title="合同.docx" onChanged={changed} />)
    const v1 = await screen.findByText('第 1 版')
    const row1 = v1.closest('li')!
    expect(within(row1).getByText(/原件/)).toBeInTheDocument()
    const row2 = screen.getByText('第 2 版').closest('li')!
    expect(within(row2).getByText('当前')).toBeInTheDocument()
    expect(within(row2).queryByRole('button', { name: '恢复这个版本' })).toBeNull()
    fireEvent.click(within(row1).getByRole('button', { name: '恢复这个版本' }))
    await screen.findByText('已恢复：第 1 版的内容现在是第 3 版（当前版本），所有版本都保留。')
    expect(a.request).toHaveBeenCalledWith({ op: 'restoreRevision', documentId: DOC, revision: 1, expectRevision: 2 })
    expect(changed).toHaveBeenCalled()
    await waitFor(() => expect(screen.getByText('第 3 版')).toBeInTheDocument())
  })

  it('says nothing was restored when the document moved on', async () => {
    setLocale('zh')
    const a = api({ ok: false, status: 409, error: { code: 'revision_conflict', message: 'm' } })
    render(<DocumentVersions api={a as never} documentId={DOC} title="合同.docx" />)
    const row1 = (await screen.findByText('第 1 版')).closest('li')!
    fireEvent.click(within(row1).getByRole('button', { name: '恢复这个版本' }))
    await screen.findByText(/这次没有恢复/)
  })

  it('opens a version as its own file in Downloads', async () => {
    setLocale('zh')
    const a = api(null)
    render(<DocumentVersions api={a as never} documentId={DOC} title="合同.docx" />)
    const row1 = (await screen.findByText('第 1 版')).closest('li')!
    fireEvent.click(within(row1).getByRole('button', { name: '打开' }))
    await screen.findByText('已保存到：下载/合同（第 1 版）.docx')
    expect(a.saveDownload).toHaveBeenCalledWith(DOC, '合同（第 1 版）', 1)
    expect(a.revealDownload).toHaveBeenCalledWith('合同（第 1 版）.docx')
    expect(versionFileTitle('报告.pdf', 2)).toBe('报告（第 2 版）')
  })

  it('compares two picked versions, older on the left', async () => {
    setLocale('zh')
    const a = { request: vi.fn(async (req: { op: string }): Promise<unknown> => req.op === 'compare'
      ? { ok: true, data: { left: { document_id: DOC, revision: 1, media_type: 'd' }, right: { document_id: DOC, revision: 2, media_type: 'd' }, identical: true, only_text: true, truncated: false, next_offset: null, summary: { total: 0, added: 0, removed: 0, changed: 0, unaligned: 0 }, changes: [] } }
      : list(2)) }
    render(<DocumentVersions api={a as never} documentId={DOC} title="合同.docx" />)
    const compareBtn = await screen.findByRole('button', { name: '对比选中的两版' })
    expect(compareBtn).toBeDisabled()
    fireEvent.click(screen.getByRole('checkbox', { name: '选中第 2 版' }))
    fireEvent.click(screen.getByRole('checkbox', { name: '选中第 1 版' }))
    fireEvent.click(compareBtn)
    await screen.findByText('没有文字差异 · 只比较文字')
    expect(a.request).toHaveBeenCalledWith({ op: 'compare', left: { documentId: DOC, revision: 1 }, right: { documentId: DOC, revision: 2 } })
  })
})
