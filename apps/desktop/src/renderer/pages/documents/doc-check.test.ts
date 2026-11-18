import { describe, expect, it, vi } from 'vitest'
import {
  CHUNK_CHARS,
  CONTRACT_CHECKLIST, missingOf,
  MAX_CHUNKS,
  checkDocument,
  reportMarkdown,
  type CheckBlock,
  type CheckDeps,
} from './doc-check'

const block = (id: string, text: string, page?: number): CheckBlock =>
  ({ block_id: id, text, ...(page === undefined ? {} : { page }) })

function asks(replies: string[]): CheckDeps & { ask: ReturnType<typeof vi.fn> } {
  const ask = vi.fn()
  for (const r of replies) ask.mockResolvedValueOnce(r)
  ask.mockResolvedValue('[]')
  return { ask }
}

describe('checkDocument', () => {
  it('keeps an item whose quote is in the ref block, with that block and page as source', async () => {
    const deps = asks([
      JSON.stringify([{ kind: 'deadline', text: '合同于2026年12月31日到期', quote: '2026年12月31日到期', ref: 'b1' }]),
      JSON.stringify({ summary: [{ text: '年底到期', refs: [1] }], conflicts: [], missing: [] }),
    ])
    const report = await checkDocument(deps, '租赁合同', [block('blk-a', '本合同于2026年12月31日到期。', 2)])
    expect(report.items).toEqual([{
      kind: 'deadline',
      text: '合同于2026年12月31日到期',
      source: { block_id: 'blk-a', page: 2, quote: '2026年12月31日到期' },
    }])
    expect(report.summary).toEqual([{
      text: '年底到期',
      sources: [{ block_id: 'blk-a', page: 2, quote: '2026年12月31日到期' }],
    }])
    expect(report.cut).toBe(false)
  })

  it('retargets a wrong ref when the quote appears in exactly one other block', async () => {
    const deps = asks([
      JSON.stringify([{ kind: 'amount', text: '金额100万', quote: '人民币100万元', ref: 'b1' }]),
      JSON.stringify({ summary: [], conflicts: [], missing: [] }),
    ])
    const report = await checkDocument(deps, '合同', [
      block('blk-a', '甲方：张三', 1),
      block('blk-b', '合同金额为人民币100万元', 2),
    ])
    expect(report.items).toEqual([{
      kind: 'amount',
      text: '金额100万',
      source: { block_id: 'blk-b', page: 2, quote: '人民币100万元' },
    }])
  })

  it('drops an item whose quote is not in the document', async () => {
    const deps = asks([
      JSON.stringify([{ kind: 'condition', text: '编造的条款', quote: '乙方须在火星交货', ref: 'b1' }]),
      JSON.stringify({ summary: [{ text: '有一条', refs: [1] }], conflicts: [], missing: [] }),
    ])
    const report = await checkDocument(deps, '合同', [block('blk-a', '甲方按时付款。', 1)])
    expect(report.items).toEqual([])
    expect(report.summary).toEqual([])
  })

  it('parses a reply wrapped in think tags and extra prose', async () => {
    const deps = asks([
      '<think>先想一下</think>好的，这是结果：\n[{"kind":"deadline","text":"30天内付款","quote":"付款期限30天","ref":"b1"}]\n谢谢。',
      '<think>汇总</think> extra {"summary":[{"text":"要在30天内付款","refs":[1]}],"conflicts":[],"missing":[]}',
    ])
    const report = await checkDocument(deps, '合同', [block('blk-a', '付款期限30天。', 1)])
    expect(report.items).toHaveLength(1)
    expect(report.items[0].text).toBe('30天内付款')
    expect(report.summary).toHaveLength(1)
  })

  it('drops summary without a valid ref, conflicts with fewer than 2 refs, and missing outside the checklist', async () => {
    const deps = asks([
      JSON.stringify([
        { kind: 'deadline', text: '年底到期', quote: '2026年12月31日', ref: 'b1' },
        { kind: 'amount', text: '100万', quote: '100万元', ref: 'b2' },
      ]),
      JSON.stringify({
        summary: [
          { text: '没有编号', refs: [] },
          { text: '编号不存在', refs: [99] },
          { text: '有效摘要', refs: [1] },
        ],
        conflicts: [
          { text: '只有一个编号', refs: [1] },
          { text: '一个有效一个无效', refs: [1, 99] },
          { text: '两处不一致', refs: [1, 2] },
        ],
        missing: ['争议解决', '不是清单里的', '合同当事人', '争议解决'],
      }),
    ])
    const report = await checkDocument(deps, '合同', [
      block('a', '本合同于2026年12月31日终止。', 1),
      block('b', '总价100万元。', 2),
    ])
    expect(report.summary.map((c) => c.text)).toEqual(['有效摘要'])
    expect(report.conflicts.map((c) => c.text)).toEqual(['两处不一致'])
    expect(report.missing).toEqual(['合同当事人', '争议解决'])
    expect(report.missing.every((m) => CONTRACT_CHECKLIST.includes(m))).toBe(true)
  })

  it('never says an element is missing when the text has it (General 11-18)', () => {
    const blocks = [block('a', '出租方（甲方）：北京某某科技有限公司；乙方：张三。租期自2026年1月1日至2026年12月31日，月租金5000元。', 1)]
    expect(missingOf(['合同当事人', '合同期限', '付款金额与方式', '争议解决'], blocks)).toEqual(['争议解决'])
  })

  it('cuts after MAX_CHUNKS and asks once per kept chunk plus the summary', async () => {
    const blocks = Array.from({ length: MAX_CHUNKS + 1 }, (_, i) =>
      block(`b${i}`, '甲'.repeat(CHUNK_CHARS), i + 1))
    const replies = [
      ...Array.from({ length: MAX_CHUNKS }, () => '[]'),
      JSON.stringify({ summary: [], conflicts: [], missing: ['争议解决'] }),
    ]
    const deps = asks(replies)
    const report = await checkDocument(deps, '长合同', blocks)
    expect(report.cut).toBe(true)
    expect(deps.ask).toHaveBeenCalledTimes(MAX_CHUNKS + 1)
    expect(report.missing).toEqual(['争议解决'])
  })
})

