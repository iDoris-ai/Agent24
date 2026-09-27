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
      expect.objectContaining({ method: 'POST', path: '/api/v1/chat' }),
    )
  })

  // Review M3: `/api/v1/chat` now reports the server-measured model_id/tier
  // for the call that actually served it — the suffix shows THAT, never a
  // guessed/default name.
  it('shows a latency suffix with the SERVER-reported model_id/tier under a successful reply', async () => {
    mockBackendProxy.mockResolvedValue({
      ok: true,
      status: 200,
      data: {
        message: { content: 'Hello from AI!' },
        usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
        model_id: 'Qwen3.6-35B-A3B-MLX-8bit',
        tier: 'local',
        latency_ms: 842,
      },
    })

    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'Hi there' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })

    await waitFor(() => {
      expect(screen.getByText('Hello from AI!')).toBeInTheDocument()
    })
    // Server-reported latency_ms wins over the frontend's own timing too.
    expect(screen.getByText('Qwen3.6-35B-A3B-MLX-8bit · 本地 · 总计 842 ms')).toBeInTheDocument()
    expect(screen.queryByText(/首字/)).not.toBeInTheDocument()
  })

  // Review M3's own negative control: the server did NOT report a model_id
  // (still a possible shape) — the suffix must show NO model name at all,
  // never fall back to a guess (this component doesn't even accept a
  // default-model prop any more).
  it('shows just the timing, no model segment, when the server reports no model_id', async () => {
    mockBackendProxy.mockResolvedValue({
      ok: true,
      status: 200,
      data: { message: { content: 'Done.' } },
    })

    render(<ChatPage />)
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), { target: { value: 'hi' } })
    fireEvent.keyDown(screen.getByPlaceholderText(/输入消息/), { key: 'Enter', shiftKey: false })

    await waitFor(() => expect(screen.getByText('Done.')).toBeInTheDocument())
    expect(screen.getByText(/^总计 \d+(,\d{3})* ms$/)).toBeInTheDocument()
  })

  it('does not attach a latency suffix to the error fallback message', async () => {
    mockBackendProxy.mockResolvedValue({
      ok: false,
      status: 503,
      data: { error: { code: 'provider_unavailable', message: 'Service unavailable' } },
    })
    render(<ChatPage />)
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), { target: { value: 'test' } })
    fireEvent.keyDown(screen.getByPlaceholderText(/输入消息/), { key: 'Enter', shiftKey: false })

    await waitFor(() => expect(screen.getByText(/无法连接到后端服务/)).toBeInTheDocument())
    expect(screen.queryByText(/总计/)).not.toBeInTheDocument()
  })

  it('shows error when backend returns non-ok', async () => {
    mockBackendProxy.mockResolvedValue({
      ok: false,
      status: 503,
      data: { error: { code: 'provider_unavailable', message: 'Service unavailable' } },
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
