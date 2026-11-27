import { describe, expect, it, vi } from 'vitest'
import { asksCompare, compareSummary, runCompare } from './doc-compare'
import type { DocumentComparison } from '../../../shared/ipc-types'

describe('L6-S05: when a turn asks to compare', () => {
  it('reads 「这一版和上一版改了哪些地方」 as the current version against the one before', () => {
    expect(asksCompare('这一版和上一版改了哪些地方')).toEqual({ kind: 'versions', from: 'previous', to: 'current' })
    expect(asksCompare('对比一下这个版本和上一个版本')).toEqual({ kind: 'versions', from: 'previous', to: 'current' })
  })
  it('reads two numbered versions, the older on the left', () => {
    expect(asksCompare('第3版和第2版有什么不同')).toEqual({ kind: 'versions', from: 2, to: 3 })
  })
  it('reads two quoted titles, or 「这两份」 without any', () => {
    expect(asksCompare('对比一下《甲合同.docx》和《乙合同.docx》')).toEqual({ kind: 'documents', names: ['甲合同.docx', '乙合同.docx'] })
    expect(asksCompare('对比一下这两份合同有什么不同')).toEqual({ kind: 'documents', names: [] })
  })
  it('does not take a question about a contract, or a refusal, for a comparison', () => {
    for (const q of ['合同里写一个月押金对吗', '押金一个月是不是比两个月合理', '我不想对比这两份合同', '别比较这两份了', '上一版是谁写的', '这份合同第二条的违约金是多少']) {
      expect(asksCompare(q), q).toBeNull()
    }
  })
})

describe('L10-1: a yes/no question about two documents is a comparison too', () => {
  it('reads 「《A》和《B》有区别吗」 and the like as comparing two named documents, marked yes/no', () => {
    expect(asksCompare('《劳动合同》和《劳动合同（副本）》有区别吗？')).toEqual({ kind: 'documents', names: ['劳动合同', '劳动合同（副本）'], yesNo: true })
    for (const q of ['《甲.docx》和《乙.docx》一样吗', '《甲.docx》和《乙.docx》是否一致', '《甲.docx》跟《乙.docx》有没有不同', '《甲.docx》和《乙.docx》是不是同一份', '《甲.docx》和《乙.docx》有没有改动', '《甲.docx》与《乙.docx》内容相同吗', '《甲.docx》和《乙.docx》一样不一样']) {
      expect(asksCompare(q), q).toEqual({ kind: 'documents', names: ['甲.docx', '乙.docx'], yesNo: true })
    }
  })
  it('reads 「这两份一样吗」 and 「这两份有什么不同」 without a verb', () => {
    expect(asksCompare('这两份一样吗')).toEqual({ kind: 'documents', names: [], yesNo: true })
    expect(asksCompare('这两份合同有没有区别')).toEqual({ kind: 'documents', names: [], yesNo: true })
    expect(asksCompare('这两份有什么不同')).toEqual({ kind: 'documents', names: [] })
  })
  it('reads a yes/no question about this version and the one before as comparing versions', () => {
    expect(asksCompare('这一版和上一版有区别吗')).toEqual({ kind: 'versions', from: 'previous', to: 'current' })
  })
  it('does not take a yes/no question without two documents, or a refusal, for a comparison', () => {
    for (const q of ['年假和事假有区别吗', '《合同.docx》改过吗', '《合同.docx》有没有问题', '病假和事假一样吗', '我不想比较这两份是不是一样', '别对比这两份有没有区别']) {
      expect(asksCompare(q), q).toBeNull()
    }
  })
})

const doc = (n: number, text: string) => ({ block_ref: { paragraph_index: n }, text })
const comparison = (over: Partial<DocumentComparison> = {}): DocumentComparison => ({
  left: { document_id: 'doc_a', revision: 1, media_type: 'd' }, right: { document_id: 'doc_a', revision: 2, media_type: 'd' },
  identical: false, only_text: true, truncated: false, next_offset: null,
  summary: { total: 3, added: 1, removed: 0, changed: 1, unaligned: 1 },
  changes: [
    { id: 'c1', kind: 'changed', left: doc(1, '租金 5 万元'), right: doc(1, '租金 8 万元'), spans: [{ op: 'equal', text: '租金 ' }, { op: 'delete', text: '5' }, { op: 'insert', text: '8' }, { op: 'equal', text: ' 万元' }] },
    { id: 'c2', kind: 'added', left: null, right: doc(4, '另见附件'), spans: [] },
    { id: 'c3', kind: 'unaligned', left: doc(7, '违约责任'), right: null, spans: [] },
  ],
  ...over,
})

