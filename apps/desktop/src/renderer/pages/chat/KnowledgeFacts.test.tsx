// @vitest-environment jsdom
import { describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen } from '@testing-library/react'
import KnowledgeFacts from './KnowledgeFacts'
import { DocumentsOutcome } from './DocumentsOutcome'
import { knowledgeFacts } from '../knowledge-grounding'
import { clearDocFocus, pendingDocFocus } from '../documents/doc-focus'
import { setLocale } from '../../i18n'
import { CapabilitiesContext } from '../../capabilities/types'

const record = (over: Record<string, unknown>) => ({ searched: true, read_chunks: [], search_refused: false, found_passages: 0, found_titles: [], ...over }) as never

describe('L7-K0: the fact line from the run record', () => {
  it('says found, none and refused as the record does, and nothing when no search was made', () => {
    expect(knowledgeFacts(record({ found_passages: 2, found_titles: ['制度.pdf', '手册'] }))).toEqual({ kind: 'found', passages: 2, titles: ['制度.pdf', '手册'] })
    // Human-made zero: a search that returned nothing is 「没找到」.
    expect(knowledgeFacts(record({}))).toEqual({ kind: 'none' })
    expect(knowledgeFacts(record({ searched: false, search_refused: true }))).toEqual({ kind: 'refused' })
    expect(knowledgeFacts(record({ searched: false }))).toBeNull()
    expect(knowledgeFacts(null)).toBeNull()
    // Only a read-back is not a search that found nothing.
    expect(knowledgeFacts(record({ read_chunks: [{ knowledge_id: 'k', chunk_id: 'c' }] }))).toBeNull()
    // A daemon without the new fields says nothing wrong.
    expect(knowledgeFacts({ searched: true, read_chunks: [] } as never)).toEqual({ kind: 'none' })
  })

  it('renders each state as a line, found ones expandable', () => {
    setLocale('zh')
    const { rerender } = render(<KnowledgeFacts facts={{ kind: 'found', passages: 3, titles: ['制度.pdf', '手册'] }} />)
    expect(screen.getByText('已检索知识库：找到 3 段（《制度.pdf》《手册》），可展开')).toBeInTheDocument()
    expect(screen.getByText('以上是检索到的内容，不表示回答用了哪一段。')).toBeInTheDocument()
    rerender(<KnowledgeFacts facts={{ kind: 'none' }} />)
    expect(screen.getByText('已检索知识库：没找到')).toBeInTheDocument()
    rerender(<KnowledgeFacts facts={{ kind: 'refused' }} />)
    expect(screen.getByText(/检索没能进行/)).toBeInTheDocument()
    rerender(<KnowledgeFacts facts={null} />)
    expect(screen.queryByText(/检索/)).toBeNull()
  })
})

describe('L7-K0 (FU-139): 「已生成」 opens the document', () => {
  it('a made document opens its details on the documents page; a revision too', async () => {
    setLocale('zh')
    clearDocFocus()
    const open = vi.fn()
    const api = {
      request: vi.fn(async (r: { op: string }) => r.op === 'job'
        ? { ok: true, data: { status: 'succeeded', result: { document_id: 'doc_01ABC' } } }
        : { ok: true, data: { title: '合同（第 2 页）' } }),
    }
    ;(window as unknown as { agent24: unknown }).agent24 = { documents: api }
    render(<CapabilitiesContext.Provider value={{ defs: [], open }}><DocumentsOutcome documents={{ attempted: true, jobs: ['job1'], failed: [], revisions: [{ document_id: 'doc_02XYZ', title: '合同', revision: 3 }] }} /></CapabilitiesContext.Provider>)
    fireEvent.click(await screen.findByRole('button', { name: '已生成：《合同（第 2 页）》（在文档页）' }))
    expect(pendingDocFocus()).toEqual({ documentId: 'doc_01ABC', blockId: '' })
    expect(open).toHaveBeenCalledWith('documents', 'other')
    fireEvent.click(screen.getByRole('button', { name: /已保存：《合同》第 3 版/ }))
    expect(pendingDocFocus()?.documentId).toBe('doc_02XYZ')
  })
})
