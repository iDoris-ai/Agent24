// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { cleanup, render, screen, fireEvent, waitFor, act, within } from '@testing-library/react'
import { useState, type ReactNode } from 'react'
import SettingsPage, { SETTINGS_TAB_STORAGE_KEY } from './Settings'
import { settingsMessages } from './Settings.i18n'
import { setLocale } from '../i18n'
import { CapabilitiesContext, NavPrefsContext, type CapabilityDef, type NavPrefs } from '../capabilities/types'

const mockOmlxDetect = vi.fn()
const mockOmlxStart = vi.fn()
const mockOmlxModels = vi.fn()
const mockOmlxStop = vi.fn()
const mockOmlxWarmup = vi.fn()

function installAgent(extra: Record<string, unknown> = {}) {
  Object.defineProperty(window, 'agent24', {
    value: {
      cliToolStatus: vi.fn().mockResolvedValue({ state: 'outdated', path: '/usr/local/bin/agent24', version: '0.3.0', bundledVersion: '0.6.0' }),
      omlxDetect: mockOmlxDetect,
      omlxStart: mockOmlxStart,
      omlxModels: mockOmlxModels,
      omlxStop: mockOmlxStop,
      omlxWarmup: mockOmlxWarmup,
      ...extra,
    },
    writable: true,
    configurable: true,
  })
}

beforeEach(() => {
  vi.clearAllMocks()
  mockOmlxDetect.mockReset()
  mockOmlxStart.mockReset()
  mockOmlxModels.mockReset()
  mockOmlxStop.mockReset()
  mockOmlxWarmup.mockReset()
  mockOmlxDetect.mockResolvedValue(null)
  mockOmlxStart.mockResolvedValue({ ok: false, url: '', error: 'not found' })
  window.localStorage.removeItem(SETTINGS_TAB_STORAGE_KEY)
  installAgent()
})

async function renderPage() {
  await act(async () => { render(<SettingsPage />) })
}

async function openTab(name: string) {
  await act(async () => { fireEvent.click(screen.getByRole('tab', { name })) })
}

function pinOrder(): string[] {
  return screen.getAllByRole('listitem').map((el) => el.querySelector('.settings-pin-name')?.textContent ?? '')
}

function cardValue(label: string): string {
  const summary = screen.getByRole('group', { name: 'Activity summary' })
  const dt = within(summary).getByText(label)
  return dt.nextElementSibling?.textContent ?? ''
}

function localNoon(dayOffset: number): string {
  const d = new Date()
  d.setDate(d.getDate() + dayOffset)
  d.setHours(12, 0, 0, 0)
  return d.toISOString()
}

function applyStore(next: NavPrefs, dropped: string[] = []) {
  return { prefs: next, dropped }
}

function PrefsHarness({
  initial, apply, children,
}: {
  initial: NavPrefs
  apply: (next: NavPrefs) => { prefs: NavPrefs; dropped: string[] }
  children: ReactNode
}) {
  const [prefs, setPrefs] = useState(initial)
  return (
    <NavPrefsContext.Provider value={{
      prefs,
      setPrefs: async (next) => {
        const result = apply(next)
        setPrefs(result.prefs)
        return result.dropped
      },
    }}>
      {children}
    </NavPrefsContext.Provider>
  )
}

async function renderPrefs(
  initial: NavPrefs,
  apply: (next: NavPrefs) => { prefs: NavPrefs; dropped: string[] },
) {
  await act(async () => {
    render(<PrefsHarness initial={initial} apply={apply}><SettingsPage /></PrefsHarness>)
  })
}

const PINS: NavPrefs = { pinned: ['documents', 'design', 'create'], slotCount: 3, showOverview: true }

