// Whether the anchors of an extracted value cover the gold anchors of its
// field (acceptance README §4.4). Places are compared, not strings: `find`
// has put each gold quote in the text layer (ACC-FIND), and a result anchor
// covers it when the two share part of a block.

import type { components } from '@agent24/api-client'
import type { GoldAnchor, GoldField } from './samples'
import { type Extracted, matchedGroups } from './score'
import { l2 } from './text'

export type Anchor = components['schemas']['DocumentsAnchor']

/** Where `find` put a gold anchor's quote on its page; nowhere, when it did not find it. */
export type Locate = (anchor: GoldAnchor) => Anchor[]

export interface AnchorCover {
  gold: GoldAnchor
  /** How a result anchor covers it: at the same place, or failing a place to compare, by its quote. */
  by?: 'place' | 'quote'
  /** `find` did not locate the quote, so there was no place to compare. */
  unlocated: boolean
}

export interface AnchorScore {
  /** `unsourced`: the result gave no anchors, so nothing is covered. */
  verdict: 'all' | 'some' | 'none' | 'unsourced'
  anchors: AnchorCover[]
}

function samePlace(a: Anchor, b: Anchor): boolean {
  const [x, y] = [a.text_range, b.text_range]
  return a.text_layer_sha256 === b.text_layer_sha256 && a.block_id === b.block_id && x?.start < y?.end && y?.start < x?.end
}

function sameQuote(gold: GoldAnchor, ours: Anchor): boolean {
  const [x, y] = [l2(gold.quote), l2(typeof ours.quote === 'string' ? ours.quote : '')]
  return ours.page === gold.page && x !== '' && y !== '' && (x.includes(y) || y.includes(x))
}

function cover(gold: GoldAnchor, found: Anchor[], ours: Anchor[]): AnchorCover {
  const unlocated = found.length === 0
  // Places are comparable only within one text layer.
  const places = found.filter((f) => ours.some((r) => r.text_layer_sha256 === f.text_layer_sha256))
  if (places.length > 0) return { gold, unlocated, ...(places.some((f) => ours.some((r) => samePlace(f, r))) ? { by: 'place' } : {}) }
  return { gold, unlocated, ...(ours.some((r) => sameQuote(gold, r)) ? { by: 'quote' } : {}) }
}

/** The gold anchors of one value against the anchors the result gave for it. */
export function scoreAnchors(gold: GoldAnchor[], ours: Anchor[] | undefined, locate: Locate): AnchorScore {
  const given = (Array.isArray(ours) ? ours : []).filter((r) => typeof r === 'object' && r !== null)
  const anchors = gold.map((g) => cover(g, locate(g), given))
  return { verdict: given.length === 0 ? 'unsourced' : howMany(anchors), anchors }
}

function howMany(anchors: AnchorCover[]): 'all' | 'some' | 'none' {
  const covered = anchors.filter((a) => a.by !== undefined).length
  return covered === anchors.length ? 'all' : covered > 0 ? 'some' : 'none'
}

/** The anchors of a field, when both the gold and the result have a value: a
 *  present value, or the candidate groups of a conflict that match (§4.8).
 *  Each group is scored on its own: the evidence of one candidate says nothing
 *  for another's. */
export function fieldAnchors(gold: GoldField, result: Extracted | undefined, locate: Locate): AnchorScore | undefined {
  if (gold.status === 'present' && result?.status === 'present') return scoreAnchors(gold.anchors ?? [], result.anchors, locate)
  if (gold.status !== 'conflict' || result?.status !== 'conflict') return undefined
  const pairs = matchedGroups(gold.candidates ?? [], Array.isArray(result.candidates) ? result.candidates : [])
  if (pairs.length === 0) return undefined
  const scores = pairs.map((p) => scoreAnchors(p.gold.flatMap((c) => c.anchors), p.result.flatMap((c) => c.anchors ?? []), locate))
  const anchors = scores.flatMap((s) => s.anchors)
  return { verdict: scores.every((s) => s.verdict === 'unsourced') ? 'unsourced' : howMany(anchors), anchors }
}
