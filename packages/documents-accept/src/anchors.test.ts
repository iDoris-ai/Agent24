import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { type Anchor, type Locate, fieldAnchors, scoreAnchors } from './anchors'
import { type GoldAnchor, type GoldField, loadSample } from './samples'
import type { Extracted } from './score'

const S01 = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '..', 'docs', 'documenting', 'samples', 's01')
const NOTICE = loadSample(path.join(S01, 's01-07-mixed-building-notice')).gold
const gold = (key: string): GoldField => NOTICE.fields.find((f) => f.key === key)!

/** An anchor of the result, or a match of `find`: bytes `start` to `end` of a block. */
const at = (block_id: string, start: number, end: number, more: Partial<Anchor> = {}): Anchor =>
  ({ text_layer_sha256: 'sha256:layer', block_id, page: 1, text_range: { unit: 'utf8', start, end }, quote: '', ...more }) as Anchor

const WORK_ORDER: GoldAnchor = { page: 1, quote: '工单号 Work Order: WO-000731' }
const DATE: GoldAnchor = { page: 1, quote: '2026年10月8日' }
/** `find` placed the work order in block p1/b2, bytes 10 to 40, and did not find the date. */
const locate: Locate = (g) => (g === WORK_ORDER ? [at('p1/b2', 10, 40)] : [])
const one = (ours: Anchor[] | undefined, g = WORK_ORDER) => scoreAnchors([g], ours, locate).anchors[0]

describe('a gold anchor that find located', () => {
  it('is covered by a result anchor that shares part of its block', () => {
    expect(one([at('p1/b2', 30, 39)])).toEqual({ gold: WORK_ORDER, by: 'place', unlocated: false })
    expect(one([at('p1/b9', 0, 5), at('p1/b2', 0, 11)])?.by).toBe('place')
  })

  it('is not covered by one that only touches it, is in another block, or matches only its quote', () => {
    // Ranges are half-open: 40 is the first byte after the gold anchor.
    expect(one([at('p1/b2', 40, 50)])?.by).toBeUndefined()
    expect(one([at('p1/b2', 0, 10)])?.by).toBeUndefined()
    expect(one([at('p1/b3', 10, 40)])?.by).toBeUndefined()
    // The same words elsewhere on the page are another occurrence.
    expect(one([at('p1/b7', 0, 30, { quote: WORK_ORDER.quote })])).toEqual({ gold: WORK_ORDER, unlocated: false })
  })

  it('is compared by quote when the result comes from another text layer', () => {
    const other = { text_layer_sha256: 'sha256:other' }
    expect(one([at('p1/b2', 10, 40, other)])?.by).toBeUndefined()
    expect(one([at('p1/b2', 10, 40, { ...other, quote: 'Work Order: WO-000731' })])).toEqual({ gold: WORK_ORDER, by: 'quote', unlocated: false })
    // The same block and bytes in another layer are no place in this one.
    expect(one([at('p1/b9', 0, 5), at('p1/b2', 10, 40, other)])?.by).toBeUndefined()
  })
})

describe('a gold anchor that find did not locate', () => {
  it('is covered by a result anchor on its page whose quote holds, or is held by, its quote', () => {
    expect(one([at('p1/b4', 0, 9, { quote: '2026 年 10 月 8 日' })], DATE)).toEqual({ gold: DATE, by: 'quote', unlocated: true })
    expect(one([at('p1/b4', 0, 9, { quote: 'Tower B Facility Management 2026年10月8日' })], DATE)?.by).toBe('quote')
    expect(one([at('p1/b4', 0, 9, { quote: '10月8日' })], DATE)?.by).toBe('quote')
  })

  it('stays uncovered, and says so, when no quote matches or the page is another', () => {
    expect(one([at('p1/b4', 0, 9, { quote: '2026年10月12日' })], DATE)).toEqual({ gold: DATE, unlocated: true })
    expect(one([at('p2/b1', 0, 9, { page: 2, quote: DATE.quote })], DATE)?.by).toBeUndefined()
    expect(one([at('p1/b4', 0, 9, { quote: '' })], DATE)?.by).toBeUndefined()
  })
})

