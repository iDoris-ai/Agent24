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
// WeKnora's IDs are UUIDs, made only of these characters, so a changed one
// is another string and is marked; but text joined to a source read back by
// another character (`x.K#C`, `K#C.5`) is dropped, not checked.
const CITED = /[A-Za-z0-9_-]+#[A-Za-z0-9_-]+/g

/** Each `knowledge_id#chunk_id` in `text`, once, in order. */
export function citedChunks(text: string): string[] {
  return [...new Set(text.match(CITED) ?? [])]
}

/** `knowledge` null: the run had no Knowledge binding, so searched nothing. */
export function grounding(text: string, knowledge: RunKnowledge | null): Grounding {
  const chunks = knowledge?.read_chunks ?? []
  const read = new Set(chunks.map((c) => `${c.knowledge_id}#${c.chunk_id}`))
  // K3 (11-19): a small model writes a source its own way, `file.pdf#<document id>`
  // (qwen3-8b: `s01-02-en-epa-boil-water.pdf#17b1f6d6-…`, read as `pdf#17b1f6d6-…`).
  // After `#`, the ID of a document the run read stands for that document.
  // A chunk ID after another document's ID is still another source.
  const documents = new Set(chunks.map((c) => c.knowledge_id))
  const known = (cited: string) => read.has(cited) || documents.has(cited.slice(cited.indexOf('#') + 1))
  const unread = citedChunks(text).filter((id) => !known(id))
  if (!knowledge?.searched) return { kind: 'notSearched', unread }
  return { kind: read.size > 0 ? 'read' : 'nothingRead', unread }
}

/** L7-K0: what a run's searches came to, as the daemon's record says it
 * (`GET /runs/{id}` `knowledge`), for the line under an answer. Never says
 * which passage the answer used: only what was found. null: no search to
 * speak of (none asked, or only passages read back). */
export type KnowledgeFacts =
  | { kind: 'refused'; reason: 'policy' | 'failed' }
  | { kind: 'none' }
  | { kind: 'found'; passages: number; titles: string[] }

/** The record's L7-K0 fields, optional so a daemon without them reads as
 * 「nothing to say」 (they arrive with the daemon's `search_refused`). */
type RunKnowledgeFacts = RunKnowledge & { search_refused?: boolean; search_refusal?: string; found_passages?: number; found_titles?: string[] }

export function knowledgeFacts(knowledge: RunKnowledgeFacts | null | undefined): KnowledgeFacts | null {
  if (!knowledge) return null
  if (knowledge.search_refused === true) return { kind: 'refused', reason: knowledge.search_refusal === 'policy' ? 'policy' : 'failed' }
  if (!knowledge.searched) return null
  const passages = typeof knowledge.found_passages === 'number' ? knowledge.found_passages : 0
  if (passages > 0) {
    const titles = (Array.isArray(knowledge.found_titles) ? knowledge.found_titles : []).filter((t) => typeof t === 'string' && t !== '')
    return { kind: 'found', passages, titles }
  }
  // A read with no search is not 「找不到」.
  return knowledge.read_chunks.length === 0 ? { kind: 'none' } : null
}
