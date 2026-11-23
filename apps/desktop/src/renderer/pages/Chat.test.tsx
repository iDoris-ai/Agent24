// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { KNOWLEDGE_HINT } from './chat-knowledge'
import { render, screen, fireEvent, waitFor, within } from '@testing-library/react'
import ChatPage from './Chat'
import { setLocale } from '../i18n'
import { chatMessages } from './Chat.i18n'
import { chatHistorySummary } from '../../shared/chat-history'
import { resetConversationSkillsForTests } from './chat/session-skill'
import { aside, resetSkillListing, skillListing } from './chat/skill-list-aside'

const mockBackendProxy = vi.fn()

beforeEach(() => {
  vi.clearAllMocks()
  resetConversationSkillsForTests()
  resetSkillListing()
  Object.defineProperty(window, 'agent24', {
    value: { backendProxy: aside(mockBackendProxy) },
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

  // Issue #1517: "New chat" clears the thread and mints a new session_id, so
  // the previous context is not sent to the model.
  it('new chat clears messages and uses a different session_id for the next send', async () => {
    mockBackendProxy.mockResolvedValue({ ok: true, status: 200, data: { message: { content: 'ack' } } })
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'old topic' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText('ack')).toBeInTheDocument())
    const oldBody = mockBackendProxy.mock.calls[0][0].body

    fireEvent.click(screen.getByRole('button', { name: '新对话' }))
    expect(screen.queryByText('old topic')).not.toBeInTheDocument()
    expect(screen.queryByText('ack')).not.toBeInTheDocument()

    fireEvent.change(screen.getByPlaceholderText(/输入消息/), { target: { value: 'new topic' } })
    fireEvent.keyDown(screen.getByPlaceholderText(/输入消息/), { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(mockBackendProxy).toHaveBeenCalledTimes(2))
    const newBody = mockBackendProxy.mock.calls[1][0].body
    expect(newBody.session_id).toBeTruthy()
    expect(newBody.session_id).not.toBe(oldBody.session_id)
    expect(newBody.messages).toEqual([{ role: 'user', content: 'new topic' }])
  })

  // UI-01: the turn rail shows once the chat has two turns; a mark jumps to
  // its prompt; "New chat" removes it.
  it('shows the turn rail from the second turn and jumps to a turn on click', async () => {
    mockBackendProxy.mockResolvedValue({ ok: true, status: 200, data: { message: { content: 'ack' } } })
    const scroll = vi.spyOn(HTMLElement.prototype, 'scrollIntoView').mockImplementation(() => {})
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'first' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(mockBackendProxy).toHaveBeenCalledTimes(1))
    expect(screen.queryByRole('navigation', { name: '本次对话的回合' })).toBeNull()

    fireEvent.change(textarea, { target: { value: 'second' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getAllByText('ack')).toHaveLength(2))
    const rail = screen.getByRole('navigation', { name: '本次对话的回合' })
    // The rail follows the newest turn even without a scroll event.
    expect(within(rail).getByRole('button', { name: '第 2 轮：second' })).toHaveAttribute('aria-current', 'step')
    scroll.mockClear()
    fireEvent.click(within(rail).getByRole('button', { name: '第 1 轮：first' }))
    const toPrompt = scroll.mock.calls.findIndex(([arg]) => (arg as ScrollIntoViewOptions | undefined)?.block === 'start')
    expect(toPrompt).toBeGreaterThanOrEqual(0)
    expect(scroll.mock.contexts[toPrompt]).toHaveTextContent('first')
    expect(within(rail).getByRole('button', { name: '第 1 轮：first' })).toHaveAttribute('aria-current', 'step')

    fireEvent.click(screen.getByRole('button', { name: '新对话' }))
    expect(screen.queryByRole('navigation', { name: '本次对话的回合' })).toBeNull()
    scroll.mockRestore()
  })

  it('disables the new chat button while a reply is in flight', async () => {
    let resolve!: (v: unknown) => void
    mockBackendProxy.mockReturnValue(new Promise(r => { resolve = r }))
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'slow' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByRole('button', { name: '新对话' })).toBeDisabled())
    resolve({ ok: true, status: 200, data: { message: { content: 'done' } } })
    await waitFor(() => expect(screen.getByRole('button', { name: '新对话' })).toBeEnabled())
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
    // VI-02: where it ran is a badge (word + shape), not a word inside the line.
    expect(screen.getByText('Qwen3.6-35B-A3B-MLX-8bit · 用时 0.8 秒')).toBeInTheDocument()
    expect(screen.getByText('本地').closest('.privacy-badge')).toHaveClass('privacy-badge-local')
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
    expect(screen.getByText(/^用时 \d+(\.\d)? 秒$/)).toBeInTheDocument()
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
    expect(screen.queryByText(/用时/)).not.toBeInTheDocument()
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
      expect(screen.getByText(/Agent24 的后台服务没能启动/)).toBeInTheDocument()
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
      value: { backendProxy: aside(mockBackendProxy), knowledge: { status } },
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
    expect(mockBackendProxy).toHaveBeenCalledWith(expect.objectContaining({ method: 'POST', path: '/api/v1/runs', body: { prompt: `${KNOWLEDGE_HINT}\nsecond`, session_id: 's1', knowledge_query: 'second' } }))
  })

  // M1 (General/PM 11-21): the line under the answer is the daemon's record
  // of the run's document writes, never the answer's words.
  it.each([
    ['nothing made', { attempted: true, jobs: [] }, /这次没有改动文档/],
    ['a document made', { attempted: true, jobs: ['job_1'] }, /已生成：《合同-链接与脚注（修改）》/],
    // General 0.6.94: A made, B refused: B is said too, by its title.
    ['one made, one not', { attempted: true, jobs: ['job_1'], failed: ['doc_B'] }, /没能修改：《合同-公式修订分页.docx》/],
  ])('a reply whose run asked for a document write says what came of it: %s', async (_case, documents, line) => {
    const request = vi.fn(async (req: { op: string; documentId?: string }) => req.op === 'job'
      ? { ok: true, data: { job_id: 'job_1', status: 'succeeded', result: { document_id: 'doc_new' } } }
      : req.documentId === 'doc_B'
        ? { ok: true, data: { document_id: 'doc_B', title: '合同-公式修订分页.docx' } }
        : { ok: true, data: { document_id: 'doc_new', title: '合同-链接与脚注（修改）' } })
    Object.defineProperty(window, 'agent24', {
      value: {
        backendProxy: aside(mockBackendProxy),
        knowledge: { status: vi.fn().mockResolvedValue({ enabled: true, active: true, endpoint: 'https://a.example/api/v1', knowledgeBaseIds: ['kb'] }) },
        documents: { request },
      },
      writable: true,
      configurable: true,
    })
    mockBackendProxy.mockImplementation(async (req: { method: string; path: string }) => {
      if (req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: 's1' } }
      if (req.method === 'POST' && req.path === '/api/v1/runs') return { ok: true, status: 201, data: { id: 'r_patch' } }
      // 0.6.82: the model said it saved a document whatever came of it.
      return { ok: true, status: 200, data: { status: 'completed', output: { text: '已保存为新文档《合同-修订版.docx》' }, documents } }
    })
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'hello' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText('已保存为新文档《合同-修订版.docx》')).toBeInTheDocument(), { timeout: 3000 })
    await waitFor(() => expect(screen.getByText(line)).toBeInTheDocument(), { timeout: 3000 })
  })

  // L6-F2 (General, 0.6.105): the App names the document a delete is for. Two
  // answer to 《中文测试合同》: a card lists them (title, import time, pages),
  // the model is not asked; the one picked goes to the model by its id only.
  it('a delete naming several documents asks which, then sends only the picked id', async () => {
    const A = 'doc_01M679KMY6D75P0EZACBS95JYA'
    const B = 'doc_01M679KMY6D75P0EZACBS95JYB'
    const docs = [
      { document_id: A, title: '中文测试合同.pdf', created_at: '2026-11-20T01:00:00.000Z' },
      { document_id: B, title: '中文测试合同.pdf', created_at: '2026-11-21T01:00:00.000Z' },
      { document_id: 'doc_01M679KMY6D75P0EZACBS95JYC', title: '发票.pdf', created_at: '2026-11-21T01:00:00.000Z' },
    ]
    const request = vi.fn(async (req: { op: string; documentId?: string }) => req.op === 'list'
      ? { ok: true, data: { documents: docs, next_cursor: null } }
      : { ok: true, data: { ...docs.find((d) => d.document_id === req.documentId), pages: req.documentId === B ? 4 : 2 } })
    const agentAccess = vi.fn().mockResolvedValue({ enabled: true, until: '2026-12-23T00:00:00.000Z', ok: true })
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: aside(mockBackendProxy), documents: { request, agentAccess } },
      writable: true,
      configurable: true,
    })
    const prompts: string[] = []
    mockBackendProxy.mockImplementation(async (req: { method: string; path: string; body?: { prompt?: string } }) => {
      if (req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: 's1' } }
      if (req.method === 'POST' && req.path === '/api/v1/runs') {
        prompts.push(req.body?.prompt ?? '')
        return { ok: true, status: 201, data: { id: 'r_del' } }
      }
      return { ok: true, status: 200, data: { status: 'completed', output: { text: '已删除' } } }
    })
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: '把《中文测试合同.pdf》删了' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    const card = await screen.findByRole('group', { name: '选择要删除的文档' }, { timeout: 3000 })
    expect(screen.getByText(/2 份文档都对得上《中文测试合同.pdf》/)).toBeInTheDocument()
    expect(within(card).getByText(/共 4 页/)).toBeInTheDocument()
    expect(card.textContent).not.toContain('doc_')
    expect(prompts).toEqual([])
    fireEvent.click(within(card).getAllByRole('button', { name: '删除这一份' })[1]!)
    await waitFor(() => expect(prompts).toHaveLength(1), { timeout: 3000 })
    expect(prompts[0]).toContain(`document_id: ${B}`)
    expect(prompts[0]).not.toContain(A)
    expect(prompts[0]).toContain('删除第 2 份')
  })

  // L6-F5: 「回到上一个版本」 never goes to the model: the App finds the version,
  // offers it on a card, and restores it over the version shown when clicked.
  it('restores the version before the current one from a card, never through the model', async () => {
    const A = 'doc_01M679KMY6D75P0EZACBS95JYA'
    const docs = [{ document_id: A, title: '合同.docx', created_at: '2026-11-20T01:00:00.000Z', head_revision: 2 }]
    const request = vi.fn(async (req: { op: string }) => req.op === 'list'
      ? { ok: true, data: { documents: docs, next_cursor: null } }
      : req.op === 'revisions'
        ? { ok: true, data: { document_id: A, head_revision: 2, revisions: [
          { revision: 2, created_at: '2026-11-23T02:00:00.000Z' }, { revision: 1, created_at: '2026-11-20T01:00:00.000Z' }] } }
        : { ok: true, data: { document_id: A, title: '合同.docx', revision: 3, previous_revision: 2, committed: true } })
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: aside(mockBackendProxy), documents: { request } },
      writable: true,
      configurable: true,
    })
    const runs: string[] = []
    mockBackendProxy.mockImplementation(async (req: { method: string; path: string }) => {
      if (req.method === 'POST') runs.push(req.path)
      return { ok: true, status: 200, data: {} }
    })
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: '把《合同.docx》回到上一个版本' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    const card = await screen.findByRole('group', { name: '恢复版本' }, { timeout: 3000 })
    expect(screen.getByText(/现在是第 2 版。要把第 1 版/)).toBeInTheDocument()
    expect(request).not.toHaveBeenCalledWith(expect.objectContaining({ op: 'restoreRevision' }))
    fireEvent.click(within(card).getByRole('button', { name: '恢复第 1 版' }))
    await screen.findByText(/已恢复：第 1 版的内容现在是《合同.docx》的第 3 版/, {}, { timeout: 3000 })
    expect(request).toHaveBeenCalledWith({ op: 'restoreRevision', documentId: A, revision: 1, expectRevision: 2 })
    expect(screen.queryByRole('button', { name: '恢复第 1 版' })).toBeNull()
    expect(runs).toEqual([])
  })

  // General 0.6.93: a turn that ran and failed (the local model stopped at
  // prefill) was sent; only one that never started reads 「未发送」.
  it.each([
    ['provider_unavailable', '本地模型暂时不可用', false],
    ['run_not_started', '这次回答没能开始', true],
  ])('a run that fails with %s: 「未发送」 shown %s', async (code, line, notSent) => {
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: aside(mockBackendProxy), knowledge: { status: vi.fn().mockResolvedValue({ enabled: true, active: true, endpoint: 'https://a.example/api/v1', knowledgeBaseIds: ['kb'] }) } },
      writable: true,
      configurable: true,
    })
    mockBackendProxy.mockImplementation(async (req: { method: string; path: string }) => {
      if (req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: 's1' } }
      if (req.method === 'POST' && req.path === '/api/v1/runs') {
        return code === 'run_not_started'
          ? { ok: false, status: 503, data: { error: { code, message: code } } }
          : { ok: true, status: 201, data: { id: 'r1' } }
      }
      return { ok: true, status: 200, data: { status: 'failed', error: { code, message: 'oMLX memory guard aborted' } } }
    })
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'hello' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText(new RegExp(line))).toBeInTheDocument(), { timeout: 3000 })
    expect(screen.queryByText('未发送') !== null).toBe(notSent)
  })

  // M3 (0.6.73): a stopped turn read 「回答失败：run_cancelled」 under 「未发送」.
  it('a stopped turn says so in words, with the code only under 详情, and was sent', async () => {
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: aside(mockBackendProxy), knowledge: { status: vi.fn().mockResolvedValue({ enabled: true, active: true, endpoint: 'https://a.example/api/v1', knowledgeBaseIds: ['kb'] }) } },
      writable: true,
      configurable: true,
    })
    mockBackendProxy.mockImplementation(async (req: { method: string; path: string }) => {
      if (req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: 's1' } }
      if (req.method === 'POST' && req.path === '/api/v1/runs') return { ok: true, status: 201, data: { id: 'r1' } }
      return { ok: true, status: 200, data: { status: 'cancelled', error: { code: 'run_cancelled', message: 'run_cancelled' } } }
    })
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'hello' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText('已停止这次回答。')).toBeInTheDocument(), { timeout: 3000 })
    expect(screen.queryByText(/回答失败/)).toBeNull()
    expect(screen.queryByText('未发送')).toBeNull()
    expect(screen.getByText('详情').closest('details')?.textContent).toContain('run_cancelled')
  })

  it('runs the first Knowledge turn of a new conversation in a new session', async () => {
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: aside(mockBackendProxy), knowledge: { status: vi.fn().mockResolvedValue({ enabled: true, active: true, endpoint: 'https://a.example/api/v1', knowledgeBaseIds: ['kb'] }) } },
      writable: true,
      configurable: true,
    })
    let sessions = 0
    mockBackendProxy.mockImplementation(async (req: { method: string; path: string; body?: { prompt?: string } }) => {
      if (req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: `s${++sessions}` } }
      if (req.method === 'POST' && req.path === '/api/v1/runs') return { ok: true, status: 201, data: { id: `r-${req.body?.prompt?.split('\n').pop()}` } }
      const text = req.path.endsWith('r-first') ? 'answer one' : 'answer two'
      return { ok: true, status: 200, data: { status: 'completed', output: { text }, knowledge: { searched: true, read_chunks: [] } } }
    })

    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'first' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText('answer one')).toBeInTheDocument(), { timeout: 3000 })

    await waitFor(() => expect(screen.getByRole('button', { name: '新对话' })).toBeEnabled())
    fireEvent.click(screen.getByRole('button', { name: '新对话' }))
    fireEvent.change(textarea, { target: { value: 'second' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText('answer two')).toBeInTheDocument(), { timeout: 3000 })
    expect(mockBackendProxy).toHaveBeenCalledWith(expect.objectContaining({ method: 'POST', path: '/api/v1/runs', body: { prompt: `${KNOWLEDGE_HINT}\nsecond`, session_id: 's2', knowledge_query: 'second' } }))
  })

  // Mutation: pass skill_id only while creating the run session (the first round) and the second expectation goes red.
  it('sends the chosen skill_id on every run of the conversation, including the second round', async () => {
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: aside(mockBackendProxy), knowledge: { status: vi.fn().mockResolvedValue({ enabled: true, active: true }) } },
      writable: true,
      configurable: true,
    })
    skillListing.respond = () => ({ ok: true, status: 200, data: { skills: [
      { skill_id: 'live', origin: 'user', semver: '1.0.0', generation: 1, active: true },
      { skill_id: 'draft', origin: 'imported', semver: '0.1.0', generation: 0, active: false },
    ] } })
    mockBackendProxy.mockImplementation(async (req: { method: string; path: string; body?: { prompt?: string } }) => {
      if (req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: 's1' } }
      if (req.method === 'POST' && req.path === '/api/v1/runs') return { ok: true, status: 201, data: { id: `r-${req.body?.prompt?.split('\n').pop()}` } }
      const text = req.path.endsWith('r-first') ? 'answer one' : 'answer two'
      return { ok: true, status: 200, data: { status: 'completed', output: { text }, knowledge: { searched: true, read_chunks: [] } } }
    })

    render(<ChatPage />)
    const select = screen.getByRole('combobox', { name: '技能' })
    fireEvent.focus(select)
    await waitFor(() => expect(screen.getByRole('option', { name: 'live' })).toBeInTheDocument())
    expect(screen.queryByRole('option', { name: 'draft' })).not.toBeInTheDocument()
    fireEvent.change(select, { target: { value: 'live' } })

    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'first' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText('answer one')).toBeInTheDocument(), { timeout: 3000 })
    fireEvent.change(textarea, { target: { value: 'second' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText('answer two')).toBeInTheDocument(), { timeout: 3000 })

    const runs = mockBackendProxy.mock.calls.map(([req]) => req).filter((req) => req.method === 'POST' && req.path === '/api/v1/runs')
    expect(runs.map((req) => req.body)).toEqual([
      { prompt: `${KNOWLEDGE_HINT}\nfirst`, session_id: 's1', skill_id: 'live', knowledge_query: 'first' },
      { prompt: `${KNOWLEDGE_HINT}\nsecond`, session_id: 's1', skill_id: 'live', knowledge_query: 'second' },
    ])
  })

  const skillList = (ids: string[]) => ({ ok: true, status: 200, data: { skills: ids.map((id) => ({ skill_id: id, origin: 'user', semver: '1.0.0', generation: 1, active: true })) } })
  const noKnowledge = () => Object.defineProperty(window, 'agent24', {
    value: { backendProxy: aside(mockBackendProxy), knowledge: { status: vi.fn().mockResolvedValue({ enabled: false, active: false }) } },
    writable: true,
    configurable: true,
  })
  const send = (text: string) => {
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: text } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
  }
  const posts = () => mockBackendProxy.mock.calls.map(([req]) => req).filter((req) => req.method === 'POST')

  // Mutation: route on `knowledge?.active` alone (drop `|| skill`) and the first expectation goes red.
  it('sends a skill turn to /runs with skill_id even when Knowledge is off, every round', async () => {
    noKnowledge()
    skillListing.respond = () => skillList(['live'])
    mockBackendProxy.mockImplementation(async (req: { method: string; path: string; body?: { prompt?: string } }) => {
      if (req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: 's1' } }
      if (req.method === 'POST' && req.path === '/api/v1/runs') return { ok: true, status: 201, data: { id: `r-${req.body?.prompt?.split('\n').pop()}` } }
      return { ok: true, status: 200, data: { status: 'completed', output: { text: `ans ${req.path.slice('/api/v1/runs/r-'.length)}` }, knowledge: null } }
    })
    render(<ChatPage />)
    const select = screen.getByRole('combobox', { name: '技能' })
    await waitFor(() => expect(screen.getByRole('option', { name: 'live' })).toBeInTheDocument())
    fireEvent.change(select, { target: { value: 'live' } })
    send('first')
    await waitFor(() => expect(screen.getByText('ans first')).toBeInTheDocument(), { timeout: 3000 })
    send('second')
    await waitFor(() => expect(screen.getByText('ans second')).toBeInTheDocument(), { timeout: 3000 })
    expect(posts().map((req) => [req.path, req.body.skill_id])).toEqual([
      ['/api/v1/sessions', undefined],
      ['/api/v1/runs', 'live'],
      ['/api/v1/runs', 'live'],
    ])
    expect(posts().some((req) => req.path === '/api/v1/chat')).toBe(false)
  })

  it('keeps /api/v1/chat when no skill is chosen and Knowledge is off', async () => {
    noKnowledge()
    skillListing.respond = () => skillList(['live'])
    mockBackendProxy.mockImplementation(async () => {
      return { ok: true, status: 200, data: { message: { content: 'plain reply' } } }
    })
    render(<ChatPage />)
    send('hello')
    await waitFor(() => expect(screen.getByText('plain reply')).toBeInTheDocument())
    expect(posts().map((req) => req.path)).toEqual(['/api/v1/chat'])
  })

  it('does not send a skill that left the active list: the picker drops it on refresh', async () => {
    noKnowledge()
    let active = ['live']
    skillListing.respond = () => skillList(active)
    mockBackendProxy.mockImplementation(async () => {
      return { ok: true, status: 200, data: { message: { content: 'plain reply' } } }
    })
    render(<ChatPage />)
    const select = screen.getByRole('combobox', { name: '技能' })
    await waitFor(() => expect(screen.getByRole('option', { name: 'live' })).toBeInTheDocument())
    fireEvent.change(select, { target: { value: 'live' } })
    active = []
    fireEvent.focus(select)
    await waitFor(() => expect(select).toHaveValue(''))
    expect(screen.getByRole('status')).toHaveTextContent('live')
    send('hello')
    await waitFor(() => expect(screen.getByText('plain reply')).toBeInTheDocument())
    expect(posts().map((req) => req.path)).toEqual(['/api/v1/chat'])
  })

  it('clears the choice and says so when the run is refused with skill_not_active', async () => {
    noKnowledge()
    skillListing.respond = () => skillList(['live'])
    mockBackendProxy.mockImplementation(async (req: { method: string; path: string }) => {
      if (req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: 's1' } }
      if (req.method === 'POST' && req.path === '/api/v1/runs') return { ok: false, status: 409, data: { error: { code: 'skill_not_active', message: 'x' } } }
      return { ok: true, status: 200, data: { message: { content: 'plain reply' } } }
    })
    render(<ChatPage />)
    const select = screen.getByRole('combobox', { name: '技能' })
    await waitFor(() => expect(screen.getByRole('option', { name: 'live' })).toBeInTheDocument())
    fireEvent.change(select, { target: { value: 'live' } })
    send('first')
    await waitFor(() => expect(screen.getByText(/技能“live”已不再生效/)).toBeInTheDocument(), { timeout: 3000 })
    expect(select).toHaveValue('')
    send('again')
    await waitFor(() => expect(screen.getByText('plain reply')).toBeInTheDocument())
  })

  // Mutation: in SkillPicker, call setConversationSkill(conversation, next) even when hasMessages
  // (attach mid-chat) and the first expectation goes red.
  it('starts a new conversation when a skill is chosen in a chat that has turns; the old one is left as it was', async () => {
    const save = vi.fn(async (record: { id: string }) => ({ id: record.id, archived_at: null }))
    Object.defineProperty(window, 'agent24', {
      value: {
        backendProxy: aside(mockBackendProxy),
        knowledge: { status: vi.fn().mockResolvedValue({ enabled: false, active: false }) },
        chatHistory: { list: vi.fn().mockResolvedValue([]), save, promoteRun: vi.fn(), setTitle: vi.fn().mockResolvedValue(null), get: vi.fn().mockResolvedValue(null) },
      },
      writable: true,
      configurable: true,
    })
    skillListing.respond = () => skillList(['live'])
    mockBackendProxy.mockImplementation(async (req: { method: string; path: string; body?: { prompt?: string } }) => {
      if (req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: 's1' } }
      if (req.method === 'POST' && req.path === '/api/v1/runs') return { ok: true, status: 201, data: { id: `r-${req.body?.prompt?.split('\n').pop()}` } }
      if (req.path.startsWith('/api/v1/runs/')) return { ok: true, status: 200, data: { status: 'completed', output: { text: `ans ${req.path.slice('/api/v1/runs/r-'.length)}` }, knowledge: null } }
      return { ok: true, status: 200, data: { message: { content: 'plain reply' } } }
    })
    render(<ChatPage />)
    await waitFor(() => expect(screen.getByRole('option', { name: 'live' })).toBeInTheDocument())
    send('old topic')
    await waitFor(() => expect(screen.getByText('plain reply')).toBeInTheDocument())
    await waitFor(() => expect(save).toHaveBeenCalled())
    const oldId = save.mock.calls[0][0].id
    const oldSaved = JSON.stringify(save.mock.calls[save.mock.calls.length - 1][0])

    const select = screen.getByRole('combobox', { name: '技能' })
    expect(screen.getByText('选技能会开新对话')).toBeInTheDocument()
    fireEvent.change(select, { target: { value: 'live' } })
    // A new, empty conversation with the skill preselected; nothing attached to the old one.
    await waitFor(() => expect(screen.queryByText('old topic')).not.toBeInTheDocument())
    expect(select).toHaveValue('live')
    expect(screen.queryByText('选技能会开新对话')).not.toBeInTheDocument()

    send('one')
    await waitFor(() => expect(screen.getByText('ans one')).toBeInTheDocument(), { timeout: 3000 })
    send('two')
    await waitFor(() => expect(screen.getByText('ans two')).toBeInTheDocument(), { timeout: 3000 })
    const runs = posts().filter((req) => req.path === '/api/v1/runs')
    expect(runs.map((req) => req.body)).toEqual([
      { prompt: 'one', session_id: 's1', skill_id: 'live' },
      { prompt: 'two', session_id: 's1', skill_id: 'live' },
    ])
    expect(posts().filter((req) => req.path === '/api/v1/chat' && req.body.session_id)).toHaveLength(1)
    // The old conversation's last saved record is untouched by anything that followed.
    const lastOld = [...save.mock.calls].reverse().find(([record]) => record.id === oldId)![0]
    expect(JSON.stringify(lastOld)).toBe(oldSaved)
  })
})

