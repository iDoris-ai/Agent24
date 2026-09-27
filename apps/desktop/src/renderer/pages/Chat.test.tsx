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

  // ME4-desktop-model-ui: every successful reply gets a timing suffix. The
  // response body here is exactly today's real `ChatResponse` shape
  // ({message, usage}, no model_id/latency of its own), so this exercises the
  // actual fallback path: frontend-timed total + the topbar's default model.
  it('shows a latency suffix with the default model name under a successful reply', async () => {
    mockBackendProxy.mockResolvedValue({
      ok: true,
      status: 200,
      data: { message: { content: 'Hello from AI!' }, usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 } },
    })

    render(<ChatPage defaultModelName="Qwen3.6-35B-A3B-MLX-8bit" />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'Hi there' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })

    await waitFor(() => {
      expect(screen.getByText('Hello from AI!')).toBeInTheDocument()
    })
    // Non-streaming: only 总计 (首字 would equal it and is omitted), model name
    // present, no tier (today's ChatResponse doesn't report one).
    expect(screen.getByText(/Qwen3\.6-35B-A3B-MLX-8bit · 总计 \d+(,\d{3})* ms/)).toBeInTheDocument()
    expect(screen.queryByText(/首字/)).not.toBeInTheDocument()
  })

  it('shows just the timing, no model segment, when no default model name is known', async () => {
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
    render(<ChatPage defaultModelName="m" />)
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
