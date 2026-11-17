import { describe, expect, it, vi } from 'vitest'
import { runDocIntent } from './doc-intent-run'

const doc = { id: 'doc_A', title: '保单.pdf', revision: 1 }
const job = (status: string, extra: object = {}) => ({ job_id: 'job_1', kind: 'import', status, attempt: 1, created_at: '', updated_at: '', ...extra })

function api(docs: Array<{ document_id: string; title: string; media_type: string }>) {
  const request = vi.fn(async (req: { op: string; parts?: unknown }) => {
    if (req.op === 'list') return { ok: true, status: 200, data: { documents: docs, next_cursor: null } }
    if (req.op === 'compose') return { ok: true, status: 202, data: job('succeeded', { result: { document_id: 'doc_N' } }) }
    return { ok: false, status: 400, error: { code: 'invalid_request', message: 'x' } }
  })
  return { request } as unknown as Window['agent24']['documents'] & { request: typeof request }
}

describe('D2b intents carried out', () => {
  it('splits pages of the conversation\'s document', async () => {
    const a = api([])
    expect(await runDocIntent(a, doc, { kind: 'split', pages: '2-4' })).toContain('《保单.pdf（第 2-4 页）》')
    expect(a.request).toHaveBeenCalledWith(expect.objectContaining({ op: 'compose', parts: [{ documentId: 'doc_A', pages: '2-4' }] }))
  })

  it('merges with the one document of that title, and says so when there is none', async () => {
    const a = api([{ document_id: 'doc_B', title: '附件.pdf', media_type: 'application/pdf' }, { document_id: 'doc_A', title: '保单.pdf', media_type: 'application/pdf' }])
    expect(await runDocIntent(a, doc, { kind: 'merge', other: '附件' })).toContain('合并：保单.pdf + 附件.pdf')
    expect(a.request).toHaveBeenCalledWith(expect.objectContaining({ op: 'compose', parts: [{ documentId: 'doc_A' }, { documentId: 'doc_B' }] }))
    expect(await runDocIntent(a, doc, { kind: 'merge', other: '没有这份' })).toContain('没有找到')
  })
})
