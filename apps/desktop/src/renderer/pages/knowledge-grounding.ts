// What a Knowledge answer may claim, from what its run's Knowledge tools did
// (`GET /api/v1/runs/{id}` `knowledge`), never from the answer's own text: a
// model has answered without searching and closed with sources it made up
// (decision 2026-10-31-knowledge-grounded-sources).

import type { components } from '@agent24/api-client'

export type RunKnowledge = components['schemas']['RunKnowledge']

/** `read`: the run read chunks back, so the answer is labelled as answered
 * with Knowledge. `unread`: each `knowledge_id#chunk_id` the answer names
 * that the run did not read back, so it is not shown as a source. */
export type Grounding =
  | { kind: 'notSearched'; unread: string[] }
  | { kind: 'nothingRead'; unread: string[] }
  | { kind: 'read'; unread: string[] }

// Anything joined by `#`, as the tools ask sources to be written. Broad on
// purpose: a made-up source left unmarked is worse than a URL's fragment
// marked as not read. The right side stops short of `.` and `:`, which end a
// sentence or a label.
const CITED = /[\w.:+=~@%-]+#[\w+=~@%-]+/g
const escape = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')

/** Each `x#y` in `text`, once, in order. */
export function citedChunks(text: string): string[] {
  return [...new Set(text.match(CITED) ?? [])]
}

/** `knowledge` null: the run had no Knowledge binding, so searched nothing. */
export function grounding(text: string, knowledge: RunKnowledge | null): Grounding {
  const read = (knowledge?.read_chunks ?? []).map((c) => `${c.knowledge_id}#${c.chunk_id}`)
  // The sources the run read back stand wherever they are written ("来源:"
  // right before one, say); every other `x#y` left is not a source.
  const rest = read.reduce((rest, id) => rest.replace(new RegExp(`${escape(id)}(?![\\w+=~@%-])`, 'g'), ' '), text)
  const unread = citedChunks(rest)
  if (!knowledge?.searched) return { kind: 'notSearched', unread }
  return { kind: read.length > 0 ? 'read' : 'nothingRead', unread }
}
