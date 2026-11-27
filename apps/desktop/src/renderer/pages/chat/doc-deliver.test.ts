import { describe, expect, it, vi } from 'vitest'
import { asksDeliver, preflightText, receiptsText, runPreflight, runReceipts } from './doc-deliver'
import type { DeliveryReceipt, DocumentPreflight } from '../../../shared/ipc-types'

const DOC = 'doc_01M679KMY6D75P0EZACBS95JYA'

describe('L8 DOC-4: what a turn asks', () => {
  it.each([
    ['这份合同要发给对方了，帮我检查一下有没有没删干净的东西', true, { kind: 'check' }],
    ['发出去之前帮我查一下有没有修订和批注', true, { kind: 'check' }],
    ['看看《合同.docx》里还有没有批注', false, { kind: 'check' }],
    ['去掉修订和批注，导出一份 PDF 放桌面', true, { kind: 'deliver', format: 'pdf' }],
    ['帮我把修订都接受了，存一份干净的 Word', true, { kind: 'deliver', format: 'docx' }],
    ['给我一份干净的版本', true, { kind: 'deliver', format: 'docx' }],
    // The words the preflight note tells the model to suggest.
    ['去掉修订和批注，存一份干净的副本', true, { kind: 'deliver', format: 'docx' }],
    ['把《合同.docx》的批注删掉，导出pdf', false, { kind: 'deliver', format: 'pdf' }],
    // A question about cleaning only checks (it reads, nothing is made).
    ['能去掉批注吗？', true, { kind: 'check' }],
    ['我上次发出去的是哪个版本？', false, { kind: 'receipts' }],
    ['之前交付给客户的是哪一版', true, { kind: 'receipts' }],
  ])('%s', (text, inDocument, want) => {
    expect(asksDeliver(text, inDocument)).toEqual(want)
  })

  it.each([
    ['不要删批注，导出 PDF', true],
    ['先别去掉修订', true],
    ['不用检查了，直接发吧', true],
    ['检查一下这段的语法', true],
    ['批注是什么意思', true],
    ['帮我检查一下有没有修订', false], // no document in sight
    ['导出 PDF', true], // not a clean copy: the existing conversion
    ['今天天气怎么样', true],
    ['把这句话里的批注删掉', false], // not about a document
    ['快递回执怎么写', false],
    ['上次那个版本的付款条款是什么', true],
  ])('not a delivery: %s', (text, inDocument) => {
    expect(asksDeliver(text, inDocument)).toBeNull()
  })
})

const P = (over: Partial<DocumentPreflight>): DocumentPreflight => ({ document_id: DOC, revision: 3, title: '合同.docx', format: 'docx', clean: false, cleanable: true, findings: [], ...over })

describe('L8 DOC-4: the check in words', () => {
  it('lists each kind with its count and examples, and what a copy will and will not clean', () => {
    const t = preflightText(P({ findings: [
      { kind: 'tracked_deletions', count: 2, examples: ['旧条款', '第二处'] },
      { kind: 'comments', count: 1, examples: ['张三：这里要改'] },
      { kind: 'unfilled_fields', count: 1, examples: ['{{甲方}}'] },
      { kind: 'author', count: 1, examples: ['李四'] },
    ] }))
    expect(t).toContain('《合同.docx》第 3 版')
    expect(t).toMatch(/修订（删除）\s*2\s*处/)
    expect(t).toContain('旧条款')
    expect(t).toMatch(/批注\s*1\s*处/)
    expect(t).toContain('{{甲方}}')
    expect(t).toContain('李四')
    // A copy cleans revisions, comments and author; not fields or hidden text.
    expect(t).toContain('未填字段和隐藏文字不会自动处理')
    expect(t).toContain('原文档不动')
    expect(t).not.toContain('放心')
  })

  it('found nothing: says what was not checked, never 「放心」', () => {
    const t = preflightText(P({ clean: true }))
    expect(t).toContain('没有发现')
    expect(t).toContain('脚注')
    expect(t).not.toContain('放心')
    const pdf = preflightText(P({ format: 'pdf', cleanable: false, clean: true, title: '合同.pdf' }))
    expect(pdf).toContain('只检查了作者')
    expect(pdf).toContain('不能清理')
  })
})

const R = (over: Partial<DeliveryReceipt>): DeliveryReceipt => ({ document_id: DOC, title: '合同.docx', revision: 3, at: '2026-11-26T10:00:00.000Z', format: 'pdf', file_name: '合同（交付）.pdf', saved_to: '/Users/u/Desktop/合同（交付）.pdf', sha256: 'sha256:ab12', copy_document_id: DOC, again: false, ...over })

describe('L8 DOC-4: the receipts in words', () => {
  it('the latest first, its version, file and place; a second save of the same version said so', () => {
    const t = receiptsText([R({ again: true, file_name: '合同（交付） (2).pdf', saved_to: '/Users/u/Desktop/合同（交付） (2).pdf' }), R({ revision: 2, format: 'docx', file_name: '合同（交付）.docx' })], '合同.docx')
    const first = t.indexOf('合同（交付） (2).pdf')
    expect(first).toBeGreaterThan(-1)
    expect(first).toBeLessThan(t.indexOf('合同（交付）.docx'))
    expect(t).toContain('第 3 版')
    expect(t).toContain('/Users/u/Desktop/合同（交付） (2).pdf')
    expect(t).toContain('同一版本再次保存')
    expect(t).toContain('不表示对方已收到')
  })

  it('none yet', () => {
    expect(receiptsText([], '合同.docx')).toContain('还没有')
  })
})

describe('L8 DOC-4: asking the module', () => {
  it('runs the check of the document and offers the card with the format asked', async () => {
    const request = vi.fn(async () => ({ ok: true, status: 200, data: P({ findings: [{ kind: 'comments', count: 1, examples: ['x'] }] }) }))
    const out = await runPreflight({ request } as never, { id: DOC, title: '合同.docx' }, { kind: 'deliver', format: 'pdf' })
    expect(request).toHaveBeenCalledWith({ op: 'preflight', documentId: DOC })
    expect(out.card).toEqual({ documentId: DOC, title: '合同.docx', revision: 3, cleanable: true, suggested: 'pdf' })
    expect(out.text).toContain('批注')
  })

  it('a failed check says so, with no card', async () => {
    const request = vi.fn(async () => ({ ok: false, status: 422, error: { code: 'unsupported_format', message: 'x' } }))
    const out = await runPreflight({ request } as never, { id: DOC, title: '图.png' }, { kind: 'check' })
    expect(out.card).toBeUndefined()
    expect(out.text).toContain('只能检查 Word 和 PDF')
  })

  it('reads the receipts of the document, or of all', async () => {
    const request = vi.fn(async () => ({ ok: true, status: 200, data: { receipts: [R({})] } }))
    await runReceipts({ request } as never, { id: DOC, title: '合同.docx' })
    expect(request).toHaveBeenLastCalledWith({ op: 'receipts', documentId: DOC })
    const all = await runReceipts({ request } as never, null)
    expect(request).toHaveBeenLastCalledWith({ op: 'receipts' })
    expect(all).toContain('合同（交付）.pdf')
  })
})
