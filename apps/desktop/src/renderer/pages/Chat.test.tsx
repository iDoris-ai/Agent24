// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import ChatPage from './Chat'
import { setLocale } from '../i18n'
import { chatMessages } from './Chat.i18n'

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
  beforeEach(() => { setLocale('zh') })
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

  // M1-T12: every `/api/v1/chat` call carries this conversation's session_id
  // (a daemon-side D1 memory key), and it stays the SAME across multiple
  // sends within one mounted conversation but differs across conversations
  // (a fresh mount of the page).
  it('sends a stable session_id with every call, and a fresh one per conversation', async () => {
    mockBackendProxy.mockResolvedValue({
      ok: true,
      status: 200,
      data: { message: { content: 'ack' } },
    })

    const { unmount } = render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'first' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(mockBackendProxy).toHaveBeenCalledTimes(1))

    fireEvent.change(textarea, { target: { value: 'second' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(mockBackendProxy).toHaveBeenCalledTimes(2))

    const firstCallBody = mockBackendProxy.mock.calls[0][0].body
    const secondCallBody = mockBackendProxy.mock.calls[1][0].body
    expect(firstCallBody.session_id).toBeTruthy()
    expect(secondCallBody.session_id).toBe(firstCallBody.session_id)

    unmount()
    mockBackendProxy.mockClear()
    render(<ChatPage />)
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), { target: { value: 'third' } })
    fireEvent.keyDown(screen.getByPlaceholderText(/输入消息/), { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(mockBackendProxy).toHaveBeenCalledTimes(1))
    const thirdCallBody = mockBackendProxy.mock.calls[0][0].body
    expect(thirdCallBody.session_id).toBeTruthy()
    expect(thirdCallBody.session_id).not.toBe(firstCallBody.session_id)
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

    await waitFor(() => expect(screen.getByText(/对话请求失败/)).toBeInTheDocument())
    expect(screen.queryByText(/总计/)).not.toBeInTheDocument()
  })

  it('does not blame the model service when the backend daemon never became ready', async () => {
    // `proxyToBackend` answers this exact shape while agent24d has not
    // announced its ready line. The renderer must say so instead of telling
    // the user to start a model service that was never the problem.
    mockBackendProxy.mockResolvedValue({
      ok: false,
      status: 503,
      data: {
        error: {
          code: 'backend_not_ready',
          message: 'Backend daemon is not ready (no announced port or credential)',
        },
      },
    })

    render(<ChatPage />)
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), { target: { value: 'test' } })
    fireEvent.keyDown(screen.getByPlaceholderText(/输入消息/), { key: 'Enter', shiftKey: false })

    await waitFor(() => {
      expect(screen.getByText(/Agent24 后端服务（agent24d）尚未就绪/)).toBeInTheDocument()
    })
    expect(screen.queryByText(/对话请求失败/)).not.toBeInTheDocument()
    expect(screen.queryByText(/11434/)).not.toBeInTheDocument()
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
      expect(screen.getByText(/对话请求失败/)).toBeInTheDocument()
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

  // M1-T14: the receipt must come from the server's `memory_receipt` field,
  // never from the model's own reply text — these three cover each value,
  // plus the absent case is already covered implicitly by every other test
  // above (none of their mock responses set `memory_receipt`).
  it('shows the saved receipt line when memory_receipt is "saved"', async () => {
    mockBackendProxy.mockResolvedValue({
      ok: true,
      status: 200,
      data: { message: { content: '好的' }, memory_receipt: 'saved' },
    })
    render(<ChatPage />)
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), { target: { value: '记住我对花生过敏' } })
    fireEvent.keyDown(screen.getByPlaceholderText(/输入消息/), { key: 'Enter', shiftKey: false })

    await waitFor(() => expect(screen.getByText('好的')).toBeInTheDocument())
    expect(screen.getByText('✓ 已记住，可在「记忆」页查看或撤回')).toBeInTheDocument()
  })

  it('shows the paused receipt line when memory_receipt is "paused_not_saved"', async () => {
    mockBackendProxy.mockResolvedValue({
      ok: true,
      status: 200,
      data: { message: { content: '好的' }, memory_receipt: 'paused_not_saved' },
    })
    render(<ChatPage />)
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), { target: { value: '记住我对花生过敏' } })
    fireEvent.keyDown(screen.getByPlaceholderText(/输入消息/), { key: 'Enter', shiftKey: false })

    await waitFor(() => expect(screen.getByText('好的')).toBeInTheDocument())
    expect(screen.getByText('记忆已暂停，这条没有被记住')).toBeInTheDocument()
  })

  it('shows the failed receipt line when memory_receipt is "failed"', async () => {
    mockBackendProxy.mockResolvedValue({
      ok: true,
      status: 200,
      data: { message: { content: '好的' }, memory_receipt: 'failed' },
    })
    render(<ChatPage />)
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), { target: { value: '记住我对花生过敏' } })
    fireEvent.keyDown(screen.getByPlaceholderText(/输入消息/), { key: 'Enter', shiftKey: false })

    await waitFor(() => expect(screen.getByText('好的')).toBeInTheDocument())
    expect(screen.getByText('记住失败，请稍后重试')).toBeInTheDocument()
  })

  it('shows the partial receipt line when memory_receipt is "partial"', async () => {
    mockBackendProxy.mockResolvedValue({
      ok: true,
      status: 200,
      data: { message: { content: '好的' }, memory_receipt: 'partial', memory_error_code: 'memory_quota_rows' },
    })
    render(<ChatPage />)
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), { target: { value: '记住我对花生过敏' } })
    fireEvent.keyDown(screen.getByPlaceholderText(/输入消息/), { key: 'Enter', shiftKey: false })

    await waitFor(() => expect(screen.getByText('好的')).toBeInTheDocument())
    expect(screen.getByText('对话已保存，但这条记忆没记住')).toBeInTheDocument()
  })

  it('shows no receipt line when memory_receipt is absent', async () => {
    mockBackendProxy.mockResolvedValue({
      ok: true,
      status: 200,
      data: { message: { content: 'Hello from AI!' } },
    })
    render(<ChatPage />)
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), { target: { value: 'Hi there' } })
    fireEvent.keyDown(screen.getByPlaceholderText(/输入消息/), { key: 'Enter', shiftKey: false })

    await waitFor(() => expect(screen.getByText('Hello from AI!')).toBeInTheDocument())
    expect(screen.queryByText(/已记住|记忆已暂停|记住失败/)).not.toBeInTheDocument()
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