describe('reportMarkdown', () => {
  it('includes page numbers, omits empty sections, and says so when nothing is cited', () => {
    const withPages = reportMarkdown('租赁合同', {
      summary: [{ text: '年底到期', sources: [{ block_id: 'a', page: 2, quote: '到期' }] }],
      items: [
        { kind: 'deadline', text: '2026-12-31到期', source: { block_id: 'a', page: 2, quote: '到期' } },
        { kind: 'amount', text: '100万', source: { block_id: 'b', quote: '100万' } },
      ],
      conflicts: [],
      missing: ['争议解决'],
      cut: false,
    })
    expect(withPages).toContain('摘要')
    expect(withPages).toContain('年底到期（第 2 页）')
    expect(withPages).toContain('期限')
    expect(withPages).toContain('2026-12-31到期（第 2 页）')
    expect(withPages).toContain('金额')
    expect(withPages).toContain('100万（出处：块）')
    expect(withPages).not.toContain('条件')
    expect(withPages).not.toContain('可能矛盾')
    expect(withPages).toContain('未找到')
    expect(withPages).toContain('争议解决')
    expect(withPages).toContain('在文档页点「总结并检查」可以逐条跳回原文。')
    expect(withPages).not.toContain('文档较长')

    const empty = reportMarkdown('空', {
      summary: [], items: [], conflicts: [], missing: [], cut: true,
    })
    expect(empty).toContain('没有找到可以给出出处的结论。')
    expect(empty).not.toContain('摘要')
    expect(empty).not.toContain('期限')
    expect(empty).toContain('文档较长，只检查了前面一部分')
    expect(empty).toContain('在文档页点「总结并检查」可以逐条跳回原文。')
  })
})
