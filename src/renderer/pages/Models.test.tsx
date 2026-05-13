// @vitest-environment jsdom
import { describe, it, expect, vi, beforeAll } from 'vitest'
import { render, screen } from '@testing-library/react'
import ModelsPage from './Models'

beforeAll(() => {
  window.agent24 = {
    backendProxy: vi.fn().mockResolvedValue({ ok: false, status: 503, data: [] }),
  } as never
})

describe('ModelsPage', () => {
  it('renders model list title', () => {
    render(<ModelsPage />)
    expect(screen.getByText('模型管理')).toBeInTheDocument()
  })

  it('renders all model entries', () => {
    render(<ModelsPage />)
    expect(screen.getByText('Qwen3-30B-A3B')).toBeInTheDocument()
    expect(screen.getByText('bge-m3')).toBeInTheDocument()
    expect(screen.getByText('FLUX.1-schnell')).toBeInTheDocument()
  })

  it('renders download button', () => {
    render(<ModelsPage />)
    expect(screen.getByRole('button', { name: '+ 下载模型' })).toBeInTheDocument()
  })

  it('renders model badges', () => {
    render(<ModelsPage />)
    const badges = screen.getAllByText('常驻')
    expect(badges.length).toBeGreaterThan(0)
    const ondemand = screen.getAllByText('按需')
    expect(ondemand.length).toBeGreaterThan(0)
  })
})
