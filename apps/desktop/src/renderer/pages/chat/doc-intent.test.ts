import { describe, expect, it } from 'vitest'
import { findByTitle, parseDocIntent } from './doc-intent'

describe('D2b document intents in a conversation about a document', () => {
  it('reads page ranges to split out', () => {
    expect(parseDocIntent('把第2到4页拆出来')).toEqual({ kind: 'split', pages: '2-4' })
    expect(parseDocIntent('拆出第 2-4 页')).toEqual({ kind: 'split', pages: '2-4' })
    expect(parseDocIntent('把第三页单独拆出来')).toEqual({ kind: 'split', pages: '3' })
    expect(parseDocIntent('提取第1页和第5到七页')).toEqual({ kind: 'split', pages: '1,5-7' })
    expect(parseDocIntent('拆分出第十二页')).toEqual({ kind: 'split', pages: '12' })
  })

  it('leaves ordinary questions to the model', () => {
    expect(parseDocIntent('第2页说了什么？')).toBeNull()
    expect(parseDocIntent('付款期限是多少')).toBeNull()
    expect(parseDocIntent('这个条款怎么拆解理解？')).toBeNull()
  })

  it('asks how to say it when the pages or the other document are missing', () => {
    expect(parseDocIntent('拆分一下')).toEqual({ kind: 'unclear', about: 'split' })
    expect(parseDocIntent('帮我把它拆开')).toEqual({ kind: 'unclear', about: 'split' })
    expect(parseDocIntent('把这两份合并')).toEqual({ kind: 'unclear', about: 'merge' })
    expect(parseDocIntent('把第4到2页拆出来')).toEqual({ kind: 'unclear', about: 'split' })
  })

  it('reads a merge with a named document', () => {
    expect(parseDocIntent('和《保险单 B》合并')).toEqual({ kind: 'merge', other: '保险单 B' })
    expect(parseDocIntent('把它跟「boil-water」合在一起')).toEqual({ kind: 'merge', other: 'boil-water' })
  })

  it('finds the named document by title, the original over what was made from it', () => {
    const docs = [
      { document_id: 'doc_1', title: '保险单 B.pdf' },
      { document_id: 'doc_2', title: 'boil-water.pdf' },
      { document_id: 'doc_3', title: '合并：CFYAD26.pdf + boil-water.pdf' },
      { document_id: 'doc_4', title: 'notice A.pdf' },
      { document_id: 'doc_5', title: 'notice B.pdf' },
    ]
    expect(findByTitle(docs, '保险单 B')).toEqual({ match: docs[0] })
    expect(findByTitle(docs, 'boil-water')).toEqual({ match: docs[1] })
    expect(findByTitle(docs, 'boil')).toEqual({ match: docs[1] })
    expect(findByTitle(docs, 'notice')).toEqual({ candidates: [docs[3], docs[4]] })
    expect(findByTitle(docs, '不存在')).toBeNull()
  })
})