// M4 A2: a Knowledge turn whose run waits on an approval becomes a task of
// its conversation (promotionDue); one that just answers stays an answer.
describe('ChatPage promoting a Knowledge run to a task', () => {
  beforeEach(() => { setLocale('zh') })

  const page = (statuses: string[]) => {
    const promoteRun = vi.fn().mockResolvedValue(null)
    const save = vi.fn(async (record: { id: string }) => ({ id: record.id }))
    Object.defineProperty(window, 'agent24', {
      value: {
        backendProxy: aside(mockBackendProxy),
        knowledge: { status: vi.fn().mockResolvedValue({ enabled: true, active: true }) },
        chatHistory: { list: vi.fn().mockResolvedValue([]), save, promoteRun },
      },
      writable: true,
      configurable: true,
    })
    let polls = 0
    mockBackendProxy.mockImplementation(async (req: { method: string; path: string }) => {
      if (req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: 's1' } }
      if (req.method === 'POST' && req.path === '/api/v1/runs') return { ok: true, status: 201, data: { id: 'r1' } }
      const status = statuses[Math.min(polls++, statuses.length - 1)]
      return { ok: true, status: 200, data: { status, output: { text: 'done' }, knowledge: null } }
    })
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'do it' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    return { promoteRun, save }
  }

  it('promotes the run of its own conversation once it waits on an approval', async () => {
    const { promoteRun, save } = page(['awaiting_approval', 'completed'])
    await waitFor(() => expect(screen.getByText('done')).toBeInTheDocument(), { timeout: 4000 })
    const conversation = save.mock.calls[0][0].id
    expect(promoteRun).toHaveBeenCalledTimes(1)
    expect(promoteRun).toHaveBeenCalledWith(conversation, 'r1')
  })

  it('leaves a run that only answers an answer', async () => {
    const { promoteRun } = page(['running', 'completed'])
    await waitFor(() => expect(screen.getByText('done')).toBeInTheDocument(), { timeout: 4000 })
    expect(promoteRun).not.toHaveBeenCalled()
  })
})

