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

// Two IDs joined by `#`, as the tools ask sources to be written; an ID is a
// run of these characters, so `Sources:`, `~~` or a full stop around one is
// not part of it, and `a#b+c#d` is two sources. Broad on purpose: a made-up
// source left unmarked is worse than a URL's fragment marked as not read.
// WeKnora's IDs are UUIDs; an ID with other characters would be split and
// marked, never passed over.
const CITED = /[A-Za-z0-9_-]+#[A-Za-z0-9_-]+/g

/** Each `knowledge_id#chunk_id` in `text`, once, in order. */
export function citedChunks(text: string): string[] {
  return [...new Set(text.match(CITED) ?? [])]
}

/** `knowledge` null: the run had no Knowledge binding, so searched nothing. */
export function grounding(text: string, knowledge: RunKnowledge | null): Grounding {
  const read = new Set((knowledge?.read_chunks ?? []).map((c) => `${c.knowledge_id}#${c.chunk_id}`))
  const unread = citedChunks(text).filter((id) => !read.has(id))
  if (!knowledge?.searched) return { kind: 'notSearched', unread }
  return { kind: read.size > 0 ? 'read' : 'nothingRead', unread }
}
