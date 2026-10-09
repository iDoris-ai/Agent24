// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react'
import { App, pickChatModel, extractDaemonDefaultModel } from './App'
import type { ModuleManifest } from '../shared/ipc-types'

const mockBackendProxy = vi.fn()
const mockModulesList = vi.fn()
const mockGetAppVersion = vi.fn()
const mockOmlxDetect = vi.fn()
const mockOmlxStart = vi.fn()
const mockOmlxModels = vi.fn()
const mockBackendEndpoint = vi.fn()

const helloManifest: ModuleManifest = {
  id: '@auraaihq/example-hello',
  version: '0.1.0',
  name: 'Hello',
  description: 'Test module',
  type: 'ui',
  permissions: ['llm'],
  navItem: { icon: '👋', label: 'Hello', route: '/modules/hello' },
}

beforeEach(() => {
  vi.clearAllMocks()
  mockGetAppVersion.mockResolvedValue('1.0.0')
  mockBackendProxy.mockResolvedValue({ ok: true, status: 200, data: { status: 'ok', ts: Date.now() } })
  mockModulesList.mockResolvedValue([])
  mockOmlxDetect.mockResolvedValue({ ok: true, models: ['Qwen3-8B-4bit'] })
  mockBackendEndpoint.mockResolvedValue({ port: 60128 })

  Object.defineProperty(window, 'agent24', {
    value: {
      backendProxy: mockBackendProxy,
      modulesList: mockModulesList,
      getAppVersion: mockGetAppVersion,
      omlxDetect: mockOmlxDetect,
      omlxStart: mockOmlxStart,
      omlxModels: mockOmlxModels,
      backendEndpoint: mockBackendEndpoint,
    },
    writable: true,
    configurable: true,
  })
})

