// How one extracted value is judged against its gold field: the status table
// and the comparison of values (acceptance README §4.2, §4.3). Anchors are
// judged apart from this (§4.4).

import type { components } from '@agent24/api-client'
import type { GoldCandidate, GoldField } from './samples'
import { l2 } from './text'

export type Extracted = components['schemas']['DocumentsExtractedValue']

/** The mechanical verdict on a field. Only `correct` counts as right, and
 *  `review` is a person's to settle (§4.7); the others name what went wrong. */
export type Verdict = 'correct' | 'review' | 'mismatch' | 'missed' | 'fabricated' | 'chose_one' | 'false_conflict' | 'unanswered'

/** Why the candidates of a conflict are for review. */
export type CandidateNote = 'incomplete' | 'extra' | 'normalized_only'

export interface FieldScore {
  key: string
  verdict: Verdict
  candidates?: CandidateNote[]
  /** When both sides say missing: whether their reasons agree. Recorded, never judged. */
  reasonAgrees?: boolean
}

interface Said {
  value?: string
  normalized?: string
}

const STATUSES: unknown[] = ['present', 'missing', 'conflict']

/** A result comes over HTTP: what should be text and is not counts as no text. */
const text = (x: unknown): string => (typeof x === 'string' ? x : '')

/** One of the two holds the other, as L2 text; an empty one holds nothing. */
function overlaps(a: string | undefined, b: string | undefined): boolean {
  const [x, y] = [l2(text(a)), l2(text(b))]
  return x !== '' && y !== '' && (x.includes(y) || y.includes(x))
}

/** A normalized form is the model's own word for its value, so alone it settles nothing. */
function sameForm(a: string | undefined, b: string | undefined): boolean {
  return text(a).trim() !== '' && text(a).trim() === text(b).trim()
}

/** Two values compared (§4.3). */
export function compareValues(gold: Said, result: Said): 'same' | 'review' | 'different' {
  const overlap = overlaps(gold.value, result.value)
  const form = sameForm(gold.normalized, result.normalized)
  if (overlap && (form || l2(text(gold.value)) === l2(text(result.value)))) return 'same'
  return overlap || form ? 'review' : 'different'
}

function groups<T extends Said>(said: T[]): Map<string, T[]> {
  const by = new Map<string, T[]>()
  for (const c of said) {
    // What is not an object says nothing: it is a candidate of no form and no value.
    const candidate = typeof c === 'object' && c !== null ? c : ({} as T)
    const form = text(candidate.normalized).trim()
    by.set(form, [...(by.get(form) ?? []), candidate])
  }
  return by
}

/** The candidates of a conflict, grouped by normalized form (§4.3). A gold
 *  group and the result's group of the same form match when a candidate of one
 *  holds, or is held by, a candidate of the other. */
export function matchedGroups<T extends Said>(gold: GoldCandidate[], result: T[]): { gold: GoldCandidate[]; result: T[] }[] {
  const got = groups(result)
  return [...groups(gold)].flatMap(([form, golds]) => {
    const ours = got.get(form) ?? []
    return golds.some((g) => ours.some((r) => overlaps(g.value, r.value))) ? [{ gold: golds, result: ours }] : []
  })
}

/** What keeps the candidates of the two sides from matching (§4.3), or nothing when they match. */
export function compareCandidates(gold: GoldCandidate[], result: Said[]): CandidateNote[] {
  const [wanted, got] = [groups(gold), groups(result)]
  const matched = matchedGroups(gold, result).length
  const notes: CandidateNote[] = []
  if ([...wanted.keys()].some((form) => !got.has(form))) notes.push('incomplete')
  if ([...wanted.keys()].filter((form) => got.has(form)).length > matched) notes.push('normalized_only')
  if ([...got.keys()].some((form) => !wanted.has(form))) notes.push('extra')
  return notes
}

/** `unread` stands for "not in the document" when part of it was not read (ADR-DOC-02 §3.2). */
function reasonsAgree(gold: string | undefined, result: string | undefined, partial: boolean): boolean {
  const absent = gold === 'not_in_document' || gold === 'referenced_but_absent'
  return gold === result || (partial && absent && result === 'unread')
}

/** One gold field against the value extracted for its key, if any (§4.2).
 *  `partial`: the text layer had regions that were not read. */
export function scoreField(gold: GoldField, answer: Extracted | null | undefined, partial = false): FieldScore {
  const key = gold.key
  // An answer of no status the contract has is no answer: its value is not looked at.
  const result = typeof answer === 'object' && answer !== null && STATUSES.includes(answer.status) ? answer : undefined
  if (gold.status === 'missing') {
    if (result === undefined) return { key, verdict: 'unanswered' }
    if (result.status !== 'missing') return { key, verdict: 'fabricated' }
    return { key, verdict: 'correct', reasonAgrees: reasonsAgree(gold.missing_reason, result.missing_reason, partial) }
  }
  if (result === undefined || result.status === 'missing') return { key, verdict: 'missed' }
  if (gold.status === 'conflict') {
    if (result.status === 'present') return { key, verdict: 'chose_one' }
    const candidates = compareCandidates(gold.candidates ?? [], Array.isArray(result.candidates) ? result.candidates : [])
    return candidates.length === 0 ? { key, verdict: 'correct' } : { key, verdict: 'review', candidates }
  }
  if (result.status === 'conflict') return { key, verdict: 'false_conflict' }
  const values = compareValues(gold, result)
  return { key, verdict: values === 'same' ? 'correct' : values === 'review' ? 'review' : 'mismatch' }
}
