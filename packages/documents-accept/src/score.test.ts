import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { type GoldField, loadSample } from './samples'
import { type Extracted, compareCandidates, compareValues, scoreField } from './score'

const S01 = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '..', 'docs', 'documenting', 'samples', 's01')
const gold = (sample: string, key: string): GoldField => {
  const f = loadSample(path.join(S01, sample)).gold.fields.find((x) => x.key === key)
  if (f === undefined) throw new Error(`no gold field ${key}`)
  return f
}
const NOTICE = 's01-07-mixed-building-notice'
const HOLIDAYS = 's01-03-zh-holiday-2026'

const present = (key: string, value: string, normalized?: string): Extracted => ({ key, status: 'present', value, normalized, anchors: [] })
const missing = (key: string, missing_reason: Extracted['missing_reason'] = 'not_in_document'): Extracted => ({ key, status: 'missing', missing_reason })
const conflict = (key: string, ...said: [string, string][]): Extracted => ({
  key,
  status: 'conflict',
  candidates: said.map(([value, normalized]) => ({ value, normalized, anchors: [] })),
})

describe('compareValues', () => {
  it('finds the same text the same, whatever the spacing or the width of its characters', () => {
    expect(compareValues({ value: '沪财税〔2025〕145号' }, { value: '沪财税〔2025〕145 号' })).toBe('same')
    expect(compareValues({ value: '9:00至18:00' }, { value: '９：００至１８：００' })).toBe('same')
    expect(compareValues({ value: 'WO-000731' }, { value: 'WO-000731' })).toBe('same')
  })

  it('finds a shorter span of the gold value the same only with the same normalized form', () => {
    const makeup = { value: '2月14日（周六）、2月28日（周六）上班', normalized: '2026-02-14,2026-02-28' }
    expect(compareValues(makeup, { value: '2月14日（周六）、2月28日（周六）', normalized: '2026-02-14,2026-02-28' })).toBe('same')
    expect(compareValues(makeup, { value: '2月14日（周六）、2月28日（周六）', normalized: '' })).toBe('review')
    expect(compareValues(makeup, { value: '2月14日（周六）', normalized: '2026-02-14' })).toBe('review')
    // The longer side may be the result as well.
    expect(compareValues({ value: '20元/证' }, { value: '到期换发20元/证' })).toBe('review')
  })

  it('does not take the same normalized form alone for the same value', () => {
    // The right date, quoted from the sentence about another event.
    const lift = { value: '10月12日', normalized: '2026-10-12' }
    expect(compareValues(lift, { value: 'Monday, 12 October 2026', normalized: '2026-10-12' })).toBe('review')
    expect(compareValues(lift, { value: '2026年10月12日（周一）', normalized: '2026-10-12' })).toBe('same')
  })

  it('finds anything else different, an empty value included', () => {
    expect(compareValues({ value: 'WO-000731' }, { value: 'WO-731' })).toBe('different')
    expect(compareValues({ value: '10月12日', normalized: '2026-10-12' }, { value: '10月13日', normalized: '2026-10-13' })).toBe('different')
    expect(compareValues({ value: 'WO-000731' }, { value: '' })).toBe('different')
    expect(compareValues({ value: 'WO-000731' }, {})).toBe('different')
    expect(compareValues({ value: 'WO-000731', normalized: '' }, { value: 'FM-2026-0042', normalized: '' })).toBe('different')
  })
})