// M4 A4: a Knowledge turn whose run is still going on after the wait is
// left to go on as the conversation's task, with a card; the next turn starts
// a new session (Knowledge's review of A4).
describe('ChatPage with Knowledge when the run outlasts the wait', () => {
  beforeEach(() => { setLocale('zh') })

  const page = (recorded: boolean) => {
    // main's summary is left to the page (null), as for a save main did not list
    let saved: Parameters<typeof chatHistorySummary>[0] | null = null
    const save = vi.fn(async (record: Parameters<typeof chatHistorySummary>[0]) => { saved = record; return null })
    const promoteRun = vi.fn(async (_id: string, runId: string) =>
      (recorded && saved ? chatHistorySummary({ ...saved, runs: [{ run_id: runId, kind: 'task', started_at: saved.created_at }] }) : null))
    Object.defineProperty(window, 'agent24', {
      value: {
        backendProxy: aside(mockBackendProxy),
        knowledge: { status: vi.fn().mockResolvedValue({ enabled: true, active: true }) },
        chatHistory: { list: vi.fn().mockResolvedValue([]), save, promoteRun },
      },
      writable: true,
      configurable: true,
    })
    const realNow = Date.now.bind(Date)
    const clock = { skew: 0 }
    vi.spyOn(Date, 'now').mockImplementation(() => realNow() + clock.skew)
    let sessions = 0
    const runs: { session_id: string }[] = []
    mockBackendProxy.mockImplementation(async (req: { method: string; path: string; body?: { session_id: string } }) => {
      if (req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: `s${++sessions}` } }
      if (req.method === 'POST' && req.path === '/api/v1/runs') { runs.push(req.body!); return { ok: true, status: 201, data: { id: `r${runs.length}` } } }
      if (req.path === '/api/v1/runs/r1/cancel') return { ok: true, status: 200, data: {} }
      if (req.path === '/api/v1/runs/r1') {
        // The first answer comes, then the wait is over.
        clock.skew = 11 * 60_000
        return { ok: true, status: 200, data: { status: 'running', output: null, knowledge: null } }
      }
      return { ok: true, status: 200, data: { status: 'completed', output: { text: 'second answer' }, knowledge: null } }
    })
    render(<ChatPage />)
    const send = (text: string) => {
      const textarea = screen.getByPlaceholderText(/输入消息/)
      fireEvent.change(textarea, { target: { value: text } })
      fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    }
    return { save, promoteRun, runs, clock, send }
  }
  afterEach(() => { vi.restoreAllMocks() })

  it('leaves the run going on as a task, says so, and asks the next turn in a new session', async () => {
    const { save, promoteRun, runs, clock, send } = page(true)
    send('long job')
    await waitFor(() => expect(screen.getByText(chatMessages.zh['task.ongoing'])).toBeInTheDocument(), { timeout: 4000 })
    const conversation = save.mock.calls[0][0].id
    expect(promoteRun).toHaveBeenCalledWith(conversation, 'r1')
    expect(mockBackendProxy).not.toHaveBeenCalledWith(expect.objectContaining({ path: '/api/v1/runs/r1/cancel' }))
    clock.skew = 0
    send('next')
    await waitFor(() => expect(screen.getByText('second answer')).toBeInTheDocument(), { timeout: 4000 })
    expect(runs.map(r => r.session_id)).toEqual(['s1', 's2'])
    // Each prompt sent can be handed to the assistant as a task of its own.
    expect(screen.getAllByRole('button', { name: chatMessages.zh['task.handOver'] })).toHaveLength(2)
  })

  it('stops a run it could not record as a task, rather than leave it running without a card (Codex A4 C1)', async () => {
    const { promoteRun, send } = page(false)
    send('long job')
    await waitFor(() => expect(mockBackendProxy).toHaveBeenCalledWith({ method: 'POST', path: '/api/v1/runs/r1/cancel' }), { timeout: 4000 })
    expect(promoteRun).toHaveBeenCalled()
    expect(screen.queryByText(chatMessages.zh['task.ongoing'])).not.toBeInTheDocument()
    expect(screen.getByText(/run_timeout/)).toBeInTheDocument()
  })

  it('does not record the run it stopped once main can write again (Codex A4 r2 C1)', async () => {
    const { promoteRun, clock, send } = page(true)
    // main cannot write this once: the page tries again after its next save.
    promoteRun.mockRejectedValueOnce(new Error('disk full'))
    send('long job')
    await waitFor(() => expect(screen.getByText(/run_timeout/)).toBeInTheDocument(), { timeout: 4000 })
    expect(mockBackendProxy).toHaveBeenCalledWith({ method: 'POST', path: '/api/v1/runs/r1/cancel' })
    // Later saves (the next turn's) promote nothing.
    clock.skew = 0
    send('next')
    await waitFor(() => expect(screen.getByText('second answer')).toBeInTheDocument(), { timeout: 4000 })
    expect(promoteRun).toHaveBeenCalledTimes(1)
  })
})