describe('SettingsPage', () => {
  it('exposes the bundled CLI installer when an older CLI is on PATH', async () => {
    mockOmlxDetect.mockResolvedValue({ url: 'http://localhost:8088', apiKey: '', models: [] })
    await renderPage()
    await openTab('Command line tool')
    expect(await screen.findByText('Command line tool is outdated (0.3.0) — Update')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Install / Update `agent24` command' })).toBeEnabled()
  })

  it('renders settings title', async () => {
    await renderPage()
    expect(screen.getByText('Settings')).toBeInTheDocument()
  })

  it('shows Connected when oMLX detected', async () => {
    mockOmlxDetect.mockResolvedValue({
      url: 'http://127.0.0.1:8088',
      apiKey: 'test',
      models: ['Qwen3-8B-4bit', 'Qwen3-30B'],
    })
    await renderPage()
    await openTab('Models')
    await waitFor(() => {
      expect(screen.getByText('Connected')).toBeInTheDocument()
    })
    expect(screen.getByText(/2 model/)).toBeInTheDocument()
  })

  it('shows Disconnected when oMLX not reachable', async () => {
    mockOmlxDetect.mockResolvedValue(null)
    mockOmlxStart.mockResolvedValue({ ok: false, url: '', error: 'Failed' })
    await renderPage()
    await openTab('Models')
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

    await renderPage()
    await openTab('Models')
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

    await renderPage()
    await openTab('Models')
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

    await renderPage()
    await openTab('Models')
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

    await renderPage()
    await openTab('Models')
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

    await renderPage()
    await openTab('Models')
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
    await renderPage()
    await openTab('Models')
    const eyeBtn = screen.getByTitle(/Show|Hide/)
    fireEvent.click(eyeBtn)
    expect(screen.getByTitle('Hide')).toBeInTheDocument()
  })

  it('keeps a typed API key when leaving and returning to Models', async () => {
    await renderPage()
    await openTab('Models')
    fireEvent.change(await screen.findByLabelText('API Key'), { target: { value: 'secret-key' } })
    await openTab('General')
    // Label queries still see a mounted field. `hidden` is what takes it out of the page.
    expect(screen.getByLabelText('API Key').closest('[hidden]')).not.toBeNull()
    await openTab('Models')
    expect(screen.getByLabelText('API Key').closest('[hidden]')).toBeNull()
    expect(screen.getByLabelText('API Key')).toHaveValue('secret-key')
    expect(mockOmlxDetect).toHaveBeenCalledTimes(1)
  })
})

describe('SettingsPage English default', () => {
  it('shows the English service section', async () => {
    await renderPage()
    await openTab('Models')
    expect(screen.getByText('AI Model Service')).toBeInTheDocument()
  })

  it('relabels a detection error when the language changes (not frozen in the old language)', async () => {
    await renderPage()
    await openTab('Models')
    await waitFor(() => expect(screen.getByText(/Could not start or connect/)).toBeInTheDocument())
    await act(async () => { setLocale('zh') })
    expect(screen.getByText(/无法启动或连接 AI 模型服务/)).toBeInTheDocument()
  })
})

describe('SettingsPage: oMLX needs an API key (401) vs really down', () => {
  beforeEach(() => { setLocale('en') })

  it('detect says authRequired: explains the key is needed, points at the key field, does not auto-start', async () => {
    mockOmlxDetect.mockResolvedValue({ authRequired: true, url: 'http://127.0.0.1:8088' })
    await renderPage()
    await openTab('Models')
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
    await renderPage()
    await openTab('Models')
    expect(await screen.findByText(/Could not start or connect/)).toBeInTheDocument()
    expect(screen.queryByText(/requires an API key/)).toBeNull()
    expect(screen.getByLabelText('API Key')).toHaveAttribute('aria-invalid', 'false')
  })

  it('Test button: 401 -> key-needed message; other failure -> plain failure text', async () => {
    mockOmlxDetect.mockResolvedValue(null)
    mockOmlxStart.mockResolvedValue({ ok: false, url: '', error: '' })
    await renderPage()
    await openTab('Models')
    await screen.findByText(/Could not start or connect/)

    mockOmlxModels.mockResolvedValueOnce({ ok: false, models: [], authRequired: true, error: 'authentication required' })
    await act(async () => { fireEvent.click(screen.getByRole('button', { name: /^Test$/ })) })
    expect(await screen.findByText(/requires an API key/)).toBeInTheDocument()

    mockOmlxModels.mockResolvedValueOnce({ ok: false, models: [], error: 'no response or no models' })
    await act(async () => { fireEvent.click(screen.getByRole('button', { name: /^Test$/ })) })
    expect(await screen.findByText(/no response or no models/)).toBeInTheDocument()
  })
})

describe('Settings tabs', () => {
  it('lists the seven sections and moves with the arrow keys', async () => {
    await renderPage()
    const tablist = screen.getByRole('tablist', { name: 'Settings sections' })
    for (const name of ['General', 'Models', 'Voice', 'Activity', 'Command line tool', 'Connections', 'Advanced']) {
      expect(within(tablist).getByRole('tab', { name })).toBeInTheDocument()
    }
    expect(screen.getAllByRole('tab', { selected: true })).toHaveLength(1)
    expect(screen.getByRole('tab', { name: 'Voice' })).toHaveAttribute('tabindex', '-1')

    const general = screen.getByRole('tab', { name: 'General' })
    general.focus()
    await act(async () => { fireEvent.keyDown(general, { key: 'ArrowRight' }) })
    const models = screen.getByRole('tab', { name: 'Models' })
    expect(models).toHaveAttribute('aria-selected', 'true')
    expect(models).toHaveFocus()
    expect(screen.getByText('AI Model Service')).toBeInTheDocument()

    await act(async () => { fireEvent.keyDown(models, { key: 'ArrowLeft' }) })
    expect(general).toHaveAttribute('aria-selected', 'true')
    expect(general).toHaveFocus()

    await act(async () => { fireEvent.keyDown(general, { key: 'ArrowLeft' }) })
    const advanced = screen.getByRole('tab', { name: 'Advanced' })
    expect(advanced).toHaveAttribute('aria-selected', 'true')
    expect(advanced).toHaveFocus()
    expect(screen.getByText('No advanced settings in this build.')).toBeInTheDocument()
    await act(async () => { fireEvent.keyDown(advanced, { key: 'ArrowRight' }) })
    expect(general).toHaveAttribute('aria-selected', 'true')
    expect(screen.getAllByRole('tab', { selected: true })).toHaveLength(1)
  })

  it('remembers the last tab and ignores a stored value that is not a tab', async () => {
    await renderPage()
    await openTab('Voice')
    expect(window.localStorage.getItem(SETTINGS_TAB_STORAGE_KEY)).toBe('voice')
    cleanup()
    await renderPage()
    expect(screen.getByRole('tab', { name: 'Voice' })).toHaveAttribute('aria-selected', 'true')
    expect(screen.getByRole('button', { name: 'Open Voice' })).toBeInTheDocument()

    cleanup()
    window.localStorage.setItem(SETTINGS_TAB_STORAGE_KEY, 'not-a-tab')
    await renderPage()
    expect(screen.getByRole('tab', { name: 'General' })).toHaveAttribute('aria-selected', 'true')
  })

  it('still switches tabs when storage throws', async () => {
    const original = window.localStorage
    const throwing = {
      getItem: () => { throw new Error('blocked') },
      setItem: () => { throw new Error('blocked') },
      removeItem: () => { throw new Error('blocked') },
      clear: () => { throw new Error('blocked') },
      key: () => null,
      length: 0,
    }
    Object.defineProperty(window, 'localStorage', { configurable: true, value: throwing })
    try {
      await renderPage()
      expect(screen.getByRole('tab', { name: 'General' })).toHaveAttribute('aria-selected', 'true')
      await openTab('Models')
      expect(screen.getByRole('tab', { name: 'Models' })).toHaveAttribute('aria-selected', 'true')
      expect(screen.getByText('AI Model Service')).toBeInTheDocument()
    } finally {
      Object.defineProperty(window, 'localStorage', { configurable: true, value: original })
    }
  })
})

describe('Settings sidebar prefs', () => {
  it('shows the default pins from NavPrefsContext', async () => {
    await renderPage()
    expect(screen.getByTestId('slot-count')).toHaveTextContent('3')
    expect(pinOrder()).toEqual(['documents', 'design', 'create'])
    expect(screen.getByRole('switch', { name: 'Show Overview in sidebar' })).toHaveAttribute('aria-checked', 'true')
    expect(screen.getByText(/pin \(1.6\)/)).toBeInTheDocument()
  })

  it('steps the slot count and stops at 1 and 6', async () => {
    const apply = vi.fn((next: NavPrefs) => applyStore(next))
    await renderPrefs({ pinned: ['documents'], slotCount: 1, showOverview: true }, apply)
    const decrease = screen.getByRole('button', { name: 'Fewer pinned slots' })
    expect(decrease).toBeDisabled()
    fireEvent.click(decrease)
    expect(apply).not.toHaveBeenCalled()

    cleanup()
    apply.mockClear()
    await renderPrefs({ pinned: ['documents'], slotCount: 6, showOverview: true }, apply)
    const increase = screen.getByRole('button', { name: 'More pinned slots' })
    expect(increase).toBeDisabled()
    fireEvent.click(increase)
    expect(apply).not.toHaveBeenCalled()

    cleanup()
    await renderPrefs(PINS, apply)
    await act(async () => { fireEvent.click(screen.getByRole('button', { name: 'More pinned slots' })) })
    await waitFor(() => expect(screen.getByTestId('slot-count')).toHaveTextContent('4'))
    expect(apply).toHaveBeenCalledWith(expect.objectContaining({ slotCount: 4, pinned: PINS.pinned, showOverview: true }))
  })

  it('shows the ids setPrefs returns when a slot shrink unpins them', async () => {
    const apply = vi.fn((next: NavPrefs) => ({
      prefs: { ...next, pinned: next.pinned.slice(0, next.slotCount) },
      dropped: next.slotCount < PINS.slotCount ? ['schedules', 'memory'] : [],
    }))
    await renderPrefs(PINS, apply)
    await act(async () => { fireEvent.click(screen.getByRole('button', { name: 'Fewer pinned slots' })) })
    expect(await screen.findByText('Unpinned schedules, memory. They are still in Capabilities.')).toBeInTheDocument()
    expect(pinOrder()).toEqual(['documents', 'design'])

    await act(async () => { setLocale('zh') })
    expect(screen.getByText('已取消钉选 schedules, memory。它们仍在能力中心。')).toBeInTheDocument()
    await act(async () => { setLocale('th') })
    expect(screen.getByText('เลิกปักหมุด schedules, memory แล้ว ยังอยู่ใน Capabilities')).toBeInTheDocument()
    await act(async () => { setLocale('en') })

    await act(async () => { fireEvent.click(screen.getByRole('button', { name: 'More pinned slots' })) })
    await waitFor(() => expect(screen.queryByText(/Unpinned/)).not.toBeInTheDocument())
  })

  it('reorders pins with buttons that can take keyboard focus', async () => {
    const apply = vi.fn((next: NavPrefs) => applyStore(next))
    await renderPrefs(PINS, apply)
    expect(screen.getByRole('button', { name: 'Move documents up' })).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Move create down' })).toBeDisabled()
    const up = screen.getByRole('button', { name: 'Move design up' })
    up.focus()
    expect(up).toHaveFocus()
    expect(up.tagName).toBe('BUTTON')
    await act(async () => { fireEvent.click(up) })
    await waitFor(() => expect(pinOrder()).toEqual(['design', 'documents', 'create']))
    expect(apply).toHaveBeenCalledWith(expect.objectContaining({
      pinned: ['design', 'documents', 'create'], slotCount: 3, showOverview: true,
    }))
    await act(async () => { fireEvent.click(screen.getByRole('button', { name: 'Move design down' })) })
    await waitFor(() => expect(pinOrder()).toEqual(['documents', 'design', 'create']))
  })

  it('toggles Show Overview in sidebar', async () => {
    const apply = vi.fn((next: NavPrefs) => applyStore(next))
    await renderPrefs(PINS, apply)
    const sw = screen.getByRole('switch', { name: 'Show Overview in sidebar' })
    expect(sw).toHaveAttribute('aria-checked', 'true')
    await act(async () => { fireEvent.click(sw) })
    await waitFor(() => expect(sw).toHaveAttribute('aria-checked', 'false'))
    expect(apply).toHaveBeenCalledWith(expect.objectContaining({ showOverview: false, slotCount: 3 }))
  })

  it('says when nothing is pinned', async () => {
    await renderPrefs({ pinned: [], slotCount: 3, showOverview: true }, (next) => applyStore(next))
    expect(screen.getByText('Nothing is pinned. Pin capabilities from Capabilities.')).toBeInTheDocument()
    expect(screen.queryByRole('listitem')).not.toBeInTheDocument()
  })
})

describe('Settings activity', () => {
  const today = localNoon(0)
  const yesterday = localNoon(-1)

  function runs() {
    return [
      { id: 'run-a', session_id: null, status: 'running', input: { prompt: 'do the thing' }, usage: { total_tokens: 10 }, created_at: today },
      { id: 'run-b', session_id: null, status: 'running', input: { prompt: 'older run' }, usage: { total_tokens: 5 }, created_at: yesterday },
      { id: 'run-c', session_id: null, status: 'awaiting_approval', input: { prompt: 'need approval' }, usage: { total_tokens: 1 }, created_at: today },
      { id: 'run-d', session_id: null, status: 'failed', input: { prompt: 'failed now' }, usage: { total_tokens: 7 }, created_at: yesterday, ended_at: today },
      { id: 'run-e', session_id: null, status: 'failed', input: { prompt: 'failed then' }, usage: { total_tokens: 3 }, created_at: yesterday, ended_at: yesterday },
      { id: 'run-f', session_id: null, status: 'completed', input: { prompt: 'done today' }, usage: { total_tokens: 100 }, created_at: today },
      { id: 'run-g', session_id: null, status: 'queued', input: { prompt: 'queued today' }, usage: { total_tokens: 4 }, created_at: today },
    ]
  }

  it('embeds Runs and fills the four cards from that run list', async () => {
    installAgent({
      backendProxy: vi.fn().mockResolvedValue({ ok: true, status: 200, data: { runs: runs() } }),
    })
    await renderPage()
    await openTab('Activity')
    expect(await screen.findByText('Runs')).toBeInTheDocument()
    expect(await screen.findByText('do the thing')).toBeInTheDocument()
    expect(screen.getByText('Select a run to view details')).toBeInTheDocument()
    await waitFor(() => expect(cardValue('Running')).toBe('2'))
    expect(cardValue('Waiting for approval')).toBe('1')
    expect(cardValue('Failed today')).toBe('1')
    expect(cardValue('Tokens today')).toBe('115')
  })

  it('shows zeros when the run list is empty', async () => {
    installAgent({
      backendProxy: vi.fn().mockResolvedValue({ ok: true, status: 200, data: { runs: [] } }),
    })
    await renderPage()
    await openTab('Activity')
    expect(await screen.findByText('No runs yet')).toBeInTheDocument()
    await waitFor(() => expect(cardValue('Running')).toBe('0'))
    expect(cardValue('Waiting for approval')).toBe('0')
    expect(cardValue('Failed today')).toBe('0')
    expect(cardValue('Tokens today')).toBe('0')
  })

  it('shows an em dash on every card when the run list cannot be loaded', async () => {
    installAgent({
      backendProxy: vi.fn().mockResolvedValue({ ok: false, status: 503, data: { error: { message: 'backend down' } } }),
    })
    await renderPage()
    await openTab('Activity')
    expect(await screen.findByText('backend down')).toBeInTheDocument()
    await waitFor(() => expect(cardValue('Running')).toBe('\u2014'))
    expect(cardValue('Waiting for approval')).toBe('\u2014')
    expect(cardValue('Failed today')).toBe('\u2014')
    expect(cardValue('Tokens today')).toBe('\u2014')
  })
})

describe('Settings voice link and previous blocks', () => {
  it('opens the voice capability id, or the voice page when that is how it is registered', async () => {
    const open = vi.fn()
    const voice: CapabilityDef = { id: 'agent-ear', group: 'office', nameKey: 'n', descKey: 'd', builtin: false, page: 'voice' }
    await act(async () => {
      render(
        <CapabilitiesContext.Provider value={{ defs: [voice], open, approvalsWaiting: 0 }}>
          <SettingsPage />
        </CapabilitiesContext.Provider>,
      )
    })
    await openTab('Voice')
    fireEvent.click(screen.getByRole('button', { name: 'Open Voice' }))
    expect(open).toHaveBeenCalledWith('agent-ear', 'other')
  })

  it('falls back to the voice id when the catalog has no voice capability yet', async () => {
    const open = vi.fn()
    await act(async () => {
      render(
        <CapabilitiesContext.Provider value={{ defs: [], open, approvalsWaiting: 0 }}>
          <SettingsPage />
        </CapabilitiesContext.Provider>,
      )
    })
    await openTab('Voice')
    fireEvent.click(screen.getByRole('button', { name: 'Open Voice' }))
    expect(open).toHaveBeenCalledWith('voice', 'other')
  })

  it('keeps every previous settings block on its tab', async () => {
    await renderPage()
    expect(screen.getByText('Settings')).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Appearance' })).toBeInTheDocument()
    expect(screen.getByLabelText('Language')).toBeInTheDocument()
    expect(screen.getByText('Applies to the whole app immediately')).toBeInTheDocument()
    expect(screen.queryByLabelText('API Key')).not.toBeInTheDocument()
    expect(screen.queryByText(/Command line tool is outdated/)).not.toBeInTheDocument()

    await openTab('Models')
    expect(screen.getByRole('heading', { name: 'AI Model Service' })).toBeInTheDocument()
    expect(screen.getByText('Service URL')).toBeInTheDocument()
    expect(screen.getByText('OpenAI-compatible API endpoint')).toBeInTheDocument()
    expect(screen.getByLabelText('API Key')).toBeInTheDocument()
    expect(screen.getByText('Leave empty if no auth required')).toBeInTheDocument()
    await screen.findByText('Disconnected')
    expect(screen.getByText('Start Port')).toBeInTheDocument()
    expect(screen.getByText(/Port when clicking/)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Auto-detect' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Start oMLX' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /^Test$/ })).toBeInTheDocument()
    expect(screen.getByText('Connect to list and configure models')).toBeInTheDocument()

    await openTab('Command line tool')
    expect(screen.getByRole('heading', { name: 'Command line tool' })).toBeInTheDocument()
    expect(screen.getByText('Command on PATH')).toBeInTheDocument()
    expect(await screen.findByRole('button', { name: 'Install / Update `agent24` command' })).toBeEnabled()
    expect(screen.getByText('Command line tool is outdated (0.3.0) — Update')).toBeInTheDocument()

    await openTab('Voice')
    expect(screen.getByText('There are no Voice or AgentEar settings here. Open the Voice capability to use them.')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Open Voice' })).toBeInTheDocument()

    await openTab('Connections')
    expect(screen.getByRole('heading', { name: 'Connections' })).toBeInTheDocument()
    expect(screen.getByText('No connection settings in this build.')).toBeInTheDocument()

    await openTab('Advanced')
    expect(screen.getByRole('heading', { name: 'Advanced' })).toBeInTheDocument()
    expect(screen.getByText('No advanced settings in this build.')).toBeInTheDocument()
  })
})