describe('L6-S05: the summary is made of the changes, by code', () => {
  it('has one line per change, each with its id, and says only text was compared', () => {
    const s = compareSummary(comparison())
    expect(s.head).toBe('共 3 处改动：删 0 处、加 1 处、改 1 处、无法对齐 1 处 · 只比较文字')
    expect(s.lines.map((l) => l.id)).toEqual(['c1', 'c2', 'c3'])
    expect(s.lines[0]!.text).toContain('〔5→8〕')
    expect(s.lines[0]!.text).toContain('第 2 段')
    expect(s.lines[1]!.text).toContain('另见附件')
    expect(s.lines[2]!.text).toContain('无法对齐')
  })
  it('says there is no difference, and how many are not listed', () => {
    expect(compareSummary(comparison({ identical: true, changes: [], summary: { total: 0, added: 0, removed: 0, changed: 0, unaligned: 0 } })).head).toBe('没有文字差异 · 只比较文字')
    const many = Array.from({ length: 12 }, (_, i) => ({ id: `c${i + 1}`, kind: 'added' as const, left: null, right: doc(i, `新增第${i}段`), spans: [] }))
    const s = compareSummary(comparison({ changes: many, summary: { total: 12, added: 12, removed: 0, changed: 0, unaligned: 0 } }))
    expect(s.lines).toHaveLength(8)
    expect(s.more).toBe(4)
  })
})

describe('L6-S05: running a comparison', () => {
  const ok = (data: unknown) => ({ ok: true, status: 200, data })
  it('compares a document with its previous version, by revision numbers read from its versions', async () => {
    const request = vi.fn(async (r: { op: string }) => r.op === 'revisions' ? ok({ document_id: 'doc_a', head_revision: 3, revisions: [{ revision: 3 }, { revision: 2 }, { revision: 1 }] }) : ok(comparison()))
    const out = (await runCompare({ request } as never, { kind: 'versions', from: 'previous', to: 'current' }, { id: 'doc_a', title: '合同.docx' }, []))!
    expect(request).toHaveBeenLastCalledWith({ op: 'compare', left: { documentId: 'doc_a', revision: 2 }, right: { documentId: 'doc_a', revision: 3 } })
    expect(out.card?.lines).toHaveLength(3)
    expect(out.text).toContain('共 3 处改动')
  })
  it('says so when there is one version only, or no such version', async () => {
    const one = vi.fn(async () => ok({ document_id: 'doc_a', head_revision: 1, revisions: [{ revision: 1 }] }))
    expect((await runCompare({ request: one } as never, { kind: 'versions', from: 'previous', to: 'current' }, { id: 'doc_a', title: '合同.docx' }, []))!.text).toContain('只有一个版本')
    const two = vi.fn(async () => ok({ document_id: 'doc_a', head_revision: 2, revisions: [{ revision: 2 }, { revision: 1 }] }))
    expect((await runCompare({ request: two } as never, { kind: 'versions', from: 5, to: 'current' }, { id: 'doc_a', title: '合同.docx' }, []))!.text).toContain('没有第 5 版')
  })
  it('asks which two when it does not know, and explains a refusal', async () => {
    const out = (await runCompare({ request: vi.fn() } as never, { kind: 'documents', names: [] }, null, []))!
    expect(out.text).toContain('请说要对比哪两份')
    const refused = vi.fn(async () => ({ ok: false, status: 422, error: { code: 'unsupported_format', message: 'm' } }))
    const r = (await runCompare({ request: refused } as never, { kind: 'versions', from: 1, to: 2 }, { id: 'doc_a', title: '合同.docx' }, []))!
    expect(r.text).not.toContain('共')
  })
})

describe('L10-1: a yes/no question about documents that are not in the library goes to the model', () => {
  const ok = (data: unknown) => ({ ok: true, status: 200, data })
  const lib = (titles: string[]) => vi.fn(async (r: { op: string }) => r.op === 'list'
    ? ok({ documents: titles.map((t, i) => ({ document_id: `doc_${i}`, title: t, head_revision: 1, created_at: '2026-11-27T00:00:00Z', media_type: 'd' })) })
    : ok(comparison()))
  it('returns null when a named document is not in the library (「《劳动法》和《劳动合同法》有区别吗」 is a question of law)', async () => {
    const request = lib(['劳动合同.docx'])
    expect(await runCompare({ request } as never, { kind: 'documents', names: ['劳动法', '劳动合同法'], yesNo: true }, null, [])).toBeNull()
    expect(request.mock.calls.some(([r]) => (r as { op: string }).op === 'compare')).toBe(false)
  })
  it('still compares when both are in the library', async () => {
    const request = lib(['劳动合同.docx', '劳动合同（副本）.docx'])
    const out = await runCompare({ request } as never, { kind: 'documents', names: ['劳动合同.docx', '劳动合同（副本）.docx'], yesNo: true }, null, [])
    expect(out).not.toBeNull()
    expect(request.mock.calls.some(([r]) => (r as { op: string }).op === 'compare')).toBe(true)
  })
  it('keeps saying a document was not found when the turn asked to compare outright', async () => {
    const request = lib(['劳动合同.docx'])
    const out = await runCompare({ request } as never, { kind: 'documents', names: ['劳动法', '劳动合同法'] }, null, [])
    expect(out?.text).toContain('没有找到')
  })
})