describe('ChatPage with Knowledge', () => {
  beforeEach(() => { setLocale('zh') })

  it('asks main before every turn, so a switch turned On after the page opened takes effect', async () => {
    const status = vi.fn()
      .mockResolvedValueOnce({ enabled: true, active: false })
      .mockResolvedValue({ enabled: true, active: true })
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: mockBackendProxy, knowledge: { status } },
      writable: true,
      configurable: true,
    })
    mockBackendProxy.mockImplementation(async (req: { method: string; path: string }) => {
      if (req.path === '/api/v1/chat') return { ok: true, status: 200, data: { message: { content: 'plain' } } }
      if (req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: 's1' } }
      if (req.method === 'POST' && req.path === '/api/v1/runs') return { ok: true, status: 201, data: { id: 'r1' } }
      return { ok: true, status: 200, data: { status: 'completed', output: { text: 'from the knowledge base' }, knowledge: { searched: true, read_chunks: [{ knowledge_id: 'doc-7', chunk_id: 'chk-71' }] } } }
    })

    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'first' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText('plain')).toBeInTheDocument())

    fireEvent.change(textarea, { target: { value: 'second' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText('from the knowledge base')).toBeInTheDocument(), { timeout: 3000 })
    expect(screen.getByText(chatMessages.zh['knowledge.via'])).toBeInTheDocument()
    expect(screen.queryByText(/本次没有读到/)).not.toBeInTheDocument()
    expect(mockBackendProxy).toHaveBeenCalledWith(expect.objectContaining({ method: 'POST', path: '/api/v1/runs', body: { prompt: 'second', session_id: 's1' } }))
  })
})