describe('compareCandidates', () => {
  // Three statements of the date, two of them the 12th: two groups.
  const dates = gold(NOTICE, 'shutoff_date').candidates ?? []

  it('matches when every gold group is there, and nothing else', () => {
    expect(dates.map((c) => c.normalized)).toEqual(['2026-10-12', '2026-10-13', '2026-10-12'])
    expect(compareCandidates(dates, [{ value: '10月12日', normalized: '2026-10-12' }, { value: '10月13日', normalized: '2026-10-13' }])).toEqual([])
    // One candidate of a group is enough for the group, in either wording.
    expect(compareCandidates(dates, [{ value: '12 October 2026', normalized: '2026-10-12' }, { value: '10月13日', normalized: '2026-10-13' }])).toEqual([])
  })

  it('says which way the sets differ', () => {
    const twelfth = { value: '10月12日', normalized: '2026-10-12' }
    const thirteenth = { value: '10月13日', normalized: '2026-10-13' }
    expect(compareCandidates(dates, [twelfth, { ...twelfth, value: '12 October' }])).toEqual(['incomplete'])
    expect(compareCandidates(dates, [twelfth, thirteenth, { value: '10月14日', normalized: '2026-10-14' }])).toEqual(['extra'])
    expect(compareCandidates(dates, [twelfth, { value: '10月14日', normalized: '2026-10-14' }])).toEqual(['incomplete', 'extra'])
    expect(compareCandidates(dates, [])).toEqual(['incomplete'])
  })

  it('does not match a group on its normalized form alone', () => {
    const quotedElsewhere = { value: '同日', normalized: '2026-10-12' }
    expect(compareCandidates(dates, [quotedElsewhere, { value: '10月13日', normalized: '2026-10-13' }])).toEqual(['normalized_only'])
  })

  it('matches no gold group with a candidate that has no normalized form, and ignores space around one', () => {
    expect(compareCandidates(dates, [{ value: '10月12日', normalized: '2026-10-12' }, { value: '10 月 13 日' }])).toEqual(['incomplete', 'extra'])
    expect(compareCandidates(dates, [{ value: '10月12日', normalized: '2026-10-12' }, { value: '10月13日', normalized: ' 2026-10-13 ' }])).toEqual([])
  })
})

describe('scoreField: the status table', () => {
  const order = gold(NOTICE, 'work_order') // present
  const phone = gold(NOTICE, 'contact_phone') // missing, not in the document
  const date = gold(NOTICE, 'shutoff_date') // conflict
  const verdict = (g: GoldField, r: Extracted | null | undefined) => scoreField(g, r).verdict

  it('a present gold field', () => {
    expect(verdict(order, present('work_order', 'WO-000731'))).toBe('correct')
    expect(verdict(order, present('work_order', '工单号 Work Order: WO-000731'))).toBe('review')
    expect(verdict(order, present('work_order', 'WO-731'))).toBe('mismatch')
    expect(verdict(order, missing('work_order'))).toBe('missed')
    expect(verdict(order, conflict('work_order', ['WO-000731', 'WO-000731'], ['FM-2026-0042', 'FM-2026-0042']))).toBe('false_conflict')
    expect(verdict(order, undefined)).toBe('missed')
  })

  it('a missing gold field: any value is made up, whatever it says', () => {
    expect(verdict(phone, missing('contact_phone'))).toBe('correct')
    expect(verdict(phone, present('contact_phone', '物业服务中心'))).toBe('fabricated')
    expect(verdict(phone, conflict('contact_phone', ['021-1', '021-1'], ['021-2', '021-2']))).toBe('fabricated')
    expect(verdict(phone, undefined)).toBe('unanswered')
  })

  it('a conflict in the gold: one value is a choice nobody asked for, even a candidate', () => {
    expect(verdict(date, conflict('shutoff_date', ['10月12日', '2026-10-12'], ['10月13日', '2026-10-13']))).toBe('correct')
    expect(verdict(date, present('shutoff_date', '2026年10月12日（周一）', '2026-10-12'))).toBe('chose_one')
    expect(verdict(date, missing('shutoff_date'))).toBe('missed')
    expect(verdict(date, undefined)).toBe('missed')
    expect(scoreField(date, conflict('shutoff_date', ['10月12日', '2026-10-12'], ['10月14日', '2026-10-14']))).toEqual({
      key: 'shutoff_date',
      verdict: 'review',
      candidates: ['incomplete', 'extra'],
    })
  })

  it('an answer whose status the contract does not have is no answer, whatever else it says', () => {
    const right = { key: 'work_order', value: order.value, anchors: [] }
    expect(verdict(order, { ...right, status: 'absent' } as never)).toBe('missed')
    expect(verdict(order, right as never)).toBe('missed')
    expect(verdict(date, { key: 'shutoff_date', status: 'absent', candidates: date.candidates } as never)).toBe('missed')
    expect(verdict(date, { key: 'shutoff_date', candidates: date.candidates } as never)).toBe('missed')
    expect(verdict(phone, { key: 'contact_phone', status: 'absent' } as never)).toBe('unanswered')
    expect(verdict(phone, { key: 'contact_phone', missing_reason: 'not_in_document' } as never)).toBe('unanswered')
    expect(verdict(order, null)).toBe('missed')
    expect(verdict(phone, null)).toBe('unanswered')
    expect(verdict(order, 'present' as never)).toBe('missed')
  })

  it('a result that breaks the contract is never correct, and never a crash', () => {
    expect(verdict(order, { key: 'work_order', status: 'present' })).toBe('mismatch')
    expect(verdict(order, { key: 'work_order', status: 'present', value: 731 } as never)).toBe('mismatch')
    expect(verdict(order, { key: 'work_order', status: 'present', value: 'WO-731', normalized: 731 } as never)).toBe('mismatch')
    expect(verdict(date, { key: 'shutoff_date', status: 'conflict' })).toBe('review')
    expect(verdict(date, { key: 'shutoff_date', status: 'conflict', candidates: [{ value: 12, normalized: 12 }, null] } as never)).toBe('review')
    expect(verdict(date, { key: 'shutoff_date', status: 'conflict', candidates: {} } as never)).toBe('review')
    expect(verdict(date, { key: 'shutoff_date', status: 'conflict', candidates: '2026-10-12' } as never)).toBe('review')
  })

  it('a candidate that is not an object matches no gold group, even one whose form is blank', () => {
    const blank = [{ value: '10月12日', normalized: ' ', anchors: [] }, { value: '10月13日', normalized: '2026-10-13', anchors: [] }]
    expect(compareCandidates(blank, [null, { value: '10月13日', normalized: '2026-10-13' }] as never)).toEqual(['normalized_only'])
    expect(compareCandidates(date.candidates!, [null, 7, 'x'] as never)).toEqual(['incomplete', 'extra'])
  })
})

