import { describe, expect, it, vi } from 'vitest'
import { deleteTarget, localTime } from './delete-card'
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
    expect(said).toContain(localTime('2026-11-21T06:05:00.000Z'))
    expect(localTime('2026-11-21T06:05:00.000Z')).toMatch(/^2026-11-2\d \d\d:05$/)
    expect(said).toContain('task.ask.deleteSame{"n":"1"}')
  })
  it('a library not read whole leaves the same-title check unfinished, not 「none」', async () => {
    const detail = { title: '合同.pdf', created_at: '2026-11-21T06:05:00.000Z' }
    const many = api(detail, [{ document_id: DOC, title: '合同.pdf' }])
    const request = many!.request as unknown as ReturnType<typeof vi.fn>
    const first = request.getMockImplementation() as (req: { op: string }) => Promise<unknown>
    request.mockImplementation(async (req: { op: string }) => req.op === 'list'
      ? { ok: true, status: 200, data: { documents: [], next_cursor: 'abc' } } : first(req))
    const said = await deleteTarget(many, DOC, t)
    expect(said).toContain('task.ask.deleteSameUnknown')
    expect(said).toContain('task.ask.deletePagesUnknown')
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
