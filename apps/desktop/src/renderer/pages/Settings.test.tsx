// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react'
import SettingsPage from './Settings'
import { setLocale } from '../i18n'

const mockOmlxDetect = vi.fn()
const mockOmlxStart = vi.fn()
const mockOmlxModels = vi.fn()
const mockOmlxStop = vi.fn()
const mockOmlxWarmup = vi.fn()

beforeEach(() => {
  vi.clearAllMocks()
  Object.defineProperty(window, 'agent24', {
    value: {
      omlxDetect: mockOmlxDetect,
      omlxStart: mockOmlxStart,
      omlxModels: mockOmlxModels,
      omlxStop: mockOmlxStop,
      omlxWarmup: mockOmlxWarmup,
    },
    writable: true,
    configurable: true,
  })
})

describe('SettingsPage', () => {
  it('renders settings title', async () => {
    mockOmlxDetect.mockResolvedValue(null)
    mockOmlxStart.mockResolvedValue({ ok: false, url: '', error: 'not found' })
    await act(async () => { render(<SettingsPage />) })
    expect(screen.getByText('Settings')).toBeInTheDocument()
  })

  it('shows Connected when oMLX detected', async () => {
    mockOmlxDetect.mockResolvedValue({
      url: 'http://127.0.0.1:8088',
      apiKey: 'test',
      models: ['Qwen3-8B-4bit', 'Qwen3-30B'],
    })
    await act(async () => { render(<SettingsPage />) })
    await waitFor(() => {
      expect(screen.getByText('Connected')).toBeInTheDocument()
    })
    expect(screen.getByText(/2 model/)).toBeInTheDocument()
  })

  it('shows Disconnected when oMLX not reachable', async () => {
    mockOmlxDetect.mockResolvedValue(null)
    mockOmlxStart.mockResolvedValue({ ok: false, url: '', error: 'Failed' })
    await act(async () => { render(<SettingsPage />) })
    await waitFor(() => {
      expect(screen.getByText('Disconnected')).toBeInTheDocument()
    })
  })

  it('auto-detect button triggers re-detection', async () => {
    mockOmlxDetect.mockResolvedValueOnce(null).mockResolvedValueOnce({
      url: 'http://127.0.0.1:8088',
      apiKey: 'test',
      models: ['Qwen3-8B'],
    })
    mockOmlxStart.mockResolvedValue({ ok: false, url: '', error: '' })

    await act(async () => { render(<SettingsPage />) })
    await waitFor(() => screen.getByText('Disconnected'))

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Auto.?Detect/i }))
    })
    await waitFor(() => {
      expect(screen.getByText('Connected')).toBeInTheDocument()
    })
  })

  it('stop button calls omlxStop', async () => {
    mockOmlxDetect.mockResolvedValue({
      url: 'http://127.0.0.1:8088',
      apiKey: 'test',
      models: ['Qwen3-8B'],
    })
    mockOmlxStop.mockResolvedValue(undefined)

    await act(async () => { render(<SettingsPage />) })
    await waitFor(() => screen.getByText('Connected'))

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Stop/i }))
    })
    expect(mockOmlxStop).toHaveBeenCalled()
    await waitFor(() => {
      expect(screen.getByText('Disconnected')).toBeInTheDocument()
    })
  })

  it('test connection shows result', async () => {
    mockOmlxDetect.mockResolvedValue({
      url: 'http://127.0.0.1:8088',
      apiKey: 'test',
      models: ['Qwen3-8B'],
    })
    mockOmlxModels.mockResolvedValue({ ok: true, models: ['Qwen3-8B'] })

    await act(async () => { render(<SettingsPage />) })
    await waitFor(() => screen.getByText('Connected'))

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Test/i }))
    })
    await waitFor(() => {
      expect(screen.getByText(/Connected — 1 model/)).toBeInTheDocument()
    })
  })

  it('start oMLX button calls omlxStart', async () => {
    // First call in initialize() fails, so page lands in Disconnected
    mockOmlxDetect.mockResolvedValue(null)
    mockOmlxStart.mockResolvedValue({ ok: false, url: '', error: 'Failed' })

    await act(async () => { render(<SettingsPage />) })
    await waitFor(() => {
      expect(screen.getByText('Disconnected')).toBeInTheDocument()
    })

    // Now clicking "Start oMLX" calls startServer → omlxStart again
    mockOmlxStart.mockResolvedValue({ ok: false, url: '', error: 'Still failing' })
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Start oMLX' }))
    })
    // omlxStart called twice total: once from initialize(), once from startServer()
    expect(mockOmlxStart).toHaveBeenCalledTimes(2)
  })

  it('setCategory updates model category to resident and warms up', async () => {
    mockOmlxDetect.mockResolvedValue({
      url: 'http://127.0.0.1:8088',
      apiKey: 'test',
      models: ['Qwen3-8B'],
    })
    mockOmlxWarmup.mockResolvedValue(undefined)

    await act(async () => { render(<SettingsPage />) })
    // Wait for models to appear (which means connected + models loaded)
    await waitFor(() => screen.getByText('Qwen3-8B'))

    // Find all Resident buttons (one per model + one in the description text)
    const residentBtns = screen.getAllByRole('button').filter(b => b.textContent === 'Resident')
    expect(residentBtns.length).toBeGreaterThan(0)
    await act(async () => {
      fireEvent.click(residentBtns[0])
    })
    expect(mockOmlxWarmup).toHaveBeenCalledWith('http://127.0.0.1:8088', 'test', 'Qwen3-8B')
  })

  it('toggle API key visibility', async () => {
    mockOmlxDetect.mockResolvedValue(null)
    mockOmlxStart.mockResolvedValue({ ok: false, url: '' })

    await act(async () => { render(<SettingsPage />) })
    const eyeBtn = screen.getByTitle(/Show|Hide/)
    fireEvent.click(eyeBtn)
    expect(screen.getByTitle('Hide')).toBeInTheDocument()
  })
})

