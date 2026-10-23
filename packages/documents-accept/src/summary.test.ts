import { describe, expect, it } from 'vitest'
import type { AnchorCover } from './anchors'
import type { GoldField } from './samples'
import type { Verdict } from './score'
import { type Scored, coverage, summarize } from './summary'

const QUOTE = { page: 1, quote: 'x' }
const covered: AnchorCover = { gold: QUOTE, by: 'place', unlocated: false }
const byQuote: AnchorCover = { gold: QUOTE, by: 'quote', unlocated: true }
const uncovered: AnchorCover = { gold: QUOTE, unlocated: false }
const unplaced: AnchorCover = { gold: QUOTE, unlocated: true }

function scored(key: string, verdict: Verdict, gold: Partial<GoldField> = {}, anchors?: AnchorCover[]): Scored {
  return {
    sample: 's01-07',
    gold: { key, q: 'Q1', status: 'present', severity: 'major', ...gold },
    score: { key, verdict },
    ...(anchors ? { anchors: { verdict: 'some', anchors } } : {}),
  }
}

const RUN: Scored[] = [
  scored('a', 'correct', { severity: 'critical' }, [covered, covered]),
  scored('b', 'review', { severity: 'critical' }, [covered, uncovered]),
  scored('c', 'mismatch', { severity: 'critical' }, [byQuote, unplaced]),
  scored('d', 'mismatch'),
  scored('e', 'correct', { status: 'missing', severity: 'critical' }),
  scored('f', 'fabricated', { status: 'missing', severity: 'critical' }),
  scored('g', 'unanswered', { status: 'missing', severity: 'critical' }),
  scored('h', 'correct', { status: 'conflict', severity: 'critical' }),
  scored('i', 'chose_one', { status: 'conflict', severity: 'critical' }),
  scored('j', 'review', { status: 'conflict' }),
  scored('k', 'missed', { severity: 'critical' }),
  scored('l', 'false_conflict', { severity: 'critical' }),
  scored('m', 'correct', { role: 'answer' }),
  scored('x', 'correct', { role: 'context', q: 'ctx', severity: 'critical' }, [covered]),
  scored('y', 'mismatch', { role: 'context', q: 'ctx', severity: 'critical' }),
  scored('z', 'review', { role: 'context', q: 'ctx', severity: 'critical' }),
]

describe('summarize', () => {
  const { answer, context } = summarize(RUN)

  it('counts a field for review among the fields, and not among the correct ones', () => {
    expect(answer).toMatchObject({ fields: 13, correct: 4, review: 2 })
  })

  it('counts missing and conflict fields apart', () => {
    expect(answer.missing).toEqual({ fields: 3, correct: 1 })
    expect(answer.conflict).toEqual({ fields: 3, correct: 1 })
  })

  it('counts every gold anchor of the scored values, covered by place or by quote, and the uncovered ones find could not place', () => {
    expect(answer.anchors).toEqual({ gold: 6, covered: 4, unlocated: 1 })
  })

  it('lists each critical field that is wrong with what went wrong, and the ones for review apart', () => {
    expect(answer.critical).toEqual([
      { sample: 's01-07', key: 'c', verdict: 'mismatch' },
      { sample: 's01-07', key: 'f', verdict: 'fabricated' },
      { sample: 's01-07', key: 'g', verdict: 'unanswered' },
      { sample: 's01-07', key: 'i', verdict: 'chose_one' },
      { sample: 's01-07', key: 'k', verdict: 'missed' },
      { sample: 's01-07', key: 'l', verdict: 'false_conflict' },
    ])
    expect(answer.criticalReview).toEqual([{ sample: 's01-07', key: 'b' }])
  })

  it('keeps the context fields out of all of it, with totals of their own', () => {
    expect(context).toEqual({
      fields: 3,
      correct: 1,
      review: 1,
      missing: { fields: 0, correct: 0 },
      conflict: { fields: 0, correct: 0 },
      anchors: { gold: 1, covered: 1, unlocated: 0 },
      critical: [{ sample: 's01-07', key: 'y', verdict: 'mismatch' }],
      criticalReview: [{ sample: 's01-07', key: 'z' }],
    })
  })

  it('has nothing to say of no fields', () => {
    expect(summarize([]).answer).toMatchObject({ fields: 0, correct: 0, anchors: { gold: 0, covered: 0, unlocated: 0 }, critical: [] })
  })
})

describe('coverage', () => {
  it('passes a point when every field it names is correct, and waits while one is for review', () => {
    expect(coverage('s01-07', { C1: ['a', 'e'], C4: ['a', 'b'], C6: ['a', 'b', 'c'], C9: ['f'], C10: ['h', 'j'] }, RUN)).toEqual({
      C1: 'passed',
      C4: 'review',
      C6: 'failed',
      C9: 'failed',
      C10: 'review',
    })
  })

  it('fails a point that names a field with no score', () => {
    expect(coverage('s01-07', { C2: ['a', 'nowhere'] }, RUN)).toEqual({ C2: 'failed' })
  })

  it('reads the fields of its own sample, though another sample has the same key', () => {
    const other: Scored = { ...scored('a', 'mismatch'), sample: 's01-01' }
    expect(coverage('s01-07', { C1: ['a'] }, [other, ...RUN])).toEqual({ C1: 'passed' })
    expect(coverage('s01-01', { C1: ['a'] }, [...RUN, other])).toEqual({ C1: 'failed' })
  })
})