describe('scoreAnchors', () => {
  it('says how many of the gold anchors are covered', () => {
    const verdict = (ours: Anchor[] | undefined) => scoreAnchors([WORK_ORDER, DATE], ours, locate).verdict
    expect(verdict([at('p1/b2', 10, 40), at('p1/b4', 0, 9, { quote: DATE.quote })])).toBe('all')
    expect(verdict([at('p1/b2', 10, 40)])).toBe('some')
    expect(verdict([at('p1/b9', 0, 5)])).toBe('none')
  })

  it('finds nothing covered when the result gave no anchors, and still says what find could not place', () => {
    expect(scoreAnchors([WORK_ORDER, DATE], [], locate)).toEqual({
      verdict: 'unsourced',
      anchors: [{ gold: WORK_ORDER, unlocated: false }, { gold: DATE, unlocated: true }],
    })
    expect(scoreAnchors([WORK_ORDER], undefined, locate).verdict).toBe('unsourced')
  })

  it('takes anchors that break the contract for no anchor, without a crash', () => {
    expect(scoreAnchors([WORK_ORDER], [null, 7] as never, locate).verdict).toBe('unsourced')
    expect(scoreAnchors([WORK_ORDER, DATE], [{ block_id: 'p1/b2', text_layer_sha256: 'sha256:layer', quote: 8 }] as never, locate).verdict).toBe('none')
  })
})

describe('fieldAnchors', () => {
  const order = gold('work_order')
  const date = gold('shutoff_date') // candidates: the 12th (2 anchors), the 13th (3), the 12th in English (1)
  const everywhere: Locate = (g) => [at(`p1/${g.quote}`, 0, 5)]
  const there = (g: GoldAnchor) => at(`p1/${g.quote}`, 0, 5)
  const candidate = (value: string, normalized: string, anchors: Anchor[]) => ({ value, normalized, anchors })

  it('scores a field with a value on both sides, and no other', () => {
    const present: Extracted = { key: 'work_order', status: 'present', value: 'WO-000731', anchors: order.anchors!.map(there) }
    expect(fieldAnchors(order, present, everywhere)?.verdict).toBe('all')
    expect(fieldAnchors(order, { ...present, anchors: [] }, everywhere)?.verdict).toBe('unsourced')
    expect(fieldAnchors(order, { key: 'work_order', status: 'missing' }, everywhere)).toBeUndefined()
    expect(fieldAnchors(order, undefined, everywhere)).toBeUndefined()
    expect(fieldAnchors(gold('contact_phone'), { key: 'contact_phone', status: 'present', value: 'x', anchors: [] }, everywhere)).toBeUndefined()
    expect(fieldAnchors(date, { key: 'shutoff_date', status: 'present', value: '10月12日', anchors: [] }, everywhere)).toBeUndefined()
  })

  it('scores a conflict by the candidate groups that match', () => {
    const [zh12, zh13, en12] = date.candidates!
    expect([zh12, zh13, en12].map((c) => c!.anchors.length)).toEqual([2, 3, 1])
    const twelfth = candidate('10月12日', '2026-10-12', [...zh12!.anchors, ...en12!.anchors].map(there))
    const thirteenth = candidate('10月13日', '2026-10-13', [there(zh13!.anchors[0]!)])
    const both = fieldAnchors(date, { key: 'shutoff_date', status: 'conflict', candidates: [twelfth, thirteenth] }, everywhere)
    expect(both?.anchors).toHaveLength(6)
    expect(both?.anchors.filter((a) => a.by === 'place')).toHaveLength(4)
    expect(both?.verdict).toBe('some')
    // Only the group of the 12th is in the result: the anchors of the 13th are not counted.
    const half = fieldAnchors(date, { key: 'shutoff_date', status: 'conflict', candidates: [twelfth, candidate('10月14日', '2026-10-14', [])] }, everywhere)
    expect(half).toMatchObject({ verdict: 'all' })
    expect(half?.anchors).toHaveLength(3)
    // A candidate's anchors speak for its own group only: the 12th cited at the
    // place of the 13th covers nothing, and does not hide that the 13th has no source.
    const misplaced = candidate('10月12日', '2026-10-12', zh13!.anchors.map(there))
    const crossed = fieldAnchors(date, { key: 'shutoff_date', status: 'conflict', candidates: [misplaced, candidate('10月13日', '2026-10-13', [])] }, everywhere)
    expect(crossed?.verdict).toBe('none')
    expect(crossed?.anchors.filter((a) => a.by !== undefined)).toHaveLength(0)
    const unsourced = [candidate('10月12日', '2026-10-12', []), candidate('10月13日', '2026-10-13', [])]
    expect(fieldAnchors(date, { key: 'shutoff_date', status: 'conflict', candidates: unsourced }, everywhere)?.verdict).toBe('unsourced')
    expect(fieldAnchors(date, { key: 'shutoff_date', status: 'conflict', candidates: [twelfth, unsourced[1]!] }, everywhere)?.verdict).toBe('some')
    const none = fieldAnchors(date, { key: 'shutoff_date', status: 'conflict', candidates: [candidate('10月14日', '2026-10-14', [])] }, everywhere)
    expect(none).toBeUndefined()
    expect(fieldAnchors(date, { key: 'shutoff_date', status: 'conflict' }, everywhere)).toBeUndefined()
  })
})