describe('ChatPage with Knowledge when the answer names sources the run did not read', () => {
  beforeEach(() => { setLocale('zh') })

  const answerWith = async (text: string, knowledge: unknown, prompt = 'Can the tap water be drunk during the boil water notice?') => {
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: aside(mockBackendProxy), knowledge: { status: vi.fn().mockResolvedValue({ enabled: true, active: true, endpoint: 'https://a.example/api/v1', knowledgeBaseIds: ['kb'] }) } },
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
    fireEvent.change(textarea, { target: { value: prompt } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText(text)).toBeInTheDocument(), { timeout: 3000 })
  }

  // UI polish 03 (David): a turn the system does not search first (writing,
  // 「记住…」, a document's rewrite or split) is not told it was not searched.
  it.each(['帮我写一段产品介绍', '记住我喜欢喝茶'])('no 「没有检索知识库」 under %s, which is not searched first', async (prompt) => {
    await answerWith('好的。', { searched: false, read_chunks: [] }, prompt)
    expect(mockBackendProxy).toHaveBeenCalledWith(expect.objectContaining({ path: '/api/v1/runs', body: expect.not.objectContaining({ knowledge_query: expect.anything() }) }))
    expect(screen.queryByText(chatMessages.zh['knowledge.notSearched'])).not.toBeInTheDocument()
  })

  it('says the answer did not search Knowledge when the run called no tool, and marks its sources', async () => {
    const made = 'b512455b-6df2-4419-8df1-72126627085c#22b86b23-4bce-4898-9e43-9c62411f54c9'
    await answerWith(`煮沸至少 1 分钟。来源：s01-02-zh-water-2025.pdf，${made}`, { searched: false, read_chunks: [] })
    expect(screen.queryByText(chatMessages.zh['knowledge.via'])).not.toBeInTheDocument()
    expect(screen.getByText(chatMessages.zh['knowledge.notSearched'])).toBeInTheDocument()
    expect(screen.getByText(`⚠️ 回答里提到的这些来源本次没有读到，不能当作依据：${made}`)).toBeInTheDocument()
  })

  it('marks only the sources that are not among the chunks the run read back', async () => {
    await answerWith('答案。来源：doc-7#chk-71；doc-9#chk-99', { searched: true, read_chunks: [{ knowledge_id: 'doc-7', chunk_id: 'chk-71' }] })
    expect(screen.getByText(chatMessages.zh['knowledge.via'])).toBeInTheDocument()
    expect(screen.getByText('⚠️ 回答里提到的这些来源本次没有读到，不能当作依据：doc-9#chk-99')).toBeInTheDocument()
  })
})