describe('ChatPage with Knowledge when the answer names sources the run did not read', () => {
  beforeEach(() => { setLocale('zh') })

  const answerWith = async (text: string, knowledge: unknown) => {
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: mockBackendProxy, knowledge: { status: vi.fn().mockResolvedValue({ enabled: true, active: true, endpoint: 'https://a.example/api/v1', knowledgeBaseIds: ['kb'] }) } },
      writable: true,
      configurable: true,
    })
    mockBackendProxy.mockImplementation(async (req: { method: string; path: string }) => {
      if (req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: 's1' } }
      if (req.method === 'POST' && req.path === '/api/v1/runs') return { ok: true, status: 201, data: { id: 'r1' } }
      return { ok: true, status: 200, data: { status: 'completed', output: { text }, knowledge } }
    })
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'Can the tap water be drunk during the boil water notice?' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText(text)).toBeInTheDocument(), { timeout: 3000 })
  }

  it('says the answer did not search Knowledge when the run called no tool, and marks its sources', async () => {
    const made = 'b512455b-6df2-4419-8df1-72126627085c#22b86b23-4bce-4898-9e43-9c62411f54c9'
    await answerWith(`煮沸至少 1 分钟。来源：s01-02-zh-water-2025.pdf，${made}`, { searched: false, read_chunks: [] })
    expect(screen.queryByText(chatMessages.zh['knowledge.via'])).not.toBeInTheDocument()
    expect(screen.getByText(chatMessages.zh['knowledge.notSearched'])).toBeInTheDocument()
    expect(screen.getByText(`⚠️ 以下来源本次没有读到，不能当作来源：${made}`)).toBeInTheDocument()
  })

  it('marks only the sources that are not among the chunks the run read back', async () => {
    await answerWith('答案。来源：doc-7#chk-71；doc-9#chk-99', { searched: true, read_chunks: [{ knowledge_id: 'doc-7', chunk_id: 'chk-71' }] })
    expect(screen.getByText(chatMessages.zh['knowledge.via'])).toBeInTheDocument()
    expect(screen.getByText('⚠️ 以下来源本次没有读到，不能当作来源：doc-9#chk-99')).toBeInTheDocument()
  })
})

describe('ChatPage with Knowledge switched On but not in effect', () => {
  beforeEach(() => { setLocale('zh') })

  const askPlain = async (status: { enabled: boolean; active: boolean }) => {
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: mockBackendProxy, knowledge: { status: vi.fn().mockResolvedValue({ ...status, endpoint: 'https://a.example/api/v1', knowledgeBaseIds: ['kb'] }) } },
      writable: true,
      configurable: true,
    })
    mockBackendProxy.mockResolvedValue({ ok: true, status: 200, data: { message: { content: '工本费是 10 元。' } } })
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: '工本费多少？' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText('工本费是 10 元。')).toBeInTheDocument(), { timeout: 3000 })
    expect(mockBackendProxy).toHaveBeenCalledWith(expect.objectContaining({ method: 'POST', path: '/api/v1/chat' }))
  }

  it('says the answer did not use Knowledge while the switch is On but main has no binding', async () => {
    await askPlain({ enabled: true, active: false })
    expect(screen.getByText(chatMessages.zh['knowledge.inactive'])).toBeInTheDocument()
    expect(screen.queryByText(chatMessages.zh['knowledge.via'])).not.toBeInTheDocument()
  })

  it('says nothing about Knowledge while the switch is Off', async () => {
    await askPlain({ enabled: false, active: false })
    expect(screen.queryByText(chatMessages.zh['knowledge.inactive'])).not.toBeInTheDocument()
  })
})

describe('ChatPage with Knowledge on another server', () => {
  beforeEach(() => { setLocale('zh') })

  it('starts a new session once the endpoint or knowledge bases change', async () => {
    const status = vi.fn()
      .mockResolvedValueOnce({ enabled: true, active: true, endpoint: 'https://a.example/api/v1', knowledgeBaseIds: ['kb'] })
      .mockResolvedValueOnce({ enabled: true, active: true, endpoint: 'https://a.example/api/v1', knowledgeBaseIds: ['kb'] })
      .mockResolvedValue({ enabled: true, active: true, endpoint: 'https://b.example/api/v1', knowledgeBaseIds: ['kb'] })
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: mockBackendProxy, knowledge: { status } },
      writable: true,
      configurable: true,
    })
    let sessions = 0
    mockBackendProxy.mockImplementation(async (req: { method: string; path: string }) => {
      if (req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: `s${++sessions}` } }
      if (req.method === 'POST' && req.path === '/api/v1/runs') return { ok: true, status: 201, data: { id: 'r1' } }
      return { ok: true, status: 200, data: { status: 'completed', output: { text: `answer ${sessions}` } } }
    })

    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    for (const text of ['one', 'two', 'three']) {
      fireEvent.change(textarea, { target: { value: text } })
      fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
      await waitFor(() => expect(mockBackendProxy).toHaveBeenCalledWith(expect.objectContaining({ body: expect.objectContaining({ prompt: text }) })), { timeout: 3000 })
      await waitFor(() => expect(screen.queryByText(/思考中|thinking/i)).toBeNull(), { timeout: 3000 })
    }
    const runs = mockBackendProxy.mock.calls.map(([req]) => req).filter((req) => req.method === 'POST' && req.path === '/api/v1/runs')
    expect(runs.map((req) => req.body.session_id)).toEqual(['s1', 's1', 's2'])
  })
})

