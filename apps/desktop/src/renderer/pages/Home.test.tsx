// @vitest-environment jsdom
import { act, fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import HomePage from './Home'
import { setLocale } from '../i18n'

const props = { backendPort: 60128, moduleCount: 2, onNavigate: vi.fn() }

describe('HomePage', () => {
  it('narrates the first stage while the backend is online', () => {
    render(<HomePage backendOk={true} {...props} />)
    expect(screen.getByRole('slider')).toHaveAttribute('aria-valuenow', '1')
    expect(screen.getByText(/A desktop chat starts a run/)).toBeInTheDocument()
    expect(screen.getByText('Running :60128')).toBeInTheDocument()
  })

  it('stops the machine and says why when the backend is offline', () => {
    render(<HomePage backendOk={false} {...props} />)
    expect(screen.getByText(/Backend offline/)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Play|Pause/ })).toBeDisabled()
  })

  it('steps with the arrow keys and keeps readout and aria in sync', () => {
    render(<HomePage backendOk={true} {...props} />)
    const fig = screen.getByRole('slider')
    act(() => { fireEvent.keyDown(fig, { key: 'ArrowRight' }) })
    act(() => { fireEvent.keyDown(fig, { key: 'ArrowRight' }) })
    expect(fig).toHaveAttribute('aria-valuenow', '3')
    expect(fig.getAttribute('aria-valuetext')).toMatch(/^Step 3\/9: .*M-D memory/)
    expect(screen.getByText(/^3 \//)).toBeInTheDocument()
    act(() => { fireEvent.keyDown(fig, { key: 'Home' }) })
    expect(fig).toHaveAttribute('aria-valuenow', '1')
  })

  it('opens the page behind a picked component', () => {
    const onNavigate = vi.fn()
    render(<HomePage backendOk={true} {...props} onNavigate={onNavigate} />)
    fireEvent.click(screen.getByRole('button', { name: /Approval gate/ }))
    fireEvent.click(screen.getByText('Open this page →'))
    expect(onNavigate).toHaveBeenCalledWith('approvals')
  })

  it('follows the global language setting', () => {
    render(<HomePage backendOk={true} {...props} />)
    act(() => { setLocale('zh') })
    expect(screen.getByText('Agent24 总览')).toBeInTheDocument()
    expect(screen.getByText(/桌面对话发起一次 run/)).toBeInTheDocument()
    act(() => { setLocale('th') })
    expect(screen.getByText('ภาพรวม Agent24')).toBeInTheDocument()
  })

  it('keeps the caption quiet for screen readers while autoplaying, and announces manual steps', () => {
    render(<HomePage backendOk={true} {...props} />)
    const caption = screen.getByText(/A desktop chat starts a run/)
    expect(caption).toHaveAttribute('aria-live', 'off')
    act(() => { fireEvent.keyDown(screen.getByRole('slider'), { key: 'ArrowRight' }) })
    expect(screen.getByText(/agent24d creates the run/)).toHaveAttribute('aria-live', 'polite')
  })

  it('stops when reduced motion is switched on while the page is open', () => {
    let listener: ((e: { matches: boolean }) => void) | undefined
    const original = window.matchMedia
    window.matchMedia = ((q: string) => ({
      matches: false, media: q,
      addEventListener: (_: string, fn: typeof listener) => { listener = fn },
      removeEventListener: () => {},
    })) as unknown as typeof window.matchMedia
    try {
      render(<HomePage backendOk={true} {...props} />)
      expect(screen.getByRole('button', { name: 'Pause' })).toBeInTheDocument()
      act(() => { listener?.({ matches: true }) })
      expect(screen.getByRole('button', { name: 'Play' })).toBeInTheDocument()
    } finally {
      window.matchMedia = original
    }
  })
})
