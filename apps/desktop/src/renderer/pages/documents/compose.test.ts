import { describe, expect, it, vi } from 'vitest'
import { convertedPdf, convertedTitle } from './compose'

describe('L6-F5: the PDF an Office document is read from is its current version\'s', () => {
  it('names version 2 on apart, version 1 as before', () => {
    expect(convertedTitle('合同.docx')).toBe('合同（转换）.pdf')
    expect(convertedTitle('合同.docx', 1)).toBe('合同（转换）.pdf')
    expect(convertedTitle('合同.docx', 2)).toBe('合同（第 2 版，转换）.pdf')
  })

  it("does not take version 1's PDF for version 2", async () => {
    const request = vi.fn(async (req: { op: string }) => {
      if (req.op === 'get') return { ok: true, data: { head_revision: 2 } }
      if (req.op === 'list') return { ok: true, data: { documents: [
        { document_id: 'doc_v1pdf', title: '合同（转换）.pdf', media_type: 'application/pdf', created_at: '2026-11-22T00:00:00Z' },
      ] } }
      if (req.op === 'convert') return { ok: true, data: { job_id: 'job_1', status: 'succeeded', result: { document_id: 'doc_v2pdf' } } }
      return { ok: false, error: { code: 'x', message: 'x' } }
    })
    const out = await convertedPdf({ request } as unknown as Window['agent24']['documents'], 'doc_docx', '合同.docx')
    expect(out).toEqual({ ok: true, documentId: 'doc_v2pdf' })
    expect(request).toHaveBeenCalledWith({ op: 'convert', documentId: 'doc_docx', title: '合同（第 2 版，转换）.pdf' })
  })
})

describe('L12-3: convert says which engine is missing and why a job failed', () => {
  it('marks either converter missing, with its name', async () => {
    for (const engine of ['libreoffice', 'pdf2docx']) {
      const api = { request: vi.fn(() => Promise.resolve({ ok: false, status: 503, error: { code: 'engine_unavailable', message: 'not installed', details: { engine, retryable: true } } })) }
      const { convert } = await import('./compose')
      expect(await convert(api as never, 'doc_01K74Z3QJ8V5N2W9RTX6YB4M01')).toMatchObject({ ok: false, engineMissing: true, engine })
    }
  })

  it('passes the failed job\'s code on', async () => {
    const api = { request: vi.fn(() => Promise.resolve({ ok: true, status: 202, data: {
      job_id: 'job_01K74Z3QJ8V5N2W9RTX6YB4M01', kind: 'convert', status: 'failed', attempt: 1,
      error: { code: 'unsupported_format', message: 'a scan' },
      created_at: '2026-11-28T09:00:00.000Z', updated_at: '2026-11-28T09:00:00.000Z',
    } })) }
    const { convert } = await import('./compose')
    expect(await convert(api as never, 'doc_01K74Z3QJ8V5N2W9RTX6YB4M01')).toEqual({ ok: false, message: 'a scan', code: 'unsupported_format' })
  })
})
