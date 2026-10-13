// The numbers of a run (acceptance README §4.5, §4.8, and the coverage points
// of §3), from the mechanical verdicts. What a person decides in review is
// merged in elsewhere (§4.7).

import type { AnchorScore } from './anchors'
import type { GoldField } from './samples'
import type { FieldScore, Verdict } from './score'

/** One gold field of one sample, scored. */
export interface Scored {
  sample: string
  gold: GoldField
  score: FieldScore
  /** Present when both the gold and the result have a value. */
  anchors?: AnchorScore
}

interface Named {
  sample: string
  key: string
}

export interface Totals {
  fields: number
  correct: number
  /** For a person: in the fields, not yet in the correct ones. */
  review: number
  missing: { fields: number; correct: number }
  conflict: { fields: number; correct: number }
  /** Gold anchors of the fields with a value on both sides; `unlocated` of the uncovered ones `find` could not place. */
  anchors: { gold: number; covered: number; unlocated: number }
  /** Critical fields that are wrong, each with what went wrong. */
  critical: (Named & { verdict: Verdict })[]
  /** Critical fields a person has yet to decide: to be looked at first. */
  criticalReview: Named[]
}

function totals(scored: Scored[]): Totals {
  const of = (status: GoldField['status']) => {
    const these = scored.filter((s) => s.gold.status === status)
    return { fields: these.length, correct: these.filter((s) => s.score.verdict === 'correct').length }
  }
  const anchors = scored.flatMap((s) => s.anchors?.anchors ?? [])
  const critical = scored.filter((s) => s.gold.severity === 'critical')
  const named = (s: Scored): Named => ({ sample: s.sample, key: s.gold.key })
  return {
    fields: scored.length,
    correct: scored.filter((s) => s.score.verdict === 'correct').length,
    review: scored.filter((s) => s.score.verdict === 'review').length,
    missing: of('missing'),
    conflict: of('conflict'),
    anchors: {
      gold: anchors.length,
      covered: anchors.filter((a) => a.by !== undefined).length,
      unlocated: anchors.filter((a) => a.by === undefined && a.unlocated).length,
    },
    critical: critical
      .filter((s) => s.score.verdict !== 'correct' && s.score.verdict !== 'review')
      .map((s) => ({ ...named(s), verdict: s.score.verdict })),
    criticalReview: critical.filter((s) => s.score.verdict === 'review').map(named),
  }
}

/** The totals of the fields a reader must have answered, and apart from them, of the context fields. */
export function summarize(scored: Scored[]): { answer: Totals; context: Totals } {
  const context = (s: Scored) => s.gold.role === 'context'
  return { answer: totals(scored.filter((s) => !context(s))), context: totals(scored.filter(context)) }
}

/** The coverage points of one sample (samples README §2): passed when every
 *  field a point names is correct, for review while one of them waits for a
 *  person. Samples share field keys, so only this sample's fields are read. */
export function coverage(sample: string, covers: Record<string, string[]>, scored: Scored[]): Record<string, 'passed' | 'review' | 'failed'> {
  const verdict = (key: string) => scored.find((s) => s.sample === sample && s.gold.key === key)?.score.verdict
  return Object.fromEntries(
    Object.entries(covers).map(([point, keys]) => {
      const verdicts = keys.map(verdict)
      const state = verdicts.every((v) => v === 'correct') ? 'passed' : verdicts.every((v) => v === 'correct' || v === 'review') ? 'review' : 'failed'
      return [point, state]
    }),
  )
}
