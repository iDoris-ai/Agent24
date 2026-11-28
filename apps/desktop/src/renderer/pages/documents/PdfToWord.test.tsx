// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import PdfToWord from './PdfToWord'
import { setLocale } from '../../i18n'

const DOC = 'doc_01K74Z3QJ8V5N2W9RTX6YB4M01'
const NEW_DOC = 'doc_01K74Z3QJ8V5N2W9RTX6YB4M09'
type Res = { ok: boolean; status: number; data?: unknown; error?: unknown }
const ok = (data: unknown): Promise<Res> => Promise.resolve({ ok: true, status: 200, data })
const err = (status: number, code: string, message: string, details?: unknown): Promise<Res> =>
  Promise.resolve({ ok: false, status, error: { code, message, ...(details ? { details } : {}) } })
const job = (status: string, extra: Record<string, unknown> = {}) => ({
  job_id: 'job_01K74Z3QJ8V5N2W9RTX6YB4M01', kind: 'convert', status, attempt: 1,
  created_at: '2026-11-28T09:00:00.000Z', updated_at: '2026-11-28T09:00:00.000Z', ...extra,
})
const succeeded = () => ok(job('succeeded', { result: { document_id: NEW_DOC, revision: 1 } }))
const missing = () => err(503, 'engine_unavailable', 'the PDF to Word converter is not installed', { engine: 'pdf2docx', retryable: true })

function mount(onConvert: Array<() => Promise<Res>>, install?: () => Promise<unknown>, save?: () => Promise<unknown>) {
  const queue = [...onConvert]
  const request = vi.fn((req: { op: string }) => {
    if (req.op === 'convert') return (queue.shift() ?? (() => err(500, 'internal', 'no more')))()
    return err(400, 'invalid_request', 'unexpected')
  })
  const api = {
    request,
    installPdfWordEngine: vi.fn(install ?? (() => Promise.resolve({ state: 'ready' }))),
    pdfWordEngineStatus: vi.fn(() => Promise.resolve({ state: 'missing' })),
    onPdfWordEngineChanged: vi.fn(() => () => {}),
    saveDownload: vi.fn(save ?? (() => Promise.resolve({ ok: true, docxName: 'putuo.docx' }))),
    revealDownload: vi.fn(() => Promise.resolve(true)),
  }
  return { api: api as never as Window['agent24']['documents'], request, raw: api }
}

beforeEach(() => setLocale('zh'))

