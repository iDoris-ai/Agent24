import { describe, expect, it, vi } from 'vitest'
import { headOf } from './DocumentsOutcome'

describe('headOf (L6-F5)', () => {
  const docs = [
    { document_id: 'doc_A', title: '合同.docx' },
    { document_id: 'doc_B', title: '合同.pdf' },
  ]
  const api = () => ({
    request: vi.fn(async (req: { op: string; documentId?: string }) => req.op === 'list'
      ? { ok: true, data: { documents: docs } }
      : { ok: true, data: { head_revision: req.documentId === 'doc_A' ? 2 : 1 } }),
  }) as never

  it('reads the version of the exact title when its converted PDF shares the stem', async () => {
    expect(await headOf(api(), '合同.docx')).toBe(2)
  })
  it('is unread when only a stem is given and two documents share it', async () => {
    expect(await headOf(api(), '合同')).toBeNull()
  })
})
