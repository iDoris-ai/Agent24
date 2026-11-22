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

  // PR-Daemon #1995 L1: an edit past the 60th character showed the same
  // opening on both sides; the box shows the text around the first change.
  it('a change late in a long paragraph: both sides cut around where they first differ', async () => {
    const head = '本合同由甲乙双方在平等自愿、协商一致的基础上订立，双方应当遵守国家有关法律法规及本合同的全部约定条款内容，未尽事宜另行协商，'
    expect(head.length).toBeGreaterThan(60)
    const out = await box(patch([{ index: 0, text: `${head}乙方须在五日内付款。` }]),
      'approve', '批准', undefined, lookups('a.docx', new Map([[0, `${head}乙方应于十日内付款。`]])), clip)
    const line = out.detail.split('\n')[1]!
    expect(line).toContain('乙方应于十日内付款。')
    expect(line).toContain('乙方须在五日内付款。')
    expect(line).toMatch(/^第 1 段：「…/)
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

  // General 0.6.96: documents.paragraphs (asked for approval after another
  // regression) showed 「具体内容：{"count":1,"document_id":"doc_01M8035X…","from":3}」.
  it('a read-only documents tool: the document by its title, no JSON, no ids, no content line', async () => {
    const out = await box({ summary: `documents.paragraphs: {"count":1,"document_id":"${DOC}","from":3}`, payload: { count: 1, document_id: DOC, from: 3 } },
      'approve', '批准', undefined, lookups('采购合同.docx', null), clip)
    expect(out.message).toBe('确认允许助理对《采购合同.docx》「查看文档的段落」？')
    expect(out.detail).toBe('只是查看，不会改动文档。')
  })

  it('another tool naming a document: the document by its title, the rest as a list', async () => {
    const out = await box({ summary: 'x_tool: {…}', payload: { document_id: DOC, mode: 'fast', opts: { deep: true } } },
      'approve', '批准', undefined, lookups('a.docx', null), clip)
    expect(out.message).toBe('确认允许助理对《a.docx》「x_tool」？')
    expect(out.detail).toContain('具体内容：\nmode：fast\nopts：\n  deep：true')
    expect(`${out.message}${out.detail}`).not.toMatch(/doc_|[{}"]/)
  })

  it('another tool: its name in words, what it does, the decision when not a plain approve, the page note marked', async () => {
    const out = await box({ summary: 'shell_exec: {"argv":["ls"]}', payload: { argv: ['ls'] } },
      'approve_for_session', '本会话内一律批准', '看一下', lookups(null, null), clip)
    expect(out.message).toBe('确认允许助理「执行命令」？')
    expect(out.detail).toContain('你的决定：本会话内一律批准')
    expect(out.detail).toContain('具体内容：\nargv：\n  1. ls')
    expect(out.detail).not.toMatch(/[{}[\]"]/)
    expect(out.detail).toContain('页面备注（仅供参考）：看一下')
  })
})