describe('ChatPage when the daemon refuses an old session', () => {
  beforeEach(() => { setLocale('zh') })

  it('asks again in a new session, once', async () => {
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: mockBackendProxy, knowledge: { status: vi.fn().mockResolvedValue({ enabled: true, active: true, endpoint: 'https://a.example/api/v1', knowledgeBaseIds: ['kb'] }) } },
      writable: true,
      configurable: true,
    })
    let sessions = 0
    mockBackendProxy.mockImplementation(async (req: { method: string; path: string; body?: { session_id?: string } }) => {
      if (req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: `s${++sessions}` } }
      if (req.method === 'POST' && req.path === '/api/v1/runs') {
        return req.body?.session_id === 's1'
          ? { ok: false, status: 409, data: { error: { code: 'knowledge_session_rebound', message: 'start a new session' } } }
          : { ok: true, status: 201, data: { id: 'r1' } }
      }
      return { ok: true, status: 200, data: { status: 'completed', output: { text: 'from s2' } } }
    })

    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'q' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText('from s2')).toBeInTheDocument(), { timeout: 3000 })
    const runs = mockBackendProxy.mock.calls.map(([req]) => req).filter((req) => req.method === 'POST' && req.path === '/api/v1/runs')
    expect(runs.map((req) => req.body.session_id)).toEqual(['s1', 's2'])
  })
})

describe('ChatPage with Knowledge when iDoris needs reserved memory', () => {
  beforeEach(() => { setLocale('zh') })

  it('shows the inline confirmation, then sends the turn again in the same session with the flag', async () => {
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: mockBackendProxy, knowledge: { status: vi.fn().mockResolvedValue({ enabled: true, active: true, endpoint: 'https://a.example/api/v1', knowledgeBaseIds: ['kb'] }) } },
      writable: true,
      configurable: true,
    })
    let sessions = 0
    mockBackendProxy.mockImplementation(async (req: { method: string; path: string; body?: { idoris_headroom_confirmed?: boolean } }) => {
      if (req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: `s${++sessions}` } }
      if (req.method === 'POST' && req.path === '/api/v1/runs') return { ok: true, status: 201, data: { id: req.body?.idoris_headroom_confirmed ? 'r2' : 'r1' } }
      return req.path.endsWith('/r1')
        ? { ok: true, status: 200, data: { status: 'failed', error: { code: 'idoris_headroom_confirmation_required', message: 'reserved memory' } } }
        : { ok: true, status: 200, data: { status: 'completed', output: { text: 'loaded with knowledge' } } }
    })

    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'q' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    const confirm = await screen.findByRole('button', { name: chatMessages.zh['headroom.confirm'] }, { timeout: 3000 })
    expect(screen.queryByText(/知识库回答失败/)).not.toBeInTheDocument()
    fireEvent.click(confirm)
    await waitFor(() => expect(screen.getByText('loaded with knowledge')).toBeInTheDocument(), { timeout: 3000 })
    const runs = mockBackendProxy.mock.calls.map(([req]) => req).filter((req) => req.method === 'POST' && req.path === '/api/v1/runs')
    expect(runs.map((req) => req.body)).toEqual([
      { prompt: 'q', session_id: 's1' },
      { prompt: 'q', session_id: 's1', idoris_headroom_confirmed: true },
    ])
    expect(sessions).toBe(1)
  })
})

describe('ChatPage English default', () => {
  it('shows the English welcome copy', () => {
    render(<ChatPage />)
    expect(screen.getByText('Your local AI assistant. Your data stays on this device.')).toBeInTheDocument()
  })
})

