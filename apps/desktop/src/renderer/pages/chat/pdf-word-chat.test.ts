import { describe, expect, it, vi } from 'vitest'
import { runToWord } from './pdf-word-chat'

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
const detail = (mediaType: string) => ({
  document_id: DOC, title: '合同.pdf', head_revision: 1,
  head: { revision: 1, media_type: mediaType },
})
const noSleep = () => Promise.resolve()

function mount(opts: {
  mediaType?: string
  convert?: () => Promise<Res>
  save?: () => Promise<unknown>
}) {
  const request = vi.fn((req: { op: string }) => {
    if (req.op === 'get') return ok(detail(opts.mediaType ?? 'application/pdf'))
    if (req.op === 'convert') return (opts.convert ?? (() => ok(job('succeeded', { result: { document_id: NEW_DOC, revision: 1 } }))))()
    return err(400, 'invalid_request', 'unexpected')
  })
  const api = {
    request,
    saveDownload: vi.fn(opts.save ?? (() => Promise.resolve({ ok: true, docxName: '合同.docx' }))),
    revealDownload: vi.fn(() => Promise.resolve(true)),
  }
  return { api: api as never as Window['agent24']['documents'], request, raw: api }
}

const SUBJECT = { id: DOC, title: '合同.pdf' }

describe('L13-1 runToWord', () => {
  it('converts, saves to Downloads and says where', async () => {
    const { api, request, raw } = mount({})
    const text = await runToWord(api, SUBJECT, '把《合同.pdf》转成 Word', noSleep)
    expect(text).toBe('已把《合同.pdf》转换为 Word：下载/合同.docx；新文档也在文档库里。')
    expect(request).toHaveBeenCalledWith({ op: 'convert', documentId: DOC })
    expect(raw.saveDownload).toHaveBeenCalledWith(NEW_DOC, '合同.pdf')
  })

  it('names the .docx when the user said doc', async () => {
    const { api } = mount({})
    const text = await runToWord(api, SUBJECT, '把《合同.pdf》转成 doc', noSleep)
    expect(text).toBe('已把《合同.pdf》转换为 Word：下载/合同.docx；新文档也在文档库里。（文件格式是 .docx。）')
  })

  it('does not say .docx twice when the user said docx', async () => {
    const { api } = mount({})
    const text = await runToWord(api, SUBJECT, '把《合同.pdf》导出为 docx', noSleep)
    expect(text).toBe('已把《合同.pdf》转换为 Word：下载/合同.docx；新文档也在文档库里。')
  })

  it('refuses a document that is not a PDF, before converting it', async () => {
    const { api, request } = mount({ mediaType: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document' })
    const text = await runToWord(api, { id: DOC, title: '合同.docx' }, '把《合同.docx》转成 Word', noSleep)
    expect(text).toBe('《合同.docx》不是 PDF，只有 PDF 能转换为 Word。')
    expect(request.mock.calls.some(([r]) => r.op === 'convert')).toBe(false)
  })

  it('points to the document page when the converter is missing', async () => {
    const { api, raw } = mount({ convert: () => err(503, 'engine_unavailable', 'the PDF to Word converter is not installed', { engine: 'pdf2docx', retryable: true }) })
    const text = await runToWord(api, SUBJECT, '把《合同.pdf》转成 Word', noSleep)
    expect(text).toBe('需要先下载 Word 转换组件：打开《合同.pdf》的文档页，点「转换为 Word」后按提示下载（约 100 MB，只需一次）。')
    expect(raw.saveDownload).not.toHaveBeenCalled()
  })

  it('says a scan cannot be converted', async () => {
    const { api } = mount({ convert: () => ok(job('failed', { error: { code: 'unsupported_format', message: 'the PDF has no text layer' } })) })
    const text = await runToWord(api, SUBJECT, '把《合同.pdf》转成 Word', noSleep)
    expect(text).toBe('《合同.pdf》是扫描件（没有文字层），暂时不能转换为 Word。')
  })

  it('says why another failure happened', async () => {
    const { api } = mount({ convert: () => ok(job('failed', { error: { code: 'parse_failed', message: 'engine exited with 4' } })) })
    const text = await runToWord(api, SUBJECT, '把《合同.pdf》转成 Word', noSleep)
    expect(text).toBe('没能转换为 Word：engine exited with 4')
  })

  it('says the document is in the library when saving to Downloads fails', async () => {
    const { api } = mount({ save: () => Promise.resolve({ ok: false, code: 'export_failed', message: 'disk full' }) })
    const text = await runToWord(api, SUBJECT, '把《合同.pdf》转成 Word', noSleep)
    expect(text).toBe('已把《合同.pdf》转换为 Word，新文档在文档库里；没能存进「下载」：disk full')
  })

  it('says the document is in the library when the job names no new document', async () => {
    const { api, raw } = mount({ convert: () => ok(job('succeeded', { result: {} })) })
    const text = await runToWord(api, SUBJECT, '把《合同.pdf》转成 Word', noSleep)
    expect(text).toBe('已把《合同.pdf》转换为 Word，新文档在文档库里。')
    expect(raw.saveDownload).not.toHaveBeenCalled()
  })

  it('says so when the document cannot be read', async () => {
    const { api, raw } = mount({})
    ;(raw.request as ReturnType<typeof vi.fn>).mockImplementation(() => err(404, 'not_found', 'no such document'))
    const text = await runToWord(api, SUBJECT, '把《合同.pdf》转成 Word', noSleep)
    expect(text).toBe('没能读取《合同.pdf》，转换没有进行。')
  })
})
