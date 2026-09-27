// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react'
import { App, pickChatModel } from './App'
import type { ModuleManifest } from '../shared/ipc-types'

const mockBackendProxy = vi.fn()
const mockModulesList = vi.fn()
const mockGetAppVersion = vi.fn()
const mockOmlxDetect = vi.fn()
const mockOmlxStart = vi.fn()
const mockOmlxModels = vi.fn()

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

  Object.defineProperty(window, 'agent24', {
    value: {
      backendProxy: mockBackendProxy,
      modulesList: mockModulesList,
      getAppVersion: mockGetAppVersion,
      omlxDetect: mockOmlxDetect,
      omlxStart: mockOmlxStart,
      omlxModels: mockOmlxModels,
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
})