describe('Settings copy', () => {
  it('has a Chinese and a Thai string for every English key', () => {
    const keys = Object.keys(settingsMessages.en) as (keyof typeof settingsMessages.en)[]
    expect(Object.keys(settingsMessages.zh).sort()).toEqual([...keys].sort())
    expect(Object.keys(settingsMessages.th).sort()).toEqual([...keys].sort())
    for (const key of keys) {
      expect(settingsMessages.en[key].trim().length).toBeGreaterThan(0)
      expect(settingsMessages.zh[key].trim().length).toBeGreaterThan(0)
      expect(settingsMessages.th[key].trim().length).toBeGreaterThan(0)
    }
  })

  it('relabels the tabs and the sidebar when the language changes', async () => {
    await renderPage()
    expect(screen.getByRole('tab', { name: 'General' })).toBeInTheDocument()
    await act(async () => { setLocale('zh') })
    expect(screen.getByRole('tab', { name: '通用' })).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: '侧边栏' })).toBeInTheDocument()
    expect(screen.getByRole('switch', { name: '在侧边栏显示 Overview' })).toBeInTheDocument()
    await act(async () => { setLocale('th') })
    expect(screen.getByRole('tab', { name: 'ทั่วไป' })).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'แถบด้านข้าง' })).toBeInTheDocument()
    await act(async () => { setLocale('en') })
  })
})
