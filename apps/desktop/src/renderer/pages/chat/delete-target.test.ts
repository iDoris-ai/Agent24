import { describe, expect, it, vi } from 'vitest'
import { choiceText, deletePrompt, deleteTargetOf } from './delete-target'

const doc = (n: number, title: string) => ({ document_id: `doc_01M679KMY6D75P0EZACBS95JY${n}`, title, created_at: '2026-11-21T01:00:00.000Z' })
const api = (docs: ReturnType<typeof doc>[], ok = true) => ({
  request: vi.fn(async (req: { op: string; documentId?: string }) => !ok
    ? { ok: false, status: 503, error: { code: 'storage_unavailable', message: 'x' } }
    : req.op === 'list' ? { ok: true, data: { documents: docs, next_cursor: null } } : { ok: true, data: { pages: 3 } }),
}) as unknown as Window['agent24']['documents']

describe('L6-F2: the App names the document a delete is for', () => {
  const lib = [doc(1, '房屋租赁合同-测试_其余页面.pdf'), doc(2, '房屋租赁合同-测试.pdf'), doc(3, '发票.pdf')]
  it('the only document of that title (General 0.6.105: the model made an id up)', async () => {
    expect(await deleteTargetOf(api(lib), null, '把《房屋租赁合同-测试_其余页面》删了'))
      .toEqual({ kind: 'one', id: lib[0]!.document_id, title: '房屋租赁合同-测试_其余页面.pdf' })
  })
  it('an equal title wins over those containing it, as the module resolves names', async () => {
    expect(await deleteTargetOf(api(lib), null, '把《房屋租赁合同-测试.pdf》删了')).toMatchObject({ kind: 'one', id: lib[1]!.document_id })
  })
  it('only in part and several: a choice, by title, time and pages', async () => {
    const t = await deleteTargetOf(api(lib), null, '把《房屋租赁》删了')
    expect(t).toMatchObject({ kind: 'choose', total: 2 })
    expect(t?.kind === 'choose' && t.candidates.map((c) => c.pages)).toEqual([3, 3])
  })
  it('none, or the library unread: said, nothing deleted', async () => {
    expect(await deleteTargetOf(api(lib), null, '把《不存在》删了')).toEqual({ kind: 'none', text: '文档库里没有找到《不存在》，这次没有删除任何文档。' })
    expect(await deleteTargetOf(api(lib, false), null, '把《发票》删了')).toMatchObject({ kind: 'none' })
  })
  it('this conversation\'s document, or none named (the model finds 「上一份」)', async () => {
    expect(await deleteTargetOf(api(lib), { id: 'doc_X', title: 'X.pdf' }, '把这份删了')).toEqual({ kind: 'one', id: 'doc_X', title: 'X.pdf' })
    expect(await deleteTargetOf(api(lib), null, '删除上一份拆出来的文件')).toBeNull()
  })
  it('a 《B》 named in A\'s conversation is B, not A (PR-Daemon #2047)', async () => {
    const a = { id: 'doc_A', title: '发票.pdf' }
    expect(await deleteTargetOf(api(lib), a, '把《房屋租赁合同-测试_其余页面》删了'))
      .toEqual({ kind: 'one', id: lib[0]!.document_id, title: '房屋租赁合同-测试_其余页面.pdf' })
    expect(await deleteTargetOf(api(lib), a, '把《房屋租赁》删了')).toMatchObject({ kind: 'choose', total: 2 })
    expect(await deleteTargetOf(api(lib), a, '把《不存在》删了')).toMatchObject({ kind: 'none' })
    // naming the open document itself needs no lookup
    expect(await deleteTargetOf(api(lib), a, '把《发票》删了')).toEqual({ kind: 'one', id: 'doc_A', title: '发票.pdf' })
  })
  it('a title that merely contains the name does not win over the exact one (PR-Daemon #2052)', async () => {
    const l2 = [doc(1, '发票-2024.pdf'), doc(2, '发票.pdf')]
    expect(await deleteTargetOf(api(l2), { id: l2[0]!.document_id, title: '发票-2024.pdf' }, '把《发票》删了'))
      .toEqual({ kind: 'one', id: l2[1]!.document_id, title: '发票.pdf' })
  })
  it('the prompt fixes the id and says not to ask the user for one', () => {
    const p = deletePrompt({ id: 'doc_X', title: 'X.pdf' }, '把这份删了')
    expect(p).toContain('document_id: doc_X')
    expect(p).toContain('不要向用户要 id')
    expect(choiceText('合同', 12, 10)).toContain('只列出最近的 10 份')
  })
})