describe('SettingsPage English default', () => {
  it('shows the English service section', async () => {
    mockOmlxDetect.mockResolvedValue(null)
    mockOmlxStart.mockResolvedValue({ ok: false, url: '', error: 'not found' })
    await act(async () => { render(<SettingsPage />) })
    expect(screen.getByText('AI Model Service')).toBeInTheDocument()
  })

  it('relabels a detection error when the language changes (not frozen in the old language)', async () => {
    mockOmlxDetect.mockResolvedValue(null)
    mockOmlxStart.mockResolvedValue({ ok: false, url: '', error: 'not found' })
    await act(async () => { render(<SettingsPage />) })
    await waitFor(() => expect(screen.getByText(/Could not start or connect/)).toBeInTheDocument())
    await act(async () => { setLocale('zh') })
    expect(screen.getByText(/无法启动或连接 AI 模型服务/)).toBeInTheDocument()
  })
})

describe('SettingsPage: oMLX needs an API key (401) vs really down', () => {
  beforeEach(() => { setLocale('en') })

  it('detect says authRequired: explains the key is needed, points at the key field, does not auto-start', async () => {
    mockOmlxDetect.mockResolvedValue({ authRequired: true, url: 'http://127.0.0.1:8088' })
    await act(async () => { render(<SettingsPage />) })
    expect(await screen.findByText(/oMLX is running but requires an API key/)).toBeInTheDocument()
    expect(mockOmlxStart).not.toHaveBeenCalled()
    const keyField = screen.getByLabelText('API Key')
    expect(keyField).toHaveAttribute('aria-invalid', 'true')
    expect(keyField).toHaveFocus()
    // not the generic "could not start or connect" message
    expect(screen.queryByText(/Could not start or connect/)).toBeNull()
  })

  it('really down (detect null, start fails): still the generic connect error, key field not flagged', async () => {
    mockOmlxDetect.mockResolvedValue(null)
    mockOmlxStart.mockResolvedValue({ ok: false, url: '', error: 'nope' })
    await act(async () => { render(<SettingsPage />) })
    expect(await screen.findByText(/Could not start or connect/)).toBeInTheDocument()
    expect(screen.queryByText(/requires an API key/)).toBeNull()
    expect(screen.getByLabelText('API Key')).toHaveAttribute('aria-invalid', 'false')
  })

  it('Test button: 401 -> key-needed message; other failure -> plain failure text', async () => {
    mockOmlxDetect.mockResolvedValue(null)
    mockOmlxStart.mockResolvedValue({ ok: false, url: '', error: '' })
    await act(async () => { render(<SettingsPage />) })
    await screen.findByText(/Could not start or connect/)

    mockOmlxModels.mockResolvedValueOnce({ ok: false, models: [], authRequired: true, error: 'authentication required' })
    await act(async () => { fireEvent.click(screen.getByRole('button', { name: /^Test$/ })) })
    expect(await screen.findByText(/requires an API key/)).toBeInTheDocument()

    mockOmlxModels.mockResolvedValueOnce({ ok: false, models: [], error: 'no response or no models' })
    await act(async () => { fireEvent.click(screen.getByRole('button', { name: /^Test$/ })) })
    expect(await screen.findByText(/no response or no models/)).toBeInTheDocument()
  })
})
