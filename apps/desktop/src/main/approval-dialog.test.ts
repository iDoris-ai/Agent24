import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { approvalDialog, type DocumentLookups } from './approval-dialog'
import { resetMainLocaleForTest, setMainLocale } from './main-i18n'

const clip = (s: string, max: number) => (s.length > max ? `${s.slice(0, max)}…` : s)
const DOC = 'doc_01M8035XWKZJGGD3NHF3X0HJ0H'
const lookups = (title: string | null, paragraphs: Map<number, string> | null): DocumentLookups => ({
  title: vi.fn(async () => title),
  paragraphs: vi.fn(async () => paragraphs),
})
const patch = (edits: unknown) => ({
  summary: `documents.patch: {"document_id":"${DOC}","edits":[{"anchor":… [truncated 94 bytes]`,
  payload: { document_id: DOC, edits },
})
const box = async (...a: Parameters<typeof approvalDialog>) => {
  const out = await approvalDialog(...a)
  if (!out.ok) throw new Error('refused')
  return out
}

describe('approvalDialog (UI polish 01)', () => {
  beforeEach(() => setMainLocale('zh'))
  afterEach(() => resetMainLocaleForTest())

  it('a patch: the document by its title, each paragraph as the document has it, no ids or JSON', async () => {
    const paras = new Map([[2, '甲方应于合同签订后十日内付款。']])
    const out = await box(patch([{ index: 2, text: '甲方须在签约后五日内付款。', tracked: true, anchor: '甲方应于' }]),
      'approve', '批准', undefined, lookups('采购合同.docx', paras), clip)
    expect(out.message).toBe('确认让助理改写《采购合同.docx》？')
    expect(out.detail).toBe([
      '以修订方式改写 1 段，生成一份新文档。',
      '第 3 段：「甲方应于合同签订后十日内付款。」→「甲方须在签约后五日内付款。」',
      '原文档不会被改动。',
    ].join('\n'))
    expect(`${out.message}${out.detail}`).not.toMatch(/doc_|run_|apr_|\{|documents\.patch|anchor/)
  })

  it('more than 5 edits: the real count, and that only the first 5 are listed', async () => {
    const edits = Array.from({ length: 7 }, (_, i) => ({ index: i, text: `新${i}` }))
    const out = await box(patch(edits), 'approve', '批准', undefined, lookups('a.docx', new Map()), clip)
    expect(out.detail).toContain('改写 7 段')
    expect(out.detail).toContain('共 7 段，下面列出前 5 段：')
    expect(out.detail).toContain('第 5 段')
    expect(out.detail).not.toContain('第 6 段')
  })

  it('the document unreadable: 「这份文档」 and 「（没读到原文）」, still the edits', async () => {
    const out = await box(patch([{ index: 0, text: '新' }]), 'approve', '批准', undefined, lookups(null, null), clip)
    expect(out.message).toBe('确认让助理改写《这份文档》？')
    expect(out.detail).toContain('第 1 段：（没读到原文）→「新」')
  })

  it('a patch without its edits is refused, never shown as if whole', async () => {
    const out = await approvalDialog({ summary: patch([]).summary, payload: { document_id: DOC } }, 'approve', '批准', undefined, lookups('a', null), clip)
    expect(out.ok).toBe(false)
  })

  it('a compose names the new document and its sources by title', async () => {
    const out = await box({
      summary: 'documents.compose: {…}',
      payload: { title: '合同（第 2-3 页）', parts: [{ document_id: DOC, pages: '2-3' }, { document_id: '《附件》' }] },
    }, 'approve', '批准', undefined, lookups('合同.pdf', null), clip)
    expect(out.message).toBe('确认让助理生成新文档《合同（第 2-3 页）》？')
    expect(out.detail).toContain('来源：《合同.pdf》第 2-3 页 + 《附件》全部页')
  })

  it('another tool: its name in words, what it does, the decision when not a plain approve, the page note marked', async () => {
    const out = await box({ summary: 'shell_exec: {"argv":["ls"]}', payload: { argv: ['ls'] } },
      'approve_for_session', '本会话内一律批准', '看一下', lookups(null, null), clip)
    expect(out.message).toBe('确认允许助理「执行命令」？')
    expect(out.detail).toContain('你的决定：本会话内一律批准')
    expect(out.detail).toContain('具体内容：{"argv":["ls"]}')
    expect(out.detail).toContain('页面备注（仅供参考）：看一下')
  })
})
