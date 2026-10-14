// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import CodeSandboxPage from './CodeSandbox'
import { setLocale } from '../i18n'

const mockBackendProxy = vi.fn()

beforeEach(() => {
  vi.clearAllMocks()
  Object.defineProperty(window, 'agent24', {
    value: { backendProxy: mockBackendProxy },
    writable: true,
    configurable: true,
  })
})

describe('CodeSandboxPage', () => {
  it('renders the English title and hint by default when available', async () => {
    mockBackendProxy.mockResolvedValue({ ok: true, status: 200, data: { available: true } })
    render(<CodeSandboxPage />)
    expect(await screen.findByText('Python sandbox')).toBeInTheDocument()
    expect(screen.getByText('Click "▶ Run" to execute the code')).toBeInTheDocument()
  })

  it('shows the unsupported notice when the sandbox is unavailable', async () => {
    mockBackendProxy.mockResolvedValue({ ok: true, status: 200, data: { available: false, error: 'no kvm' } })
    render(<CodeSandboxPage />)
    expect(await screen.findByText('Python sandbox is not supported on this device')).toBeInTheDocument()
    expect(screen.getByText('no kvm')).toBeInTheDocument()
  })

  it('runs code and shows output', async () => {
    mockBackendProxy.mockImplementation(async ({ path }: { path: string }) =>
      path === '/api/codebox/status'
        ? { ok: true, status: 200, data: { available: true } }
        : { ok: true, status: 200, data: { ok: true, output: 'hi' } })
    render(<CodeSandboxPage />)
    fireEvent.click(await screen.findByRole('button', { name: '▶ Run' }))
    await waitFor(() => expect(screen.getByText('hi')).toBeInTheDocument())
  })

  it('renders Chinese when the locale is zh', async () => {
    setLocale('zh')
    mockBackendProxy.mockResolvedValue({ ok: true, status: 200, data: { available: true } })
    render(<CodeSandboxPage />)
    expect(await screen.findByText('Python 沙箱')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: '▶ 运行' })).toBeInTheDocument()
  })
})
