import { describe, expect, it, vi } from 'vitest'
import { extractFields, extractText } from './doc-extract'

const anchor = (block_id: string, page: number, quote: string) => ({ block_id, page, quote })
const job = (status: string, extraction?: string) => ({ job_id: 'job_1', kind: 'extract', status, ...(extraction ? { result: { extraction_id: extraction } } : {}) })

function fakeApi(media: string, values: unknown[], first = 'running') {
  const calls: Array<Record<string, unknown>> = []
  const request = vi.fn(async (req: Record<string, unknown>) => {
    calls.push(req)
    switch (req.op) {
      case 'get': return { ok: true, data: { document_id: 'doc_a', title: media === 'application/pdf' ? '合同.pdf' : '合同.docx', head: { revision: 2, media_type: media } } }
      case 'list': return { ok: true, data: { documents: [{ document_id: 'doc_pdf', title: '合同（转换）.pdf', media_type: 'application/pdf', created_at: '2026-11-22' }], next_cursor: null } }
      case 'extract': return { ok: true, data: job(first) }
      case 'job': return { ok: true, data: job('succeeded', 'ext_1') }
      case 'getExtraction': return { ok: true, data: { values, next_cursor: null } }
      default: throw new Error(String(req.op))
    }
  })
  return { api: { request } as unknown as Window['agent24']['documents'], calls }
}

describe('F3 extractFields', () => {
  it('extracts with the OS job and keeps every value with its source; a missing field says so', async () => {
    const { api, calls } = fakeApi('application/pdf', [
      { key: 'f1', status: 'present', value: '张三', anchors: [anchor('p1/b2', 1, '甲方：张三')] },
      { key: 'f2', status: 'missing', missing_reason: 'not_in_document' },
      { key: 'f3', status: 'conflict', candidates: [
        { value: '5000 元', normalized: '5000', anchors: [anchor('p2/b1', 2, '5000 元')] },
        { value: '6000 元', normalized: '6000', anchors: [anchor('p3/b4', 3, '6000 元')] },
      ] },
    ])
    const card = await extractFields(api, 'doc_a', ['甲方', '开户银行', '金额'], async () => {})
    expect(calls.find((c) => c.op === 'extract')).toEqual({
      op: 'extract', documentId: 'doc_a', revision: 2,
      schema: { fields: [
        { key: 'f1', description: '甲方', type: 'text' },
        { key: 'f2', description: '开户银行', type: 'text' },
        { key: 'f3', description: '金额', type: 'amount' },
      ] },
    })
    expect(card).toEqual({ documentId: 'doc_a', title: '合同.pdf', rows: [
      { name: '甲方', values: ['张三'], sources: [anchor('p1/b2', 1, '甲方：张三')] },
      { name: '开户银行', values: [], sources: [] },
      { name: '金额', values: ['5000 元', '6000 元'], sources: [anchor('p2/b1', 2, '5000 元'), anchor('p3/b4', 3, '6000 元')] },
    ] })
    if ('error' in card) throw new Error('no card')
    expect(extractText(card)).toBe('从《合同.pdf》抽取：\n- 甲方：张三（第 1 页）\n- 开户银行：文档里没找到\n- 金额：5000 元 / 6000 元（文档里写法不一致）（第 2、3 页）')
  })

  it('extracts a Word document as the PDF it was converted to', async () => {
    const { api, calls } = fakeApi('application/vnd.openxmlformats-officedocument.wordprocessingml.document', [])
    const card = await extractFields(api, 'doc_a', ['甲方'], async () => {})
    expect(calls.find((c) => c.op === 'extract')).toMatchObject({ documentId: 'doc_pdf', revision: 1 })
    expect(card).toMatchObject({ documentId: 'doc_pdf', rows: [{ name: '甲方', values: [] }] })
  })

  it('runs again a replayed job that had failed', async () => {
    const { api, calls } = fakeApi('application/pdf', [], 'failed')
    await extractFields(api, 'doc_a', ['甲方'], async () => {})
    const submits = calls.filter((c) => c.op === 'extract')
    expect(submits).toHaveLength(2)
    expect(submits[1]).toMatchObject({ rerun: true })
  })
})
