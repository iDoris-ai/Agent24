// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import { render, screen } from '@testing-library/react'
import { setLocale } from '../../i18n'
import { DocumentsOutcome } from './DocumentsOutcome'

// L6-F5: a rewrite saved into the original made no new document; it is not
// 「这次没有改动文档」 but the version it became.
describe('a write saved as a new version', () => {
  it('says the version, not that nothing changed', () => {
    setLocale('zh')
    render(<DocumentsOutcome documents={{ attempted: true, jobs: [], failed: [], revisions: [{ document_id: 'doc_1', title: '合同.docx', revision: 2 }] }} />)
    expect(screen.getByText('已保存：《合同.docx》第 2 版（原文件，之前的版本都保留）')).toBeInTheDocument()
    expect(screen.queryByText(/这次没有改动文档/)).toBeNull()
  })
})