describe('ChatPage with Knowledge switched On but not in effect', () => {
  beforeEach(() => { setLocale('zh') })

  const askPlain = async (status: { enabled: boolean; active: boolean }) => {
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: aside(mockBackendProxy), knowledge: { status: vi.fn().mockResolvedValue({ ...status, endpoint: 'https://a.example/api/v1', knowledgeBaseIds: ['kb'] }) } },
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
      value: { backendProxy: aside(mockBackendProxy), knowledge: { status } },
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
      await waitFor(() => expect(mockBackendProxy).toHaveBeenCalledWith(expect.objectContaining({ body: expect.objectContaining({ prompt: `${KNOWLEDGE_HINT}\n${text}` }) })), { timeout: 3000 })
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
      value: { backendProxy: aside(mockBackendProxy), knowledge: { status: vi.fn().mockResolvedValue({ enabled: true, active: true, endpoint: 'https://a.example/api/v1', knowledgeBaseIds: ['kb'] }) } },
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
      value: { backendProxy: aside(mockBackendProxy), knowledge: { status: vi.fn().mockResolvedValue({ enabled: true, active: true, endpoint: 'https://a.example/api/v1', knowledgeBaseIds: ['kb'] }) } },
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
      { prompt: `${KNOWLEDGE_HINT}\nq`, session_id: 's1', knowledge_query: 'q' },
      { prompt: `${KNOWLEDGE_HINT}\nq`, session_id: 's1', idoris_headroom_confirmed: true },
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

// PR-Daemon #1909 ④: while a document conversation reads its document (page
// by page, before the model is asked), the page is busy: a second Enter sends
// nothing, and the one reply that comes lands in this conversation.
describe('ChatPage: reading the document before the turn', () => {
  beforeEach(() => { setLocale('zh') })
  it('is busy while the document is read: a second Enter sends nothing', async () => {
    const { askAboutDocument, resetDocContextForTests } = await import('./chat/doc-context')
    resetDocContextForTests()
    let release: (v: unknown) => void = () => {}
    const request = vi.fn(() => new Promise((r) => { release = r }))
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: aside(mockBackendProxy), documents: { request } },
      writable: true,
      configurable: true,
    })
    mockBackendProxy.mockResolvedValue({ ok: true, status: 200, data: { message: { content: 'answer' } } })
    askAboutDocument({ id: 'doc_1', title: '合同.pdf', revision: 1 })
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: '付款期限是多少' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(request).toHaveBeenCalled())
    fireEvent.change(textarea, { target: { value: '第二句' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    release({ ok: true, data: { blocks: [{ text: '每月 5 日前付款。' }], next_cursor: null } })
    await waitFor(() => expect(screen.getByText('answer')).toBeInTheDocument())
    const chats = mockBackendProxy.mock.calls.filter(([req]) => req.path === '/api/v1/chat')
    expect(chats).toHaveLength(1)
    expect(chats[0][0].body.messages.at(-1).content).toContain('付款期限是多少')
    resetDocContextForTests()
  })
})

// K2: a Knowledge server that did not answer is asked once more; still down,
// the turn is answered without Knowledge and says so.
describe('ChatPage: Knowledge server down (K2)', () => {
  beforeEach(() => { setLocale('zh') })
  it('answers without the knowledge run and says the knowledge base cannot be reached', async () => {
    const down = { enabled: true, active: true, endpoint: 'http://127.0.0.1:18282/api/v1', knowledgeBaseIds: ['kb'], hasKey: true, reachability: { state: 'unreachable', checkedAt: '2026-11-19T08:00:00Z' } }
    const check = vi.fn().mockResolvedValue(down)
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: aside(mockBackendProxy), knowledge: { status: vi.fn().mockResolvedValue(down), check } },
      writable: true,
      configurable: true,
    })
    mockBackendProxy.mockResolvedValue({ ok: true, status: 200, data: { message: { content: 'plain answer' } } })
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: '押金是多少' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText('plain answer')).toBeInTheDocument())
    expect(check).toHaveBeenCalledTimes(1)
    expect(screen.getByText(chatMessages.zh['knowledge.down.unreachable'])).toBeInTheDocument()
    expect(mockBackendProxy.mock.calls.some(([req]) => req.path === '/api/v1/runs')).toBe(false)
  })

  it('uses Knowledge as before once the fresh check finds the server back', async () => {
    const stale = { enabled: true, active: true, endpoint: 'https://a.example/api/v1', knowledgeBaseIds: ['kb'], hasKey: true, reachability: { state: 'unreachable', checkedAt: null } }
    const back = { ...stale, reachability: { state: 'ok', checkedAt: '2026-11-19T08:01:00Z' } }
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: aside(mockBackendProxy), knowledge: { status: vi.fn().mockResolvedValue(stale), check: vi.fn().mockResolvedValue(back) } },
      writable: true,
      configurable: true,
    })
    mockBackendProxy.mockImplementation(async (req: { method: string; path: string }) => {
      if (req.path === '/api/v1/sessions') return { ok: true, status: 201, data: { id: 's1' } }
      if (req.path === '/api/v1/runs') return { ok: true, status: 201, data: { id: 'run_1', status: 'queued' } }
      return { ok: true, status: 200, data: { status: 'completed', output: { text: 'from the knowledge base' }, knowledge: { searched: true, read_chunks: [] } } }
    })
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: '押金是多少' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(mockBackendProxy.mock.calls.some(([req]) => req.path === '/api/v1/runs')).toBe(true))
    expect(screen.queryByText(chatMessages.zh['knowledge.down.unreachable'])).not.toBeInTheDocument()
  })
})

