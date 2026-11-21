import { describe, expect, it } from 'vitest'
import { anchorMatches, mismatchedEdit } from './patch-anchor'

// The same cases as docx_patch's `an_anchor_is_the_opening_of_the_paragraph_at_its_index`.
describe('anchorMatches (M1 (c), as the documents module checks it)', () => {
  const para = '第三条 押金为两个月租金，合同期满无违约时全额退还。'
  it('takes the opening, numbering left out or written right', () => {
    for (const ok of ['押金为两个月', '第三条 押金为两个月', '第三条押金为两个月租金，', ' 押金为 两个月 ']) expect(anchorMatches(para, ok)).toBe(true)
    expect(anchorMatches('一、总则', '总则')).toBe(true)
    expect(anchorMatches('（一）租金每月 8,500 元', '租金每月 8,500')).toBe(true)
  })
  it('refuses wrong numbering, other words and fewer than six', () => {
    for (const wrong of ['第四条 押金为两个月', '乙方：张三', '合同期满无违约', '第三条 押金', '押金为', '', '。，']) expect(anchorMatches(para, wrong)).toBe(false)
    expect(anchorMatches('乙方：张三', '押金为两个月')).toBe(false)
  })
})

describe('mismatchedEdit', () => {
  const originals = new Map([[2, '乙方：张三'], [5, '第三条 押金为两个月租金，合同期满无违约时全额退还。']])
  it('says which edit is aimed wrong and how that paragraph begins', () => {
    expect(mismatchedEdit(originals, [{ index: 5, anchor: '押金为两个月' }])).toBeNull()
    expect(mismatchedEdit(originals, [{ index: 2, anchor: '押金为两个月' }])).toEqual({ index: 2, opening: '乙方：张三' })
    expect(mismatchedEdit(originals, [{ index: 9, anchor: '押金为两个月' }])).toEqual({ index: 9, opening: '' })
  })
})
