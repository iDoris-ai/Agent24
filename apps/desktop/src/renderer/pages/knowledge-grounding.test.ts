import { describe, it, expect } from 'vitest'
import { citedChunks, grounding } from './knowledge-grounding'

const DOC = 'b512455b-6df2-4419-8df1-72126627085c'
const CHUNK = '22b86b23-4bce-4898-9e43-9c62411f54c9'
/** The record's L7-K0 fields, nothing found or refused. */
const base = { search_refused: false, found_passages: 0, found_titles: [] as string[] }
const read = (...ids: [string, string][]) => ({
  ...base,
  searched: true,
  read_chunks: ids.map(([knowledge_id, chunk_id]) => ({ knowledge_id, chunk_id })),
})

describe('knowledge-grounding', () => {
  it('finds every source an answer names, once each, however it is written', () => {
    const text = `煮沸 1 分钟。\n来源 / Sources\n- s01-02-zh-water-2025.pdf，${DOC}#${CHUNK}\n- doc-7#chk-71（又见 doc-7#chk-71）\nSources:doc-9#chk-99；d#c 与 C# 语言、#1515`
    expect(citedChunks(text)).toEqual([`${DOC}#${CHUNK}`, 'doc-7#chk-71', 'doc-9#chk-99', 'd#c'])
  })

  it('a source written right after a colon or with short IDs is still checked', () => {
    // Read back: stands even with "Sources:" against it.
    expect(grounding('Sources:doc-7#chk-71', read(['doc-7', 'chk-71']))).toEqual({ kind: 'read', unread: [] })
    // Not read back: marked, not passed over.
    expect(grounding('答案。Sources:doc-9#chk-99', read(['doc-7', 'chk-71'])).unread).toEqual(['doc-9#chk-99'])
    expect(grounding('来源：d#c', read(['doc-7', 'chk-71'])).unread).toEqual(['d#c'])
    expect(grounding('来源：d#c', read(['d', 'c'])).unread).toEqual([])
    // At the end of a sentence, still the source read.
    expect(grounding('见 doc-7#chk-71. 以及 doc-7#chk-71:', read(['doc-7', 'chk-71'])).unread).toEqual([])
    // A longer ID that starts or ends with one read is another source.
    expect(grounding('来源：doc-7#chk-712', read(['doc-7', 'chk-71'])).unread).toEqual(['doc-7#chk-712'])
    expect(grounding('来源：fake-doc-7#chk-71', read(['doc-7', 'chk-71'])).unread).toEqual(['fake-doc-7#chk-71'])
    // Markdown around a source read is not part of it.
    expect(grounding('~~doc-7#chk-71~~ 与 **doc-7#chk-71**', read(['doc-7', 'chk-71'])).unread).toEqual([])
    // Two sources written together are checked one by one.
    expect(grounding('doc-7#chk-71+fake-doc-8#chk-81', read(['doc-7', 'chk-71'])).unread).toEqual(['fake-doc-8#chk-81'])
  })

  it('an answer from a run that called no Knowledge tool says so, and none of its sources stand', () => {
    const text = `可以饮用。来源：s01-02-zh-water-2025.pdf，${DOC}#${CHUNK}`
    expect(grounding(text, { ...base, searched: false, read_chunks: [] })).toEqual({ kind: 'notSearched', unread: [`${DOC}#${CHUNK}`] })
    // A run without a binding searched nothing either.
    expect(grounding(text, null)).toEqual({ kind: 'notSearched', unread: [`${DOC}#${CHUNK}`] })
  })

  it('a source the run did not read back is marked, the ones it read stand', () => {
    const text = `答案。来源：doc-7#chk-71；${DOC}#${CHUNK}`
    expect(grounding(text, read(['doc-7', 'chk-71']))).toEqual({ kind: 'read', unread: [`${DOC}#${CHUNK}`] })
    // The same chunk of another document is another source.
    expect(grounding('来源：doc-8#chk-71', read(['doc-7', 'chk-71']))).toEqual({ kind: 'read', unread: ['doc-8#chk-71'] })
  })

  it('a normal answer, every source read back, is labelled as before', () => {
    expect(grounding('答案。来源：doc-7#chk-71', read(['doc-7', 'chk-71']))).toEqual({ kind: 'read', unread: [] })
  })

  it('a search with nothing read back is not an answer from Knowledge', () => {
    expect(grounding('答案。来源：doc-7#chk-71', { ...base, searched: true, read_chunks: [] })).toEqual({ kind: 'nothingRead', unread: ['doc-7#chk-71'] })
  })
})

describe('grounding (K3: a source written the model\'s own way)', () => {
  it('takes `file.pdf#<document id>` as the document the run read, and still marks a made-up one', () => {
    const knowledge = { ...base, searched: true, read_chunks: [{ knowledge_id: '17b1f6d6-c8a6-4c99-bd44-83d2d5ad3c4d', chunk_id: 'a1457516-da54-469f-bf96-a16dd9ef9460' }] }
    const text = 'Boil for 3 minutes.\n来源：s01-02-en-epa-boil-water.pdf#17b1f6d6-c8a6-4c99-bd44-83d2d5ad3c4d\nother.pdf#00000000-0000-0000-0000-000000000000'
    expect(grounding(text, knowledge)).toEqual({ kind: 'read', unread: ['pdf#00000000-0000-0000-0000-000000000000'] })
  })
})