// Loop 3 FU: 「《合同.pdf》的押金是多少」 outside a document's conversation reads that document.
describe('ChatPage: a question naming a document in 《》', () => {
  beforeEach(() => { setLocale('zh') })
  it('sends the named document with the question', async () => {
    const request = vi.fn(async (req: { op: string }) => {
      if (req.op === 'list') return { ok: true, data: { documents: [{ document_id: 'doc_1', title: '合同.pdf', head_revision: 1, media_type: 'application/pdf', created_at: '2026-11-19T00:00:00Z' }], next_cursor: null } }
      if (req.op === 'readText') return { ok: true, data: { blocks: [{ text: '押金为人民币 17,000 元。' }], next_cursor: null } }
      return { ok: false }
    })
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: aside(mockBackendProxy), documents: { request } },
      writable: true,
      configurable: true,
    })
    mockBackendProxy.mockResolvedValue({ ok: true, status: 200, data: { message: { content: '17,000 元' } } })
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: '《合同.pdf》的押金是多少' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText('17,000 元')).toBeInTheDocument())
    const chat = mockBackendProxy.mock.calls.find(([req]) => req.path === '/api/v1/chat')![0]
    const last = chat.body.messages.at(-1).content as string
    expect(last).toContain('押金为人民币 17,000 元。')
    expect(last).toContain('《合同.pdf》的押金是多少')
  })

  it('asks as it was when no document has that title', async () => {
    const request = vi.fn(async () => ({ ok: true, data: { documents: [], next_cursor: null } }))
    Object.defineProperty(window, 'agent24', {
      value: { backendProxy: aside(mockBackendProxy), documents: { request } },
      writable: true,
      configurable: true,
    })
    mockBackendProxy.mockResolvedValue({ ok: true, status: 200, data: { message: { content: 'ok' } } })
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: '《红楼梦》讲了什么' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(screen.getByText('ok')).toBeInTheDocument())
    const chat = mockBackendProxy.mock.calls.find(([req]) => req.path === '/api/v1/chat')![0]
    expect(chat.body.messages.at(-1).content).toBe('《红楼梦》讲了什么')
  })
})

