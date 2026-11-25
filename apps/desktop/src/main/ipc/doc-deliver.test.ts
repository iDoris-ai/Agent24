import { describe, expect, it } from 'vitest'
import { deliverRequestOf } from './doc-deliver'

const DOC = 'doc_01M679KMY6D75P0EZACBS95JYA'

describe('L8 DOC-4: documents:deliver takes one well-formed request', () => {
  it('keeps a good one and refuses the rest', () => {
    const good = { documentId: DOC, revision: 3, format: 'pdf', title: '合同.docx' }
    expect(deliverRequestOf(good)).toEqual(good)
    for (const bad of [null, 'x', { ...good, documentId: 'doc_x' }, { ...good, revision: 0 }, { ...good, revision: 1.5 }, { ...good, format: 'html' }, { ...good, title: 'a'.repeat(501) }, { ...good, savedTo: '/etc/passwd' }]) {
      expect(deliverRequestOf(bad)).toBeNull()
    }
  })
})
