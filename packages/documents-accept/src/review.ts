// What a person decided (acceptance README §4.7), merged into a report. The
// mechanical verdicts stay where they are; the person's stand beside them,
// and the totals are counted once more with them.

import type { AnchorCover } from './anchors'
import type { Decided, Report, SampleReport } from './report'
import type { Verdict } from './score'
import { isDeepStrictEqual } from 'node:util'
import { type Scored, type Totals, coverage, summarize } from './summary'

/** One decision of `review.json`. It is about one extraction: with another
 *  `extraction_id` it no longer applies. */
export interface ReviewItem {
  sample: string
  extraction_id: string
  /** The key of a field: its verdict, or with `anchor`, whether that gold anchor (from 1) is covered. */
  field?: string
  anchor?: number
  /** The number of a `must_not_assert` proposition, from 1. */
  must_not_assert?: number
  verdict: string
  note?: string
}

export interface Review {
  reviewer: string
  items: ReviewItem[]
}

export interface Reviewed {
  reviewer: string
  applied: number
  /** Decisions that changed nothing, each with why: for the reviewer to look at again. */
  unused: { item: ReviewItem; why: string }[]
  /** The totals with the person's verdicts in place of the mechanical ones. */
  totals: { answer: Totals; context: Totals }
  /** The coverage points of every sample, by the person's verdicts where there are any. */
  coverage: Record<string, SampleReport['coverage']>
  must_not_assert: { violated: number; unreviewed: number }
}

const isText = (x: unknown): x is string => typeof x === 'string' && x.trim() !== ''
const isNumber = (x: unknown): boolean => x === undefined || (typeof x === 'number' && Number.isInteger(x) && x >= 1)

/** A `review.json` as a person wrote it, or an error that says what is wrong with it. */
export function readReview(json: unknown): Review {
  const review = json as Partial<Review> | null
  if (typeof review !== 'object' || review === null || !isText(review.reviewer) || !Array.isArray(review.items)) throw new Error('review.json must have a reviewer and items')
  review.items.forEach((item: Partial<ReviewItem> | null, i) => {
    const texts = item !== null && typeof item === 'object' && isText(item.sample) && isText(item.extraction_id) && isText(item.verdict)
    if (!texts || !(item.field === undefined || isText(item.field)) || !(item.note === undefined || typeof item.note === 'string') || !isNumber(item.anchor) || !isNumber(item.must_not_assert)) {
      throw new Error(`review.json: item ${i + 1} is not a decision (sample, extraction_id, verdict, and a field or a must_not_assert)`)
    }
    // What is written here is written into the report: nothing else may ride along.
    const more = Object.keys(item).filter((k) => !MEMBERS.includes(k))
    if (more.length > 0) throw new Error(`review.json: item ${i + 1} has what a decision does not have: ${more.join(', ')}`)
  })
  if (Object.keys(review).some((k) => k !== 'reviewer' && k !== 'items')) throw new Error('review.json must have a reviewer and items, and nothing else')
  return review as Review
}

const MEMBERS = ['sample', 'extraction_id', 'field', 'anchor', 'must_not_assert', 'verdict', 'note']
/** What a person may find of a field, by what the gold says of it (§4.2): the other verdicts are of no such field. */
const FIELD: Record<SampleReport['fields'][number]['gold']['status'], Verdict[]> = {
  present: ['correct', 'mismatch', 'missed', 'false_conflict'],
  missing: ['correct', 'fabricated', 'unanswered'],
  conflict: ['correct', 'mismatch', 'missed', 'chose_one'],
}
const ANCHOR = ['covered', 'uncovered']
const ASSERTED = ['violated', 'not_violated']

/** Where an item belongs in a sample's report: a setter, or why it belongs nowhere. */
function place(sample: SampleReport, item: ReviewItem, reviewer: string): (() => void) | string {
  const decided = <V extends string>(verdict: string): Decided<V> => ({ verdict: verdict as V, reviewer, ...(item.note === undefined ? {} : { note: item.note }) })
  const kinds = [item.field !== undefined, item.must_not_assert !== undefined].filter(Boolean).length
  if (kinds !== 1 || (item.anchor !== undefined && item.field === undefined)) return 'it names neither one field nor one must_not_assert'
  if (item.must_not_assert !== undefined) {
    const proposition = sample.must_not_assert.find((m) => m.n === item.must_not_assert)
    if (!proposition) return `the sample has no must_not_assert ${item.must_not_assert}`
    if (!ASSERTED.includes(item.verdict)) return `${item.verdict} is no verdict on a must_not_assert`
    return () => void (proposition.review = decided(item.verdict))
  }
  const field = sample.fields.find((f) => f.key === item.field)
  if (!field) return `the sample has no field ${item.field}`
  if (item.anchor === undefined) {
    if (!(FIELD[field.gold.status] as string[]).includes(item.verdict)) return `${item.verdict} is no verdict on a field the gold has as ${field.gold.status}`
    return () => void (field.review = decided(item.verdict))
  }
  const anchor = field.anchors?.anchors[item.anchor - 1]
  if (!anchor) return `the field ${item.field} has no scored anchor ${item.anchor}`
  if (!ANCHOR.includes(item.verdict)) return `${item.verdict} is no verdict on an anchor`
  return () => void (anchor.review = decided(item.verdict))
}

