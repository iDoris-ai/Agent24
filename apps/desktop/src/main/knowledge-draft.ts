// KB-C2-04b-2 (#1624): the conversation's draft attachments. Only a confirmed
// return_to_chat_intent adds to this (U7.2). Nothing here is sent on its own.

import { randomBytes } from 'node:crypto'

export const DRAFT_LIMIT = 16

export interface DraftItem {
  title: string | null
  text: string
  ref: { knowledge_base_id: string; knowledge_id: string; chunk_id: string }
}

/** `id` is 16 random bytes, base64url, 22 characters. */
export interface DraftEntry extends DraftItem {
  id: string
}

function copy(entry: DraftEntry): DraftEntry {
  return {
    id: entry.id,
    title: entry.title,
    text: entry.text,
    ref: {
      knowledge_base_id: entry.ref.knowledge_base_id,
      knowledge_id: entry.ref.knowledge_id,
      chunk_id: entry.ref.chunk_id,
    },
  }
}

/** All or nothing, at most {@link DRAFT_LIMIT}. Every change calls `onChange` with the full list. */
export class KnowledgeDraft {
  private entries: DraftEntry[] = []

  constructor(private readonly onChange: (entries: readonly DraftEntry[]) => void) {}

  room(): number {
    return DRAFT_LIMIT - this.entries.length
  }

  /** False when `items` would pass 16. Nothing is added in that case. */
  add(items: DraftItem[]): boolean {
    if (items.length > this.room()) return false
    if (items.length === 0) return true
    this.entries = [
      ...this.entries,
      ...items.map((item) => ({
        id: randomBytes(16).toString('base64url'),
        title: item.title,
        text: item.text,
        ref: {
          knowledge_base_id: item.ref.knowledge_base_id,
          knowledge_id: item.ref.knowledge_id,
          chunk_id: item.ref.chunk_id,
        },
      })),
    ]
    this.emit()
    return true
  }

  list(): readonly DraftEntry[] {
    return this.entries.map(copy)
  }

  remove(id: string): void {
    const next = this.entries.filter((entry) => entry.id !== id)
    if (next.length === this.entries.length) return
    this.entries = next
    this.emit()
  }

  /** Everything that was held, and the store is empty afterwards. */
  take(): DraftEntry[] {
    if (this.entries.length === 0) return []
    const out = this.entries.map(copy)
    this.entries = []
    this.emit()
    return out
  }

  clear(): void {
    if (this.entries.length === 0) return
    this.entries = []
    this.emit()
  }

  private emit(): void {
    this.onChange(this.list())
  }
}
