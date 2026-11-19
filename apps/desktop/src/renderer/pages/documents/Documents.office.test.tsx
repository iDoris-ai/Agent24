// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import DocumentsPage from './Documents'
import { resetImportStore } from './importStore'
import { setLocale } from '../../i18n'

const DOCX = 'application/vnd.openxmlformats-officedocument.wordprocessingml.document'
const XLSX = 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet'
const PPTX = 'application/vnd.openxmlformats-officedocument.presentationml.presentation'

const doc = (n: number, media: string) => ({
  document_id: `doc_01K74Z3QJ8V5N2W9RTX6YB4M${String(n).padStart(2, '0')}`,
  title: `通告 ${n}`,
  media_type: media,
  head_revision: 1,
  created_at: '2026-10-08T09:00:00.000Z',
  updated_at: '2026-10-08T09:00:00.000Z',
})
const detail = (n: number, media: string) => ({
  ...doc(n, media),
  head: { revision: 2, content_sha256: `sha256:${'a'.repeat(64)}`, size: 2048, media_type: media },
})
type Res = { ok: boolean; status: number; data?: unknown; error?: unknown }
const ok = (data: unknown): Promise<Res> => Promise.resolve({ ok: true, status: 200, data })
const err = (status: number, code: string, message: string, details?: unknown): Promise<Res> =>
  Promise.resolve({ ok: false, status, error: { code, message, ...(details ? { details } : {}) } })

function mount(docs: Array<ReturnType<typeof doc>>, onGet: (id: string) => Promise<Res>, onConvert?: () => Promise<Res>) {
  const request = vi.fn((req: { op: string; documentId?: string }) => {
    if (req.op === 'capabilities') return ok({ storage: { state: 'ready' } })
    if (req.op === 'list') return ok({ documents: docs, next_cursor: null })
    if (req.op === 'get') return onGet(req.documentId ?? '')
    if (req.op === 'convert') return (onConvert ?? (() => err(500, 'internal', 'no convert')))()
    return err(400, 'invalid_request', 'unexpected')
  })
  window.agent24 = {
    documents: {
      request,
      importFile: vi.fn(() => Promise.resolve(null)),
      onImportProgress: () => () => {},
      onEvent: () => () => {},
      installOfficeEngine: vi.fn(),
      onOfficeEngineChanged: () => () => {},
    },
  } as never
  return request
}

beforeEach(() => {
  resetImportStore()
  setLocale('zh')
})

describe('DocumentsPage: Office documents', () => {
  it('shows Convert to PDF and does not render a page', async () => {
    const request = mount(
      [doc(1, DOCX), doc(2, XLSX), doc(3, PPTX)],
      (id) => ok(detail(id.endsWith('01') ? 1 : 2, id.endsWith('01') ? DOCX : XLSX)),
    )
    render(<DocumentsPage />)
    expect(await screen.findByText('DOCX')).toBeInTheDocument()
    expect(screen.getByText('XLSX')).toBeInTheDocument()
    expect(screen.getByText('PPTX')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: '通告 1' }))
    expect(await screen.findByRole('button', { name: '转换为 PDF' })).toBeInTheDocument()
    expect(screen.getByText('Word/Excel/PowerPoint 文档：先转换为 PDF 才能预览和提问。')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: '在对话里问' })).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: '拆分…' })).not.toBeInTheDocument()
    await waitFor(() => expect(request.mock.calls.some(([r]) => r.op === 'get')).toBe(true))
    expect(request.mock.calls.filter(([r]) => r.op === 'renderPage')).toHaveLength(0)
    expect(request.mock.calls.filter(([r]) => r.op === 'readText')).toHaveLength(0)
  })

  it('offers the download when convert says LibreOffice is missing', async () => {
    const request = mount(
      [doc(1, DOCX)],
      () => ok(detail(1, DOCX)),
      () => err(503, 'engine_unavailable', 'LibreOffice is not installed', { engine: 'libreoffice', retryable: false }),
    )
    render(<DocumentsPage />)
    fireEvent.click(await screen.findByRole('button', { name: '通告 1' }))
    fireEvent.click(await screen.findByRole('button', { name: '转换为 PDF' }))
    expect(await screen.findByRole('button', { name: '下载' })).toBeInTheDocument()
    expect(screen.getByText('需要先下载转换组件（约 340 MB，只需一次）')).toBeInTheDocument()
    expect(request.mock.calls.filter(([r]) => r.op === 'renderPage')).toHaveLength(0)
    expect(request.mock.calls.some(([r]) => r.op === 'convert' && r.documentId === doc(1, DOCX).document_id)).toBe(true)
  })

  // General 11-19 (0.6.37): 「完成」 said the new document was in the list, which had not been read again.
  it('reads the list again once a conversion is done', async () => {
    const job = { job_id: 'job_01K74Z3QJ8V5N2W9RTX6YB4M01', kind: 'convert', status: 'succeeded', attempt: 1,
      result: { document_id: doc(9, 'application/pdf').document_id, revision: 1 },
      created_at: '2026-10-08T09:00:00.000Z', updated_at: '2026-10-08T09:00:00.000Z' }
    const request = mount([doc(1, DOCX)], () => ok(detail(1, DOCX)), () => ok(job))
    render(<DocumentsPage />)
    fireEvent.click(await screen.findByRole('button', { name: '通告 1' }))
    await waitFor(() => expect(request.mock.calls.filter(([r]) => r.op === 'list').length).toBeGreaterThan(0))
    const before = request.mock.calls.filter(([r]) => r.op === 'list').length
    fireEvent.click(await screen.findByRole('button', { name: '转换为 PDF' }))
    await waitFor(() => expect(request.mock.calls.filter(([r]) => r.op === 'list').length).toBeGreaterThan(before))
  })
})
