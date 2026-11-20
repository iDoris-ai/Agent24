// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { cleanup, render, screen, fireEvent, waitFor } from '@testing-library/react'
import { ModelServiceSettings } from './ModelServiceSettings'
import { setLocale } from '../../i18n'

const proxy = vi.fn()
let localModels: unknown = null
let recommendation: unknown = { model_id: null }

const daemonModels = {
  models: [
    { id: 'FLUX.1-schnell', provider: 'local', tier: 'local', loaded: false },
    { id: 'Qwen3-8B-4bit', provider: 'local', tier: 'local', loaded: true },
    { id: 'CosyVoice2', provider: 'local', tier: 'local', loaded: false },
    { id: 'Qwen3-30B', provider: 'local', tier: 'local', loaded: true },
  ],
  default_model: 'Qwen3-8B-4bit',
}

beforeEach(() => {
  cleanup()
  setLocale('en')
  proxy.mockReset()
  localModels = null
  recommendation = { model_id: null }
  proxy.mockImplementation(async (req: { method: string; path?: string; body?: { model?: string } }) => {
    if (req.path === '/api/v1/models/local') return { ok: true, data: { models: localModels } }
    if (req.path === '/api/v1/models/recommendation?role=chat') return { ok: true, data: { recommendation } }
    if (req.method === 'GET' && req.path === '/api/v1/models') {
      return { ok: true, status: 200, data: daemonModels }
    }
    if (req.method === 'GET') return { ok: false, status: 404, data: null }
    return { ok: true, status: 200, data: { model: req.body?.model } }
  })
  Object.defineProperty(window, 'agent24', {
    configurable: true,
    value: { backendProxy: proxy },
  })
})

describe('ModelServiceSettings default chat model', () => {
  it('uses iDoris kinds instead of name heuristics, including an empty inventory', async () => {
    localModels = ['chat', 'vision', 'embedding', 'rerank', 'image', 'ocr', 'tts', 'unknown'].map(kind => ({
      omlx_id: kind === 'vision' ? 'Qwen3-VL' : `model-${kind}`, kind, size_bytes: null, catalog_id: null,
    }))
    const view = render(<ModelServiceSettings />)
    expect(await screen.findByRole('button', { name: 'Qwen3-VL' })).toBeInTheDocument()
    // Model rows are the labelled buttons; the model-source panel's own controls are not.
    const modelButtons = () => screen.queryAllByRole('button').filter(button => button.hasAttribute('aria-label'))
    expect(modelButtons().map(button => button.getAttribute('aria-label'))).toEqual(['model-chat', 'Qwen3-VL'])
    view.unmount()
    localModels = []
    render(<ModelServiceSettings />)
    expect(await screen.findByText('The daemon has not reported a chat model.')).toBeInTheDocument()
    expect(modelButtons()).toEqual([])
  })

  it.each([
    ['tier_below_16gib', 'Suitable for devices with less than 16 GiB of memory.'],
    ['tier_16_48gib', 'Suitable for devices with 16–48 GiB of memory.'],
    ['tier_48_64gib', 'Suitable for devices with 48–64 GiB of memory.'],
    ['tier_64gib_plus', 'Suitable for devices with 64 GiB of memory or more.'],
    ['fallback_does_not_fit', 'The fallback model may exceed available memory.'],
    ['insufficient_memory', 'There is not enough memory for a recommended model.'],
    ['idoris_unavailable', 'iDoris is unavailable; showing the fallback recommendation.'],
  ])('marks an installed recommendation and translates %s', async (reason, expected) => {
    recommendation = { model_id: 'Qwen3-8B-4bit', installed: true, size_bytes: null, reason }
    render(<ModelServiceSettings />)
    expect(await screen.findByText('Recommended')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Qwen3-8B-4bit' })).toHaveTextContent('Recommended')
    expect(screen.getByText(expected)).toBeInTheDocument()
  })

  it.each([
    [5_000_000_000, '未下载 · 约 5.0 GB'],
    [null, '未下载 · 体积未知'],
  ])('shows an unavailable recommendation with size %s without offering download or selection', async (sizeBytes, expected) => {
    setLocale('zh')
    recommendation = { model_id: 'new-chat-model', installed: false, size_bytes: sizeBytes, reason: 'future_reason' }
    render(<ModelServiceSettings />)
    expect(await screen.findByText('推荐')).toBeInTheDocument()
    expect(screen.getByText(expected)).toBeInTheDocument()
    expect(screen.getByText('下载功能即将提供')).toBeInTheDocument()
    expect(screen.getByText('future_reason')).toBeInTheDocument()
    fireEvent.click(screen.getByText('new-chat-model'))
    expect(screen.queryByRole('button', { name: 'new-chat-model' })).not.toBeInTheDocument()
    expect(proxy).not.toHaveBeenCalledWith(expect.objectContaining({ method: 'PUT' }))
  })

  it('hides recommendations with no model id', async () => {
    render(<ModelServiceSettings />)
    await screen.findByRole('button', { name: 'Qwen3-8B-4bit' })
    expect(proxy).toHaveBeenCalledWith({ method: 'GET', path: '/api/v1/models/recommendation?role=chat' })
    expect(screen.queryByText('Recommended')).not.toBeInTheDocument()
  })

  it('falls back to name heuristics when local models are null and saves the clicked model', async () => {
    render(<ModelServiceSettings />)
    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Qwen3-8B-4bit' })).toHaveAttribute('aria-pressed', 'true')
    })
    expect(proxy).toHaveBeenCalledWith({ method: 'GET', path: '/api/v1/models' })
    expect(proxy).toHaveBeenCalledWith({ method: 'GET', path: '/api/v1/models/local' })
    expect(screen.queryByText('FLUX.1-schnell')).not.toBeInTheDocument()
    expect(screen.queryByText('CosyVoice2')).not.toBeInTheDocument()
    expect(screen.getByText('Local model service (managed by iDoris)')).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: 'Qwen3-30B' }))
    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Qwen3-30B' })).toHaveAttribute('aria-pressed', 'true')
    })
    expect(proxy).toHaveBeenCalledWith({
      method: 'PUT',
      path: '/api/v1/models/default',
      body: { model: 'Qwen3-30B' },
    })
  })

  it('shows the save error and keeps the previous selection', async () => {
    proxy.mockImplementation(async (req: { method: string; path?: string }) => {
      if (req.method === 'GET' && req.path === '/api/v1/models') {
        return { ok: true, status: 200, data: daemonModels }
      }
      if (req.method === 'GET') return { ok: false, status: 404, data: null }
      return { ok: false, status: 400, data: {} }
    })
    render(<ModelServiceSettings />)
    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Qwen3-8B-4bit' })).toHaveAttribute('aria-pressed', 'true')
    })
    fireEvent.click(screen.getByRole('button', { name: 'Qwen3-30B' }))
    expect(await screen.findByText('Could not save the default chat model.')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Qwen3-8B-4bit' })).toHaveAttribute('aria-pressed', 'true')
  })

  it('does not offer a model list while the daemon is offline', async () => {
    proxy.mockResolvedValue({ ok: false, status: 503, data: null })
    render(<ModelServiceSettings />)
    expect(await screen.findByText(/daemon is offline/)).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Qwen3-30B' })).not.toBeInTheDocument()
    expect(proxy).not.toHaveBeenCalledWith(expect.objectContaining({ method: 'PUT' }))
  })
})
