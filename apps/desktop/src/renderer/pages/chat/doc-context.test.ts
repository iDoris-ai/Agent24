import { afterEach, describe, expect, it } from 'vitest'
import { askAboutDocument, bindPendingDocument, documentOf, documentText, MAX_DOCUMENT_CHARS, pendingDocument, resetDocContextForTests, unbindDocument, withDocument } from './doc-context'

afterEach(() => resetDocContextForTests())
const doc = { id: 'd1', title: '合同', revision: 2 }

describe('F1 document context', () => {
  it('binds the asked document to the next empty conversation, once', () => {
    askAboutDocument(doc)
    expect(pendingDocument()).toEqual(doc)
    bindPendingDocument('c1')
    expect(documentOf('c1')).toEqual(doc)
    expect(pendingDocument()).toBeNull()
    bindPendingDocument('c2')
    expect(documentOf('c2')).toBeNull()
    unbindDocument('c1')
    expect(documentOf('c1')).toBeNull()
  })

  it('reads page by page and stops at the limit', async () => {
    const asked: Array<string | undefined> = []
    const read = async (req: { cursor?: string }) => {
      asked.push(req.cursor)
      return { ok: true as const, data: { blocks: [{ text: 'x'.repeat(4000) }], next_cursor: 'next' } }
    }
    const body = await documentText(doc, read)
    expect(body.cut).toBe(true)
    expect(body.text.length).toBe(MAX_DOCUMENT_CHARS)
    expect(asked).toEqual([undefined, 'next'])
    const short = await documentText(doc, async () => ({ ok: true as const, data: { blocks: [{ text: '付款期限 30 天' }], next_cursor: null } }))
    expect(short).toEqual({ text: '付款期限 30 天\n', cut: false })
    const q = withDocument(doc, short, '付款期限是多少？')
    expect(q).toContain('《合同》')
    expect(q).toContain('付款期限 30 天')
    expect(q.endsWith('付款期限是多少？')).toBe(true)
  })
})
