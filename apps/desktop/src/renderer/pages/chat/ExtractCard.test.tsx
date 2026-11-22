// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import { render, screen, within } from '@testing-library/react'
import { ExtractCard } from './ExtractCard'

// F3 (General 0.6.101): a conflict reads as its values, each with its own pages.
describe('ExtractCard', () => {
  it('lists each value of a conflict with its own sources, and chooses none', () => {
    render(<ExtractCard card={{ documentId: 'doc_1', title: '租赁合同.pdf', rows: [
      { name: '金额', found: [
        { value: '8,500 元', sources: [{ block_id: 'p2/b1', page: 2, quote: '月租金 8,500 元' }] },
        { value: '17,000 元', sources: [{ block_id: 'p3/b4', page: 3, quote: '押金 17,000 元' }] },
      ] },
      { name: '开户银行', found: [] },
    ] }} />)
    const card = screen.getByRole('group', { name: '抽取结果' })
    expect(within(card).getByText('文档里有 2 种写法，未替你选：')).toBeInTheDocument()
    const first = within(card).getByText('8,500 元', { exact: false })
    expect(within(first).getByRole('button', { name: '第 2 页' })).toBeInTheDocument()
    const second = within(card).getByText('17,000 元', { exact: false })
    expect(within(second).getByRole('button', { name: '第 3 页' })).toBeInTheDocument()
    expect(within(card).getByText('文档里没找到')).toBeInTheDocument()
  })
})