// Loop 3 FU: an Enter that ends an input method's composition does not send.
describe('ChatPage: Enter while composing', () => {
  beforeEach(() => { setLocale('zh') })
  it('sends nothing on an Enter that ends a composition, and sends on the next plain Enter', async () => {
    mockBackendProxy.mockResolvedValue({ ok: true, status: 200, data: { message: { content: 'ok' } } })
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'What does the knowledge base say' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false, isComposing: true })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false, keyCode: 229 })
    expect(mockBackendProxy).not.toHaveBeenCalled()
    fireEvent.change(textarea, { target: { value: 'What does the knowledge base say about boiling water?' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(mockBackendProxy).toHaveBeenCalledTimes(1))
    expect(mockBackendProxy.mock.calls[0][0].body.messages.at(-1).content).toBe('What does the knowledge base say about boiling water?')
  })

  it('Loop 4: sends what the box holds when Enter comes before the last keys were rendered', async () => {
    mockBackendProxy.mockResolvedValue({ ok: true, status: 200, data: { message: { content: 'ok' } } })
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/) as HTMLTextAreaElement
    fireEvent.change(textarea, { target: { value: 'How long should water be' } })
    // The rest typed, but its change not rendered yet: the DOM is ahead of the state.
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')!.set!.call(textarea, 'How long should water be boiled to make it safe to drink?')
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => expect(mockBackendProxy).toHaveBeenCalledTimes(1))
    expect(mockBackendProxy.mock.calls[0][0].body.messages.at(-1).content).toBe('How long should water be boiled to make it safe to drink?')
  })
})

