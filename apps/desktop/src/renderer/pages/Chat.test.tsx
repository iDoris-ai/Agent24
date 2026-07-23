// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import ChatPage from './Chat'

const mockBackendProxy = vi.fn()

beforeEach(() => {
  vi.clearAllMocks()
  Object.defineProperty(window, 'agent24', {
    value: { backendProxy: mockBackendProxy },
    writable: true,
    configurable: true,
  })
})

describe('ChatPage', () => {
  it('renders empty state with suggestions', () => {
    render(<ChatPage />)
    expect(screen.getByText('Agent24')).toBeInTheDocument()
    expect(screen.getByText('你的本地 AI 助理，数据不离机')).toBeInTheDocument()
    expect(screen.getByText('帮我分析一段文字')).toBeInTheDocument()
  })

  it('sends message and shows assistant reply', async () => {
    mockBackendProxy.mockResolvedValue({
      ok: true,
      status: 200,
      data: { message: { content: 'Hello from AI!' } },
    })

    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'Hi there' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })

    await waitFor(() => {
      expect(screen.getByText('Hi there')).toBeInTheDocument()
      expect(screen.getByText('Hello from AI!')).toBeInTheDocument()
    })
    expect(mockBackendProxy).toHaveBeenCalledWith(
      expect.objectContaining({ method: 'POST', path: '/api/llm/chat' }),
    )
  })

  it('shows error when backend returns non-ok', async () => {
    mockBackendProxy.mockResolvedValue({
      ok: false,
      status: 503,
      data: { error: 'Service unavailable' },
    })

    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'test' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })

    await waitFor(() => {
      expect(screen.getByText(/无法连接到后端服务/)).toBeInTheDocument()
    })
  })

  it('Shift+Enter does not send', () => {
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'test' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: true })
    expect(mockBackendProxy).not.toHaveBeenCalled()
  })

  it('send button is disabled when input is empty', () => {
    render(<ChatPage />)
    const btn = screen.getByRole('button', { name: '↑' })
    expect(btn).toBeDisabled()
  })

  it('clicking a suggestion sends it as a message', async () => {
    mockBackendProxy.mockResolvedValue({
      ok: true,
      status: 200,
      data: { message: { content: 'Done.' } },
    })

    render(<ChatPage />)
    fireEvent.click(screen.getByText('帮我分析一段文字'))

    await waitFor(() => {
      expect(mockBackendProxy).toHaveBeenCalled()
    })
  })
})
