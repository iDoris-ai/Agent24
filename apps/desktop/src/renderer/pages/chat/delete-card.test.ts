import { describe, expect, it, vi } from 'vitest'
import { deleteTarget } from './delete-card'
import { approvalText } from './TaskCard'

const DOC = 'doc_01M8035XWKZJGGD3NHF3X0HJ0H'
const t = (key: string, vars: Record<string, string> = {}) => `${key}${JSON.stringify(vars)}`

function api(detail: unknown, docs: Array<{ document_id: string; title: string }>) {
  return {
    request: vi.fn(async (req: { op: string }) =>
      req.op === 'get'
        ? (detail ? { ok: true, status: 200, data: detail } : { ok: false, status: 404, error: { code: 'not_found', message: 'gone' } })
        : { ok: true, status: 200, data: { documents: docs, next_cursor: null } }),
  } as unknown as Window['agent24']['documents']
}

describe('L6-F2: the delete card names one document from the library', () => {
  it('title, pages, import time, and the others with its title', async () => {
    const docs = [{ document_id: DOC, title: '中文测试合同.pdf' }, { document_id: 'doc_01M8035XWKZJGGD3NHF3X0HJ0J', title: '中文测试合同.PDF' }]
    const said = await deleteTarget(api({ title: '中文测试合同.pdf', created_at: '2026-11-21T06:05:00.000Z', pages: 3 }, docs), DOC, t)
    expect(said).toContain('task.ask.deleteTarget{"title":"中文测试合同.pdf","pages":"task.ask.deletePages{\\"n\\":\\"3\\"}"')
    expect(said).toContain(new Date('2026-11-21T06:05:00.000Z').toLocaleString())
    expect(said).toContain('task.ask.deleteSame{"n":"1"}')
  })
  it('a document it cannot read is said so', async () => {
    expect(await deleteTarget(api(null, []), DOC, t)).toBe('task.ask.deleteUnread{}')
  })
  it('the card text is the library\'s, by id', () => {
    const titles = new Map([[DOC, '《合同》（共 3 页，导入于 …）']])
    const text = approvalText({ summary: `documents.delete: {"document_id":"${DOC}"}`, payload: { document_id: DOC } }, t, null, titles)
    expect(text).toBe('task.ask.delete{"doc":"《合同》（共 3 页，导入于 …）"}')
  })
})
