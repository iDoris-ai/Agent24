// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import HelloModule from './HelloModule'

const mockBackendProxy = vi.fn()

beforeEach(() => {
  vi.clearAllMocks()
  Object.defineProperty(window, 'agent24', {
    value: { backendProxy: mockBackendProxy },
    writable: true,
    configurable: true,
  })
})

describe('HelloModule', () => {
  it('renders input and disabled greet button when no name', () => {
    render(<HelloModule />)
    expect(screen.getByPlaceholderText('e.g. Jason')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Greet me' })).toBeDisabled()
  })

  it('enables greet button when name is entered', () => {
    render(<HelloModule />)
    fireEvent.change(screen.getByPlaceholderText('e.g. Jason'), { target: { value: 'Jason' } })
    expect(screen.getByRole('button', { name: 'Greet me' })).not.toBeDisabled()
  })

  it('calls backendProxy and shows greeting', async () => {
    mockBackendProxy.mockResolvedValue({
      ok: true,
      status: 200,
      data: { greeting: 'Hello, Jason!' },
    })

    render(<HelloModule />)
    fireEvent.change(screen.getByPlaceholderText('e.g. Jason'), { target: { value: 'Jason' } })
    fireEvent.click(screen.getByRole('button', { name: 'Greet me' }))

    await waitFor(() => {
      expect(screen.getByText('Hello, Jason!')).toBeInTheDocument()
    })
    expect(mockBackendProxy).toHaveBeenCalledWith(
      expect.objectContaining({ method: 'POST', path: '/api/modules/hello/greet', body: { name: 'Jason' } }),
    )
  })

  it('shows error when backend returns non-ok', async () => {
    mockBackendProxy.mockResolvedValue({ ok: false, status: 500, data: { error: 'Internal error' } })

    render(<HelloModule />)
    fireEvent.change(screen.getByPlaceholderText('e.g. Jason'), { target: { value: 'test' } })
    fireEvent.click(screen.getByRole('button', { name: 'Greet me' }))

    await waitFor(() => {
      expect(screen.getByText(/Error: Internal error/)).toBeInTheDocument()
    })
  })

  it('truncates long names to 80 chars', async () => {
    mockBackendProxy.mockResolvedValue({ ok: true, status: 200, data: { greeting: 'Hi!' } })
    const longName = 'A'.repeat(120)

    render(<HelloModule />)
    fireEvent.change(screen.getByPlaceholderText('e.g. Jason'), { target: { value: longName } })
    fireEvent.click(screen.getByRole('button', { name: 'Greet me' }))

    await waitFor(() => {
      const [call] = mockBackendProxy.mock.calls
      expect((call[0] as { body: { name: string } }).body.name).toHaveLength(80)
    })
  })

  it('load info button calls GET endpoint', async () => {
    mockBackendProxy.mockResolvedValue({
      ok: true,
      status: 200,
      data: { moduleId: 'hello', description: 'Says hello' },
    })

    render(<HelloModule />)
    fireEvent.click(screen.getByRole('button', { name: 'Load info' }))

    await waitFor(() => {
      expect(screen.getByText(/ID: hello/)).toBeInTheDocument()
    })
    expect(mockBackendProxy).toHaveBeenCalledWith(
      expect.objectContaining({ method: 'GET', path: '/api/modules/hello/info' }),
    )
  })
})