describe('scoreField: the reason a field is missing', () => {
  const phone = gold(NOTICE, 'contact_phone') // not_in_document
  const agrees = (g: GoldField, reason: Extracted['missing_reason'], partial?: boolean) => scoreField(g, missing(g.key, reason), partial)

  it('is recorded and never changes the verdict', () => {
    expect(agrees(phone, 'not_in_document')).toEqual({ key: 'contact_phone', verdict: 'correct', reasonAgrees: true })
    expect(agrees(phone, 'blank_in_template')).toEqual({ key: 'contact_phone', verdict: 'correct', reasonAgrees: false })
  })

  it('may be unread for something not in the document, only when part of it was not read', () => {
    expect(agrees(phone, 'unread', true).reasonAgrees).toBe(true)
    expect(agrees(phone, 'unread').reasonAgrees).toBe(false)
    const blank = gold('s01-02-en-epa-boil-water', 'expected_resolution') // blank_in_template
    expect(agrees(blank, 'unread', true).reasonAgrees).toBe(false)
    const referenced = gold('s01-04-en-govuk-sa-deadlines', 'penalty_amount') // referenced_but_absent
    expect(agrees(referenced, 'unread', true).reasonAgrees).toBe(true)
  })
})

describe('scoreField on the gold itself', () => {
  // A smoke test: it shows that a real gold field scores at all, not that the comparison is right.
  it('finds every gold field correct when the result says exactly what the gold says', () => {
    for (const sample of [NOTICE, HOLIDAYS]) {
      for (const g of loadSample(path.join(S01, sample)).gold.fields) {
        const echo: Extracted =
          g.status === 'present' ? present(g.key, g.value ?? '', g.normalized)
          : g.status === 'missing' ? missing(g.key, g.missing_reason as Extracted['missing_reason'])
          : conflict(g.key, ...(g.candidates ?? []).map((c): [string, string] => [c.value, c.normalized]))
        expect(scoreField(g, echo).verdict, `${sample} ${g.key}`).toBe('correct')
      }
    }
  })
})
