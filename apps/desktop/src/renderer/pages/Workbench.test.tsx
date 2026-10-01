// @vitest-environment jsdom
import { describe, it, expect } from 'vitest'
import { render, screen } from '@testing-library/react'
import WorkbenchPage from './Workbench'

describe('WorkbenchPage', () => {
  it('renders workbench title', () => {
    render(<WorkbenchPage />)
    expect(screen.getByText('工作台')).toBeInTheDocument()
  })

  it('renders capability cards', () => {
    render(<WorkbenchPage />)
    expect(screen.getByText('ASR 语音识别')).toBeInTheDocument()
    expect(screen.getByText('TTS 语音合成')).toBeInTheDocument()
    expect(screen.getByText('RAG 知识库')).toBeInTheDocument()
  })

  // AUDIT-3: no card is clickable (none has an onClick), so none may claim
  // "✓ 可用" — every card, including 翻译, is a disabled "即将推出" preview.
  it('never shows a misleading "ready" status', () => {
    render(<WorkbenchPage />)
    expect(screen.queryByText('✓ 可用')).not.toBeInTheDocument()
    expect(screen.queryByText(/可用/)).not.toBeInTheDocument()
  })

  it('shows coming-soon status for every capability, marked disabled', () => {
    render(<WorkbenchPage />)
    const coming = screen.getAllByText('即将推出')
    expect(coming.length).toBe(9)

    const cards = document.querySelectorAll('.capability-card')
    expect(cards.length).toBe(9)
    cards.forEach((card) => {
      expect(card.getAttribute('aria-disabled')).toBe('true')
    })
  })
})