describe('PdfToWord', () => {
  it('converts the PDF and says where the Word document is', async () => {
    const onAdded = vi.fn()
    const { api, request } = mount([succeeded])
    render(<PdfToWord api={api} documentId={DOC} title="putuo.pdf" onAdded={onAdded} />)
    fireEvent.click(screen.getByRole('button', { name: '转换为 Word' }))
    expect(await screen.findByText('已转换为 Word，新文档也在文档列表里。')).toBeInTheDocument()
    expect(onAdded).toHaveBeenCalledTimes(1)
    expect(request).toHaveBeenCalledWith({ op: 'convert', documentId: DOC })
  })

  it('offers the download when the engine is missing, then converts', async () => {
    const onAdded = vi.fn()
    const { api, request, raw } = mount([missing, succeeded])
    render(<PdfToWord api={api} documentId={DOC} title="putuo.pdf" onAdded={onAdded} />)
    fireEvent.click(screen.getByRole('button', { name: '转换为 Word' }))
    expect(await screen.findByText('需要先下载 Word 转换组件（约 100 MB，只需一次）')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: '下载' }))
    expect(await screen.findByText('已转换为 Word，新文档也在文档列表里。')).toBeInTheDocument()
    expect(raw.installPdfWordEngine).toHaveBeenCalledTimes(1)
    expect(request.mock.calls.filter(([r]) => r.op === 'convert')).toHaveLength(2)
    expect(onAdded).toHaveBeenCalledTimes(1)
  })

  it('says why when the download fails', async () => {
    const { api } = mount([missing], () => Promise.resolve({ state: 'failed', error: 'HTTP 404' }))
    render(<PdfToWord api={api} documentId={DOC} title="putuo.pdf" />)
    fireEvent.click(screen.getByRole('button', { name: '转换为 Word' }))
    fireEvent.click(await screen.findByRole('button', { name: '下载' }))
    expect(await screen.findByText('下载没有完成：HTTP 404')).toBeInTheDocument()
  })

  it('tells a scan apart from other failures', async () => {
    const { api } = mount([
      () => ok(job('failed', { error: { code: 'unsupported_format', message: 'the PDF has no text layer (a scanned document): it cannot be converted to Word' } })),
      () => ok(job('failed', { error: { code: 'parse_failed', message: 'engine exited with 4' } })),
    ])
    render(<PdfToWord api={api} documentId={DOC} title="putuo.pdf" />)
    fireEvent.click(screen.getByRole('button', { name: '转换为 Word' }))
    expect(await screen.findByText('这是扫描件（没有文字层），暂时不能转换为 Word。')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: '转换为 Word' }))
    expect(await screen.findByText('转换没有成功：engine exited with 4')).toBeInTheDocument()
  })

  it('does not take the Office converter missing for its own', async () => {
    const { api, raw } = mount([() => err(503, 'engine_unavailable', 'the Office converter is not installed', { engine: 'libreoffice', retryable: true })])
    render(<PdfToWord api={api} documentId={DOC} title="putuo.pdf" />)
    fireEvent.click(screen.getByRole('button', { name: '转换为 Word' }))
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('转换没有成功'))
    expect(screen.queryByRole('button', { name: '下载' })).not.toBeInTheDocument()
    expect(raw.installPdfWordEngine).not.toHaveBeenCalled()
  })

  it('keeps the button off while converting', async () => {
    let finish: (r: Res) => void = () => {}
    const { api } = mount([() => new Promise<Res>((resolve) => { finish = resolve })])
    render(<PdfToWord api={api} documentId={DOC} title="putuo.pdf" />)
    fireEvent.click(screen.getByRole('button', { name: '转换为 Word' }))
    expect(await screen.findByText('正在转换为 Word…')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: '转换为 Word' })).toBeDisabled()
    finish({ ok: true, status: 202, data: job('succeeded', { result: { document_id: NEW_DOC, revision: 1 } }) })
    await waitFor(() => expect(screen.getByRole('button', { name: '转换为 Word' })).not.toBeDisabled())
  })

  it('saves the Word document to Downloads and shows it in Finder', async () => {
    const { api, raw } = mount([succeeded])
    render(<PdfToWord api={api} documentId={DOC} title="putuo.pdf" />)
    fireEvent.click(screen.getByRole('button', { name: '转换为 Word' }))
    fireEvent.click(await screen.findByRole('button', { name: '保存到下载' }))
    expect(await screen.findByText('已保存：下载/putuo.docx')).toBeInTheDocument()
    expect(raw.saveDownload).toHaveBeenCalledWith(NEW_DOC, 'putuo.pdf')
    expect(raw.revealDownload).toHaveBeenCalledWith('putuo.docx')
  })

  it('says why when saving fails', async () => {
    const { api, raw } = mount([succeeded], undefined, () => Promise.resolve({ ok: false, code: 'export_failed', message: 'disk full' }))
    render(<PdfToWord api={api} documentId={DOC} title="putuo.pdf" />)
    fireEvent.click(screen.getByRole('button', { name: '转换为 Word' }))
    fireEvent.click(await screen.findByRole('button', { name: '保存到下载' }))
    expect(await screen.findByText('没能保存：disk full')).toBeInTheDocument()
    expect(raw.revealDownload).not.toHaveBeenCalled()
  })

  it('offers no save when the job names no new document', async () => {
    const { api } = mount([() => ok(job('succeeded', { result: {} }))])
    render(<PdfToWord api={api} documentId={DOC} title="putuo.pdf" />)
    fireEvent.click(screen.getByRole('button', { name: '转换为 Word' }))
    expect(await screen.findByText('已转换为 Word，新文档也在文档列表里。')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: '保存到下载' })).not.toBeInTheDocument()
  })
})