describe('ID-HR-2 chat confirmation', () => {
  const headroom = { ok: false, status: 503, data: { error: { code: 'idoris_headroom_confirmation_required', message: 'reserved memory' } } }
  const submit = (text: string) => {
    const input = screen.getByRole('textbox')
    fireEvent.change(input, { target: { value: text } })
    fireEvent.keyDown(input, { key: 'Enter' })
  }

  it('shows an inline confirmation instead of an ordinary error without automatically retrying', async () => {
    mockBackendProxy.mockResolvedValue(headroom)
    render(<ChatPage />)
    submit('original prompt')
    expect(await screen.findByRole('button', { name: 'Load anyway' })).toBeInTheDocument()
    expect(screen.getByText(/iDoris needs to use memory reserved for the system/).closest('.message')).toHaveTextContent('original prompt')
    expect(screen.queryByText(/chat request failed/)).not.toBeInTheDocument()
    expect(mockBackendProxy).toHaveBeenCalledTimes(1)
    expect(mockBackendProxy.mock.calls[0][0].body).not.toHaveProperty('idoris_headroom_confirmed')
  })

  it('confirms exactly once with the same request and asks again for the next message', async () => {
    mockBackendProxy.mockResolvedValueOnce(headroom)
      .mockResolvedValueOnce({ ok: true, status: 200, data: { message: { content: 'Loaded.' } } })
      .mockResolvedValueOnce(headroom)
    render(<ChatPage />)
    submit('original\nprompt')
    const confirm = await screen.findByRole('button', { name: 'Load anyway' })
    fireEvent.click(confirm)
    fireEvent.click(confirm)
    expect(await screen.findByText('Loaded.')).toBeInTheDocument()
    expect(mockBackendProxy).toHaveBeenCalledTimes(2)
    expect(mockBackendProxy.mock.calls[1][0]).toEqual({ ...mockBackendProxy.mock.calls[0][0], body: { ...mockBackendProxy.mock.calls[0][0].body, idoris_headroom_confirmed: true } })
    expect(screen.getAllByText(/original/)).toHaveLength(1)
    submit('next prompt')
    expect(await screen.findByRole('button', { name: 'Load anyway' })).toBeInTheDocument()
    expect(mockBackendProxy).toHaveBeenCalledTimes(3)
    expect(mockBackendProxy.mock.calls[2][0].body).not.toHaveProperty('idoris_headroom_confirmed')
  })

  it('marks cancellation as not sent and excludes it from later requests', async () => {
    mockBackendProxy.mockResolvedValueOnce(headroom)
      .mockResolvedValue({ ok: true, status: 200, data: { message: { content: 'Done.' } } })
    render(<ChatPage />)
    submit('cancelled prompt')
    fireEvent.click(await screen.findByRole('button', { name: 'Cancel' }))
    expect(screen.getByText('Not sent')).toBeInTheDocument()
    expect(mockBackendProxy).toHaveBeenCalledTimes(1)
    submit('next prompt')
    expect(await screen.findByText('Done.')).toBeInTheDocument()
    expect(mockBackendProxy.mock.calls[1][0].body.messages).toEqual([{ role: 'user', content: 'next prompt' }])
  })

  it('does not automatically retry a confirmed request that still requires confirmation', async () => {
    mockBackendProxy.mockResolvedValue(headroom)
    render(<ChatPage />)
    submit('prompt')
    fireEvent.click(await screen.findByRole('button', { name: 'Load anyway' }))
    await waitFor(() => expect(screen.getByRole('button', { name: 'Load anyway' })).toBeDisabled())
    expect(mockBackendProxy).toHaveBeenCalledTimes(2)
    expect(screen.queryByText(/chat request failed/)).not.toBeInTheDocument()
  })

  it('shows supervisor busy as retryable without a confirmation card', async () => {
    mockBackendProxy.mockResolvedValue({ ok: false, status: 503, data: { error: { code: 'idoris_supervisor_busy', message: 'busy' } } })
    render(<ChatPage />)
    submit('prompt')
    expect(await screen.findByText(/iDoris is busy.*try again/i)).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Load anyway' })).not.toBeInTheDocument()
    expect(mockBackendProxy).toHaveBeenCalledTimes(1)
  })

  it.each(['en', 'zh', 'th'] as const)('has complete nonempty confirmation translations in %s', locale => {
    for (const key of ['headroom.explanation', 'headroom.confirm', 'headroom.cancel', 'status.notSent', 'reply.supervisorBusy']) {
      expect(chatMessages[locale]).toHaveProperty([key], expect.stringMatching(/\S/))
    }
  })
})
