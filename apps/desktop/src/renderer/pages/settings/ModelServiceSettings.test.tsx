// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { cleanup, render, screen, fireEvent, waitFor } from '@testing-library/react'
import { ModelServiceSettings } from './ModelServiceSettings'
import { setLocale } from '../../i18n'

const proxy = vi.fn()

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
  proxy.mockImplementation(async (req: { method: string; path?: string; body?: { model?: string } }) => {
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
  it('lists chat models from the daemon and saves the clicked one', async () => {
    render(<ModelServiceSettings />)
    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Qwen3-8B-4bit' })).toHaveAttribute('aria-pressed', 'true')
    })
    expect(proxy).toHaveBeenCalledWith({ method: 'GET', path: '/api/v1/models' })
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
