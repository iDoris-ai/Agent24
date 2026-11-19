// @vitest-environment jsdom
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import DocxTools, { filledValues, madeTitle } from './DocxTools'

type Api = Window['agent24']['documents']

function api(over: Partial<Record<string, unknown>> = {}) {
  const request = vi.fn(async (req: { op: string }) => {
    if (req.op === 'fields') return { ok: true, data: { document_id: 'doc_t', revision: 1, fields: [{ name: '甲方', count: 1 }, { name: '租金', count: 2 }] } }
    if (req.op === 'paragraphs') return { ok: true, data: { document_id: 'doc_t', revision: 1, paragraphs: [{ index: 0, text: '', in_table: false }, { index: 1, text: '第一条 旧', in_table: false }] } }
    if (req.op === 'fill' || req.op === 'patch') return { ok: true, data: { job_id: 'job_1', status: 'succeeded', result: { document_id: 'doc_new' } } }
    return { ok: false, error: { code: 'x', message: 'x' } }
  })
  return {
    request,
    saveDownload: vi.fn(async () => ({ ok: true, docxName: '模板（填写）.docx' })),
    revealDownload: vi.fn(async () => true),
    ...over,
  } as unknown as Api & { request: typeof request }
}

describe('DocxTools (E1b/D5a)', () => {
  it('fills only the fields given, makes a new document and saves it to Downloads', async () => {
    const a = api()
    const onAdded = vi.fn()
    render(<DocxTools api={a} documentId="doc_t" title="模板.docx" onAdded={onAdded} />)
    fireEvent.click(screen.getByText('Fill in template…'))
    fireEvent.change(await screen.findByLabelText('甲方'), { target: { value: '上海明远' } })
    fireEvent.click(screen.getByText('Make document'))
    await screen.findByText('Made 「模板（填写）」 in the document library.')
    expect(a.request).toHaveBeenCalledWith({ op: 'fill', documentId: 'doc_t', values: { 甲方: '上海明远' } })
    expect(onAdded).toHaveBeenCalled()
    fireEvent.click(screen.getByText('Save to Downloads'))
    await screen.findByText('Saved: Downloads/模板（填写）.docx')
    expect(a.saveDownload).toHaveBeenCalledWith('doc_new', '模板（填写）.docx')
  })

  it('rewrites a paragraph as a tracked change, the old text as expect', async () => {
    const a = api()
    render(<DocxTools api={a} documentId="doc_t" title="合同.docx" />)
    fireEvent.click(screen.getByText('Rewrite paragraphs…'))
    const box = await screen.findByRole('textbox', { name: 'Rewrite paragraphs' })
    expect((box as HTMLTextAreaElement).value).toBe('第一条 旧')
    fireEvent.change(box, { target: { value: '第一条 新' } })
    fireEvent.click(screen.getByText('Make revised document'))
    await waitFor(() => expect(a.request).toHaveBeenCalledWith({
      op: 'patch', documentId: 'doc_t', edits: [{ index: 1, text: '第一条 新', tracked: true, expect: '第一条 旧' }],
    }))
  })

  it('names what it makes as the module does, and drops empty values', () => {
    expect(madeTitle('合同.docx', '（修订）')).toBe('合同（修订）')
    expect(filledValues({ a: 'x', b: '' })).toEqual({ a: 'x' })
  })
})
