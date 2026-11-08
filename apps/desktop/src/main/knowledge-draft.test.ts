import { describe, expect, it } from 'vitest'

import { DRAFT_LIMIT, KnowledgeDraft, type DraftEntry, type DraftItem } from './knowledge-draft'

const item = (n: number): DraftItem => ({
  title: `t${n}`,
  text: `body${n}`,
  ref: { knowledge_base_id: 'kb', knowledge_id: 'doc', chunk_id: `c${n}` },
})

describe('KnowledgeDraft', () => {
  it('add is all or nothing', () => {
    const d = new KnowledgeDraft(() => {})
    expect(d.add(Array.from({ length: 15 }, (_, i) => item(i)))).toBe(true)
    expect(d.list()).toHaveLength(15)
    expect(d.add([item(20), item(21)])).toBe(false)
    expect(d.list().map((entry) => entry.ref.chunk_id)).toEqual(Array.from({ length: 15 }, (_, i) => `c${i}`))
    expect(d.add([item(15)])).toBe(true)
    expect(d.list()).toHaveLength(16)
    expect(d.add([item(99)])).toBe(false)
    expect(d.list()).toHaveLength(16)
    expect(d.list().every((entry) => /^[A-Za-z0-9_-]{22}$/.test(entry.id))).toBe(true)
  })

  it('room is how many more fit', () => {
    const d = new KnowledgeDraft(() => {})
    expect(d.room()).toBe(DRAFT_LIMIT)
    d.add([item(1)])
    expect(d.room()).toBe(DRAFT_LIMIT - 1)
    d.add(Array.from({ length: 15 }, (_, i) => item(i + 2)))
    expect(d.room()).toBe(0)
  })

  it('remove drops that id and ignores one that is not there', () => {
    const d = new KnowledgeDraft(() => {})
    d.add([item(1), item(2)])
    const [first, second] = d.list()
    d.remove('not-an-id')
    expect(d.list().map((entry) => entry.id)).toEqual([first.id, second.id])
    d.remove(first.id)
    expect(d.list().map((entry) => entry.id)).toEqual([second.id])
  })

  it('onChange runs for a real change, with a snapshot', () => {
    const seen: DraftEntry[][] = []
    const d = new KnowledgeDraft((entries) => { seen.push([...entries]) })
    expect(d.add([])).toBe(true)
    d.add([item(1)])
    const first = seen[0]
    d.add([item(2)])
    expect(first).toHaveLength(1)
    expect(seen[1]).toHaveLength(2)
    d.remove('missing')
    expect(seen).toHaveLength(2)
    d.remove(d.list()[0].id)
    expect(seen[2]).toHaveLength(1)
    d.remove(d.list()[0].id)
    expect(seen[3]).toEqual([])
    const before = seen.length
    d.clear()
    expect(seen).toHaveLength(before)
    d.add([item(3)])
    d.clear()
    expect(seen.at(-1)).toEqual([])
    expect(d.list()).toEqual([])
  })
})
