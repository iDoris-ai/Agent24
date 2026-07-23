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

  it('shows ready status for translation', () => {
    render(<WorkbenchPage />)
    const readyItems = screen.getAllByText('✓ 可用')
    expect(readyItems.length).toBeGreaterThan(0)
  })

  it('shows coming-soon status for most capabilities', () => {
    render(<WorkbenchPage />)
    const coming = screen.getAllByText('开发中')
    expect(coming.length).toBeGreaterThan(0)
  })
})