// F3 (Loop 6): one sentence extracts fields with the documents OS's extract
// job, shown as a card; an ordinary question about a field is not taken.
describe('ChatPage: one sentence extracts fields (F3)', () => {
  beforeEach(() => { setLocale('zh') })
  const request = vi.fn(async (req: { op: string }) => {
    if (req.op === 'get') return { ok: true, data: { document_id: 'doc_1', title: '合同.pdf', head: { revision: 1, media_type: 'application/pdf' } } }
    if (req.op === 'extract') return { ok: true, data: { job_id: 'job_1', status: 'succeeded', result: { extraction_id: 'ext_1' } } }
    if (req.op === 'getExtraction') return { ok: true, data: { next_cursor: null, values: [
      { key: 'f1', status: 'present', value: '张三', anchors: [{ block_id: 'p1/b2', page: 1, quote: '甲方：张三' }] },
      { key: 'f2', status: 'missing', missing_reason: 'not_in_document' },
    ] } }
    if (req.op === 'readText') return { ok: true, data: { blocks: [{ text: '甲方：张三' }], next_cursor: null } }
    return { ok: false }
  })
  const start = async (text: string) => {
    const { askAboutDocument, resetDocContextForTests } = await import('./chat/doc-context')
    resetDocContextForTests()
    request.mockClear()
    Object.defineProperty(window, 'agent24', { value: { backendProxy: aside(mockBackendProxy), documents: { request } }, writable: true, configurable: true })
    mockBackendProxy.mockResolvedValue({ ok: true, status: 200, data: { message: { content: 'model answer' } } })
    askAboutDocument({ id: 'doc_1', title: '合同.pdf', revision: 1 })
    render(<ChatPage />)
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: text } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
  }

  it('calls extract first and shows the card, a missing field said so', async () => {
    await start('把这份合同的甲方、开户银行抽出来')
    await waitFor(() => expect(screen.getByRole('group', { name: '抽取结果' })).toBeInTheDocument())
    expect(request.mock.calls.map(([r]) => r.op)).toEqual(['get', 'extract', 'getExtraction'])
    expect(screen.getByText('张三')).toBeInTheDocument()
    expect(screen.getByText('文档里没找到')).toBeInTheDocument()
    expect(mockBackendProxy.mock.calls.some(([req]) => req.path === '/api/v1/chat')).toBe(false)
    const { pendingDocFocus } = await import('./documents/doc-focus')
    fireEvent.click(screen.getByRole('button', { name: '第 1 页' }))
    expect(pendingDocFocus()).toEqual({ documentId: 'doc_1', page: 1, blockId: 'p1/b2' })
  })

  it('says in Chinese when the module has no model, the code only under 「详情」', async () => {
    request.mockImplementationOnce(async () => ({ ok: true, data: { document_id: 'doc_1', title: '合同.pdf', head: { revision: 1, media_type: 'application/pdf' } } }))
    request.mockImplementationOnce(async () => ({ ok: false, status: 503, error: { code: 'engine_unavailable', message: 'the kernel offers this module no model', details: { engine: 'local-model' } } }) as never)
    await start('把这份合同的甲方抽出来')
    await waitFor(() => expect(screen.getByText('没能完成抽取：本地模型没有分配给文档模块，暂时不能抽取。')).toBeInTheDocument())
    const detail = screen.getByText(/the kernel offers this module no model/)
    expect(detail.closest('details')).not.toBeNull()
  })

  // PR-Daemon #2019 ⑤: a turn taken for an extraction by mistake can still be answered.
  it('answers the same words with the model when the card says it was not an extraction', async () => {
    await start('把这份合同的甲方、开户银行抽出来')
    fireEvent.click(await screen.findByRole('button', { name: '不是要抽取？让助手回答' }))
    await waitFor(() => expect(screen.getByText('model answer')).toBeInTheDocument())
    expect(request.mock.calls.filter(([r]) => r.op === 'extract')).toHaveLength(1)
    const chat = mockBackendProxy.mock.calls.find(([req]) => req.path === '/api/v1/chat')![0]
    expect(chat.body.messages.at(-1).content).toContain('把这份合同的甲方、开户银行抽出来')
  })

  it('leaves 「这个字段怎么填」 to the model', async () => {
    await start('这个字段怎么填')
    await waitFor(() => expect(screen.getByText('model answer')).toBeInTheDocument())
    expect(request.mock.calls.some(([r]) => r.op === 'extract')).toBe(false)
  })
})