describe('App', () => {
  it('renders built-in sidebar nav items', async () => {
    await act(async () => { render(<App />) })
    expect(screen.getAllByText('对话').length).toBeGreaterThan(0)
    expect(screen.getByText('工作台')).toBeInTheDocument()
    expect(screen.getByText('模型')).toBeInTheDocument()
    expect(screen.getAllByText('设置').length).toBeGreaterThan(0)
  })

  it('shows backend status after health check', async () => {
    await act(async () => { render(<App />) })
    await waitFor(() => {
      expect(screen.getByText(/后端服务运行中/)).toBeInTheDocument()
    })
  })

  // FU-93: the sidebar used to hardcode ":8765", but agent24d is spawned with
  // `--port 0` and actually listens on whatever port the OS handed it — the
  // demo saw 60128. Assert the REAL port from backendEndpoint(), not a guess.
  it('shows the daemon\'s real port, not a hardcoded one (FU-93)', async () => {
    mockBackendEndpoint.mockResolvedValue({ port: 60128 })
    await act(async () => { render(<App />) })
    await waitFor(() => {
      expect(screen.getByText('后端服务运行中 :60128')).toBeInTheDocument()
    })
    expect(screen.queryByText(/:8765/)).not.toBeInTheDocument()
  })

  it('shows plain "running" text when the port is not yet known', async () => {
    mockBackendEndpoint.mockResolvedValue(null)
    await act(async () => { render(<App />) })
    await waitFor(() => {
      expect(screen.getByText('后端服务运行中')).toBeInTheDocument()
    })
  })

  it('shows oMLX model label after detection', async () => {
    await act(async () => { render(<App />) })
    await waitFor(() => {
      expect(screen.getByText(/Qwen3-8B-4bit/)).toBeInTheDocument()
    })
  })

  // FU-89: a live model list whose first entry is an image-gen model must not
  // become the chat page's default — pickChatModel should skip it. Mutation
  // check: removing the filter (using `detected.models[0]` directly again)
  // turns this red, since it would show the FLUX model instead.
  it('skips a leading image-gen model and defaults to a chat model (FU-89)', async () => {
    mockOmlxDetect.mockResolvedValue({
      ok: true,
      models: ['FLUX.2-klein-4B-mflux-4bit', 'Qwen3-8B-4bit'],
    })
    await act(async () => { render(<App />) })
    await waitFor(() => {
      expect(screen.getByText(/Qwen3-8B-4bit/)).toBeInTheDocument()
    })
    expect(screen.queryByText(/FLUX/)).not.toBeInTheDocument()
  })

  it('shows a message instead of a model when only non-chat models are available (FU-89)', async () => {
    mockOmlxDetect.mockResolvedValue({ ok: true, models: ['FLUX.2-klein-4B-mflux-4bit', 'bge-m3'] })
    await act(async () => { render(<App />) })
    await waitFor(() => {
      expect(screen.getByText(/无可用对话模型/)).toBeInTheDocument()
    })
  })

  // Follow-up: a real demo's topbar showed an OCR model because the desktop
  // was guessing from its own (unfiltered-enough) oMLX list instead of
  // asking the daemon for its real default. Verifies BOTH halves of the fix:
  // the OCR/VL filter, and preferring the daemon's `default_model`.
  it('skips a leading OCR model and defaults to a chat model (FU-89 follow-up)', async () => {
    mockOmlxDetect.mockResolvedValue({
      ok: true,
      models: ['GLM-OCR-bf16', 'Qwen3-8B-4bit'],
    })
    await act(async () => { render(<App />) })
    await waitFor(() => {
      expect(screen.getByText(/Qwen3-8B-4bit/)).toBeInTheDocument()
    })
    expect(screen.queryByText(/GLM-OCR/)).not.toBeInTheDocument()
  })

  it('prefers the daemon\'s real default_model over the oMLX-detected fallback', async () => {
    mockOmlxDetect.mockResolvedValue({ ok: true, models: ['GLM-OCR-bf16'] }) // would otherwise show "无可用对话模型"
    mockBackendProxy.mockImplementation((req: { method: string; path: string }) => {
      if (req.path === '/api/v1/models') {
        return Promise.resolve({
          ok: true,
          status: 200,
          data: { models: [], default_model: 'Qwen3.6-35B-A3B-MLX-8bit' },
        })
      }
      return Promise.resolve({ ok: true, status: 200, data: { status: 'ok', ts: Date.now() } })
    })
    await act(async () => { render(<App />) })
    await waitFor(() => {
      expect(screen.getByText(/Qwen3\.6-35B-A3B-MLX-8bit/)).toBeInTheDocument()
    })
    expect(screen.queryByText(/无可用对话模型/)).not.toBeInTheDocument()
  })

  it('falls back to the oMLX-derived label when the daemon reports no default_model', async () => {
    // The generic beforeEach mock's /api/v1/models response (same as /health)
    // has no `default_model` key at all — this is exactly today's behavior.
    await act(async () => { render(<App />) })
    await waitFor(() => {
      expect(screen.getByText(/Qwen3-8B-4bit · oMLX/)).toBeInTheDocument()
    })
  })

  it('shows module nav items when modules with navItem are returned', async () => {
    mockModulesList.mockResolvedValue([helloManifest])

    await act(async () => { render(<App />) })
    await waitFor(() => {
      const helloNavBtns = screen.getAllByText('Hello')
      expect(helloNavBtns.length).toBeGreaterThan(0)
    })
  })

  it('navigates to settings page on click', async () => {
    await act(async () => { render(<App />) })
    // There are multiple '设置' elements (nav + topbar); click the nav button
    const navBtns = screen.getAllByText('设置')
    fireEvent.click(navBtns[0])
    await waitFor(() => {
      expect(screen.getByText('Settings')).toBeInTheDocument()
    })
  })

  it('the Documents nav entry opens the documents page through its typed channel (#705)', async () => {
    const request = vi.fn((req: { op: string }) =>
      Promise.resolve(
        req.op === 'list'
          ? { ok: true, status: 200, data: { documents: [], next_cursor: null } }
          : { ok: true, status: 200, data: { storage: { state: 'ready' } } },
      ),
    )
    ;(window.agent24 as unknown as Record<string, unknown>).documents = {
      request,
      importFile: vi.fn(),
      onImportProgress: () => () => {},
    }
    await act(async () => { render(<App />) })
    fireEvent.click(screen.getByRole('button', { name: /文档/ }))
    await waitFor(() => expect(screen.getByText('还没有文档。')).toBeInTheDocument())
    expect(request).toHaveBeenCalledWith({ op: 'list' })
    // The page never falls back to the generic proxy for documents.
    expect(mockBackendProxy.mock.calls.some(([r]) => String(r.path).includes('/documents'))).toBe(false)
  })

  it('toggles sidebar collapse', async () => {
    await act(async () => { render(<App />) })
    const collapseBtn = screen.getByText('‹')
    fireEvent.click(collapseBtn)
    await waitFor(() => {
      expect(screen.getByText('›')).toBeInTheDocument()
    })
  })

  it('shows offline status when backend is unreachable', async () => {
    mockBackendProxy.mockResolvedValue({ ok: false, status: 503, data: null })

    await act(async () => { render(<App />) })
    await waitFor(() => {
      expect(screen.getByText('后端服务离线')).toBeInTheDocument()
    })
  })

  it('starts oMLX when not detected', async () => {
    mockOmlxDetect.mockResolvedValue(null)
    mockOmlxStart.mockResolvedValue({ ok: false, url: '' })

    await act(async () => { render(<App />) })
    await waitFor(() => {
      expect(mockOmlxStart).toHaveBeenCalled()
    })
  })

  it('renders module page when module nav item clicked', async () => {
    mockModulesList.mockResolvedValue([helloManifest])
    mockBackendProxy
      .mockResolvedValueOnce({ ok: true, status: 200, data: { status: 'ok', ts: Date.now() } }) // health
      .mockResolvedValue({ ok: true, status: 200, data: { moduleId: 'hello', description: 'test' } }) // info

    await act(async () => { render(<App />) })
    await waitFor(() => screen.getAllByText('Hello'))

    await act(async () => {
      const helloBtns = screen.getAllByText('Hello')
      fireEvent.click(helloBtns[0])
    })
    await waitFor(() => {
      expect(screen.getByText('Hello Module')).toBeInTheDocument()
    })
  })

  it('shows No AI runtime when oMLX not detected and start fails', async () => {
    mockOmlxDetect.mockResolvedValue(null)
    mockOmlxStart.mockResolvedValue({ ok: false, url: '' })

    await act(async () => { render(<App />) })
    await waitFor(() => {
      expect(screen.getByText('No AI runtime')).toBeInTheDocument()
    })
  })

  // M1-T14: switching to another page and back used to UNMOUNT ChatPage
  // (`{page === 'chat' && <ChatPage />}`), which reset its message list and
  // minted a fresh `session_id` — silently starting a new D1 memory session
  // on every round trip through another page. ChatPage must now stay
  // mounted (hidden via `display: none`) so a conversation survives
  // navigation.
  it('keeps the chat conversation and session_id across navigating away and back (M1-T14)', async () => {
    mockBackendProxy.mockImplementation((req: { method: string; path: string }) => {
      if (req.path === '/api/v1/chat') {
        return Promise.resolve({ ok: true, status: 200, data: { message: { content: 'ack' } } })
      }
      return Promise.resolve({ ok: true, status: 200, data: { status: 'ok', ts: Date.now() } })
    })

    await act(async () => { render(<App />) })
    const textarea = screen.getByPlaceholderText(/输入消息/)
    fireEvent.change(textarea, { target: { value: 'hello there' } })
    fireEvent.keyDown(textarea, { key: 'Enter', shiftKey: false })
    await waitFor(() => {
      expect(screen.getByText('hello there')).toBeInTheDocument()
      expect(screen.getByText('ack')).toBeInTheDocument()
    })
    const firstSessionId = mockBackendProxy.mock.calls.find(
      (c) => c[0].path === '/api/v1/chat',
    )?.[0].body.session_id
    expect(firstSessionId).toBeTruthy()

    // Navigate to 记忆 and back to 对话. ChatPage stays MOUNTED the whole
    // time (just hidden) — `hello there` is still findable in the DOM, so
    // the real assertion is on visibility, not presence.
    fireEvent.click(screen.getAllByText('记忆')[0])
    await waitFor(() => {
      expect(screen.getByText('hello there')).not.toBeVisible()
    })
    fireEvent.click(screen.getAllByText('对话')[0])

    // The message list and session id must be exactly as they were.
    await waitFor(() => {
      expect(screen.getByText('hello there')).toBeVisible()
      expect(screen.getByText('ack')).toBeVisible()
    })

    mockBackendProxy.mockClear()
    fireEvent.change(screen.getByPlaceholderText(/输入消息/), { target: { value: 'second message' } })
    fireEvent.keyDown(screen.getByPlaceholderText(/输入消息/), { key: 'Enter', shiftKey: false })
    await waitFor(() => {
      expect(mockBackendProxy).toHaveBeenCalledWith(
        expect.objectContaining({ path: '/api/v1/chat' }),
      )
    })
    const secondSessionId = mockBackendProxy.mock.calls.find(
      (c) => c[0].path === '/api/v1/chat',
    )?.[0].body.session_id
    expect(secondSessionId).toBe(firstSessionId)
  })
})

