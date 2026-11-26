// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest'
import { act, fireEvent, render, screen } from '@testing-library/react'
import { DeliverCard } from './DeliverCard'

const DOC = 'doc_01M679KMY6D75P0EZACBS95JYA'
const card = { documentId: DOC, title: '合同.docx', revision: 3, cleanable: true, suggested: 'pdf' as const }

function withDeliver(result: unknown) {
  const deliver = vi.fn(async () => result)
  ;(window as unknown as { agent24: unknown }).agent24 = { documents: { deliver } }
  return deliver
}

afterEach(() => { delete (window as unknown as { agent24?: unknown }).agent24 })

describe('L8 DOC-4: the delivery card', () => {
  it('offers the format asked first, and saves the version checked from its button', async () => {
    const deliver = withDeliver({ ok: true, savedTo: '/Users/u/Desktop/合同（交付）.pdf', fileName: '合同（交付）.pdf', receipt: { again: false } })
    render(<DeliverCard card={card} />)
    const buttons = screen.getAllByRole('button')
    expect(buttons[0]).toHaveTextContent('存成 PDF')
    expect(buttons[0]).toHaveClass('btn-primary')
    expect(screen.getByRole('button', { name: /存成 Word/ })).toHaveClass('btn-ghost')
    expect(screen.getByRole('group')).toHaveTextContent('原文档不动')
    await act(async () => { fireEvent.click(buttons[0]!) })
    expect(deliver).toHaveBeenCalledWith({ documentId: DOC, revision: 3, format: 'pdf', title: '合同.docx' })
    expect(screen.getByRole('group')).toHaveTextContent('已保存到 /Users/u/Desktop/合同（交付）.pdf')
    expect(screen.getByRole('group')).toHaveTextContent('已记下回执')
  })

  it('a second save of the same version says so', async () => {
    withDeliver({ ok: true, savedTo: '/d/合同（交付） (2).pdf', fileName: '合同（交付） (2).pdf', receipt: { again: true } })
    render(<DeliverCard card={card} />)
    await act(async () => { fireEvent.click(screen.getByRole('button', { name: /存成 PDF/ })) })
    expect(screen.getByRole('group')).toHaveTextContent('同一版本再次保存')
  })

  it('cancelled: nothing saved, no receipt, and the buttons stay', async () => {
    withDeliver({ ok: false, canceled: true })
    render(<DeliverCard card={card} />)
    await act(async () => { fireEvent.click(screen.getByRole('button', { name: /存成 Word/ })) })
    expect(screen.getByRole('group')).toHaveTextContent('已取消，没有保存，也没有记回执')
    expect(screen.getByRole('button', { name: /存成 Word/ })).not.toBeDisabled()
  })

  it('a failure says which step, and whether the file was saved', async () => {
    withDeliver({ ok: false, stage: 'convert', code: 'engine_unavailable', message: 'no LibreOffice' })
    const { unmount } = render(<DeliverCard card={card} />)
    await act(async () => { fireEvent.click(screen.getByRole('button', { name: /存成 PDF/ })) })
    expect(screen.getByRole('group')).toHaveTextContent('转成 PDF 没有成功')
    expect(screen.getByRole('group')).toHaveTextContent('没有保存，也没有记回执')
    unmount()
    withDeliver({ ok: false, stage: 'receipt', code: 'storage_unavailable', message: 'x', savedTo: '/d/a.pdf' })
    render(<DeliverCard card={card} />)
    await act(async () => { fireEvent.click(screen.getByRole('button', { name: /存成 PDF/ })) })
    expect(screen.getByRole('group')).toHaveTextContent('文件已保存到 /d/a.pdf，但回执没有记下')
  })

  it('a PDF is only checked: no button', () => {
    withDeliver({})
    render(<DeliverCard card={{ ...card, cleanable: false, title: '合同.pdf' }} />)
    expect(screen.queryAllByRole('button')).toEqual([])
    expect(screen.getByRole('group')).toHaveTextContent('PDF 只能检查')
  })
})
