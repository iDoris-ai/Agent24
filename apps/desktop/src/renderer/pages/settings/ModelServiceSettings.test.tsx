// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { cleanup, render, screen, fireEvent, waitFor } from '@testing-library/react'
import { ModelServiceSettings } from './ModelServiceSettings'
import { setLocale } from '../../i18n'

const models = ['FLUX.1-schnell', 'Qwen3-8B-4bit', 'CosyVoice2', 'Qwen3-30B']
const detect = vi.fn()
const proxy = vi.fn()

beforeEach(() => {
  cleanup()
  setLocale('en')
  detect.mockReset()
  proxy.mockReset()
  detect.mockResolvedValue({ url: 'http://127.0.0.1:8088', apiKey: '', models })
  proxy.mockImplementation(async (req: { method: string; body?: { model?: string } }) => {
    if (req.method === 'GET') {
      return { ok: true, status: 200, data: { models: [], default_model: 'Qwen3-8B-4bit' } }
    }
    return { ok: true, status: 200, data: { model: req.body?.model } }
  })
  Object.defineProperty(window, 'agent24', {
    configurable: true,
    value: {
      omlxDetect: detect,
      omlxStart: vi.fn(),
      omlxModels: vi.fn(),
      omlxStop: vi.fn(),
      omlxWarmup: vi.fn(),
      backendProxy: proxy,
    },
  })
})

describe('ModelServiceSettings default chat model', () => {
  it('lists only chat models and saves the clicked one', async () => {
    render(<ModelServiceSettings />)
    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Qwen3-8B-4bit' })).toHaveAttribute('aria-pressed', 'true')
    })
    expect(screen.queryByText('FLUX.1-schnell')).not.toBeInTheDocument()
    expect(screen.queryByText('CosyVoice2')).not.toBeInTheDocument()

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
    proxy.mockImplementation(async (req: { method: string }) => {
      if (req.method === 'GET') {
        return { ok: true, status: 200, data: { default_model: 'Qwen3-8B-4bit' } }
      }
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

  it('does not select a model while the daemon is offline', async () => {
    proxy.mockResolvedValue({ ok: false, status: 503, data: null })
    render(<ModelServiceSettings />)
    const row = await screen.findByRole('button', { name: 'Qwen3-30B' })
    expect(row).toHaveAttribute('aria-disabled', 'true')
    expect(screen.getByText(/daemon is offline/)).toBeInTheDocument()
    fireEvent.click(row)
    expect(proxy).not.toHaveBeenCalledWith(expect.objectContaining({ method: 'PUT' }))
  })
})