/** The fields of a sample as the totals count them, a person's verdict before the mechanical one. */
function reviewedFields(s: SampleReport): Scored[] {
  return s.fields.map((f): Scored => {
    const anchors = f.anchors?.anchors.map((a): AnchorCover => {
      if (a.review === undefined) return a
      const { by: _, review: __, ...rest } = a
      // An anchor a person found covered is covered, and is no longer one that could not be placed.
      return a.review.verdict === 'covered' ? { ...rest, by: a.by ?? 'quote' } : rest
    })
    return {
      sample: s.sample,
      gold: { key: f.key, q: '', status: f.gold.status, severity: f.severity, role: f.role },
      score: { ...f.score, verdict: f.review?.verdict ?? f.score.verdict },
      ...(f.anchors && anchors ? { anchors: { ...f.anchors, anchors } } : {}),
    }
  })
}

/** A report without a review, nor its marks: as the harness wrote it. */
function unreviewed(report: Report & { review?: Reviewed }): Report {
  const { review: _, ...plain } = structuredClone(report)
  for (const sample of plain.samples) {
    for (const marked of [...sample.fields, ...sample.fields.flatMap((f) => f.anchors?.anchors ?? []), ...sample.must_not_assert]) delete marked.review
  }
  return plain
}

const NOT_A_REPORT = 'report.json is not a report of this harness (documents-accept-report-v1)'

/** A `report.json` read back as the harness wrote it, or an error: it must be
 *  a report of this harness, and its totals and coverage points must still
 *  follow from its fields. A review merged into it is left out, marks and
 *  numbers: what a person decided is merged again from `review.json` (§4.7),
 *  so the numbers of a review are only ever counted, never read. That finds a
 *  file of another shape or one edited in one place; it is no seal: the
 *  evidence is in git, and a change to it is reviewed there. */
export function readReport(json: unknown): Report {
  const report = json as Report | null
  const covers = (c: unknown) => typeof c === 'object' && c !== null && Object.values(c).every((keys) => Array.isArray(keys) && keys.every((k) => typeof k === 'string'))
  const shaped = typeof report === 'object' && report !== null && report.schema === 'documents-accept-report-v1' && Array.isArray(report.samples) && report.samples.every((s) => typeof s === 'object' && s !== null && Array.isArray(s.fields) && s.fields.every((f) => f?.role === 'answer' || f?.role === 'context') && Array.isArray(s.must_not_assert) && covers(s.covers))
  if (!shaped) throw new Error(NOT_A_REPORT)
  let plain: Report
  let follows: boolean
  try {
    plain = unreviewed(report)
    const fields = plain.samples.flatMap(reviewedFields)
    follows = isDeepStrictEqual(summarize(fields), plain.totals) && plain.samples.every((s) => isDeepStrictEqual(coverage(s.sample, s.covers, fields), s.coverage))
  } catch (error) {
    // A field or a mark of a shape the harness does not write; anything else is no fault of the file.
    if (error instanceof TypeError) throw new Error(NOT_A_REPORT, { cause: error })
    throw error
  }
  if (!follows) throw new Error('report.json: its totals or coverage points do not follow from its fields; it was changed after it was written')
  return plain
}

/** The report with these decisions merged in, and no earlier ones: merging
 *  again starts from the mechanical verdicts. A decision that fits nothing in
 *  it, or is made twice, changes nothing and is listed. */
export function applyReview(report: Report & { review?: Reviewed }, review: Review): Report & { review: Reviewed } {
  const merged = unreviewed(report)
  const unused: Reviewed['unused'] = []
  const seen = new Set<string>()
  let applied = 0
  for (const item of review.items) {
    const sample = merged.samples.find((s) => s.sample === item.sample)
    const at = JSON.stringify([item.sample, item.field, item.anchor, item.must_not_assert])
    // The second decision on one thing does not count, whatever became of the first (§4.7).
    const twice = seen.has(at)
    const set = !sample ? `the report has no sample ${item.sample}` : sample.extraction_id !== item.extraction_id ? `it is about extraction ${item.extraction_id}, and the report is of ${sample.extraction_id ?? 'none'}` : twice ? 'it is decided twice' : place(sample, item, review.reviewer)
    if (sample?.extraction_id === item.extraction_id) seen.add(at)
    if (typeof set === 'string') {
      unused.push({ item, why: set })
      continue
    }
    set()
    applied += 1
  }
  const propositions = merged.samples.flatMap((s) => s.must_not_assert)
  const must_not_assert = { violated: propositions.filter((m) => m.review?.verdict === 'violated').length, unreviewed: propositions.filter((m) => m.review === undefined).length }
  const fields = merged.samples.flatMap(reviewedFields)
  const covered = Object.fromEntries(merged.samples.map((s) => [s.sample, coverage(s.sample, s.covers, fields)]))
  return { ...merged, review: { reviewer: review.reviewer, applied, unused, totals: summarize(fields), coverage: covered, must_not_assert } }
}