describe('pickChatModel (FU-89)', () => {
  it('picks the first model that is not a known non-chat family', () => {
    expect(pickChatModel(['FLUX.2-klein-4B-mflux-4bit', 'Qwen3-8B-4bit'])).toBe('Qwen3-8B-4bit')
    expect(pickChatModel(['Qwen3-8B-4bit', 'FLUX.2-klein-4B-mflux-4bit'])).toBe('Qwen3-8B-4bit')
  })

  it('skips image, video, ASR/TTS, embedding and rerank families', () => {
    const nonChat = [
      'FLUX.1-schnell', 'stable-diffusion-xl', 'LTX-2.3', 'mlx-whisper-large-v3',
      'CosyVoice2-0.5B', 'bge-m3', 'ModernBERT-reranker',
    ]
    for (const m of nonChat) expect(pickChatModel([m])).toBeUndefined()
  })

  it('returns undefined when nothing looks like a chat model', () => {
    expect(pickChatModel(['FLUX.1-schnell', 'bge-m3'])).toBeUndefined()
  })

  it('returns undefined for an empty list', () => {
    expect(pickChatModel([])).toBeUndefined()
  })

  // FU-89 follow-up: a real demo's topbar defaulted to an OCR model because
  // the filter list didn't cover OCR/vision-language families.
  it('skips OCR / vision-language models even when listed first', () => {
    expect(pickChatModel(['GLM-OCR-bf16', 'Qwen3.6-35B-A3B-MLX-8bit'])).toBe('Qwen3.6-35B-A3B-MLX-8bit')
    expect(pickChatModel(['PaddleOCR-VL-1.6-4bit', 'Qwen3-8B-4bit'])).toBe('Qwen3-8B-4bit')
    expect(pickChatModel(['Mage-VL', 'Qwen3-8B-4bit'])).toBe('Qwen3-8B-4bit')
  })

  it('returns undefined when only OCR/VL models are available', () => {
    expect(pickChatModel(['GLM-OCR-bf16', 'PaddleOCR-VL-1.6-4bit', 'Mage-VL'])).toBeUndefined()
  })
})

describe('extractDaemonDefaultModel', () => {
  it('returns the default_model string when present', () => {
    expect(extractDaemonDefaultModel({ ok: true, data: { default_model: 'Qwen3.6-35B-A3B-MLX-8bit' } })).toBe(
      'Qwen3.6-35B-A3B-MLX-8bit',
    )
  })

  it('returns null when the response failed, or default_model is absent/null/blank', () => {
    expect(extractDaemonDefaultModel({ ok: false, data: { default_model: 'x' } })).toBeNull()
    expect(extractDaemonDefaultModel({ ok: true, data: {} })).toBeNull()
    expect(extractDaemonDefaultModel({ ok: true, data: { default_model: null } })).toBeNull()
    expect(extractDaemonDefaultModel({ ok: true, data: { default_model: '  ' } })).toBeNull()
    expect(extractDaemonDefaultModel({ ok: true, data: null })).toBeNull()
  })
})
