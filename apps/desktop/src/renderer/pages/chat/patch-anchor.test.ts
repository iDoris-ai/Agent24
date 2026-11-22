import fs from 'node:fs'
import path from 'node:path'
import { describe, expect, it } from 'vitest'
import { anchorMatches, mismatchedEdit } from './patch-anchor'

// The cases docx_patch's `an_anchor_is_the_opening_of_the_paragraph_at_its_index`
// reads: one table for both sides of the rule.
const CASES = JSON.parse(fs.readFileSync(path.resolve(__dirname, '../../../../../../rust/apps/agent24-documents/tests/fixtures/anchor-cases.json'), 'utf8')) as Array<{ paragraph: string; anchor: string; ok: boolean }>

describe('anchorMatches (M1 (c), as the documents module checks it)', () => {
  it('reads the shared case table', () => expect(CASES.length).toBeGreaterThanOrEqual(25))
  it.each(CASES.map((c) => [c.paragraph, c.anchor, c.ok] as const))('%j opened by %j: %s', (paragraph, anchor, ok) => {
    expect(anchorMatches(paragraph, anchor)).toBe(ok)
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
