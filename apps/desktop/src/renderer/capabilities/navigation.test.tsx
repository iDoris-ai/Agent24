// @vitest-environment jsdom
import { useContext } from 'react'
import { describe, expect, it, vi } from 'vitest'
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { Sidebar } from './Sidebar'
import { Breadcrumb } from './Breadcrumb'
import { CapabilitiesProvider, NavPrefsProvider, useCapabilityRoute } from './providers'
import { CapabilitiesContext, NavPrefsContext, NAV_PREFS_DEFAULT, type NavPrefs, type NavPrefsApi } from './types'
import { CAPABILITIES } from './registry'
import CapabilitiesPage from '../pages/Capabilities'
import { SidebarPrefs } from '../pages/settings/SidebarPrefs'

describe('Sidebar', () => {
  it('renders fixed entries, ordered pins, empty slots, hidden Overview and approval badges', () => {
    const open = vi.fn()
    const navigate = vi.fn()
    const { rerender } = render(
      <CapabilitiesContext.Provider value={{ defs: CAPABILITIES, open, approvalsWaiting: 4 }}>
        <NavPrefsContext.Provider value={{ prefs: { pinned: ['voice', 'documents'], slotCount: 3, showOverview: true }, setPrefs: vi.fn() }}>
          <Sidebar activeId="voice" navigate={navigate} />
        </NavPrefsContext.Provider>
      </CapabilitiesContext.Provider>,
    )
    expect(screen.getAllByRole('button').map(b => b.textContent)).toEqual([
      'Overview4', 'Assistant', 'Voice', 'Documents', 'Capabilities4', 'Settings',
    ])
    expect(screen.getByText('Pinned 2/3')).toBeInTheDocument()
    expect(screen.getByText('Empty slot')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Voice' })).toHaveAttribute('aria-current', 'page')
    fireEvent.click(screen.getByRole('button', { name: 'Voice' }))
    expect(open).toHaveBeenCalledWith('voice', 'sidebar')
    fireEvent.click(screen.getByRole('button', { name: /Capabilities/ }))
    expect(navigate).toHaveBeenCalledWith('capabilities')
    rerender(<NavPrefsContext.Provider value={{ prefs: { ...NAV_PREFS_DEFAULT, showOverview: false }, setPrefs: vi.fn() }}>
      <Sidebar activeId="assistant" navigate={navigate} />
    </NavPrefsContext.Provider>)
    expect(screen.queryByRole('button', { name: /Overview/ })).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Assistant' })).toBeInTheDocument()
    expect(screen.queryByText('0')).not.toBeInTheDocument()
  })
})

function RouteView() {
  const { route, navigate } = useCapabilityRoute()
  const { open, defs } = useContext(CapabilitiesContext)
  const selected = route.kind === 'capability' ? defs.find(d => d.id === route.id) : undefined
  return <>
    <Sidebar activeId={route.kind === 'capability' ? route.id : route.page} navigate={navigate} />
    {selected && route.kind === 'capability' && <Breadcrumb capability={selected} origin={route.origin} onBack={() => navigate('capabilities')} />}
    {route.kind === 'page' && route.page === 'capabilities' && <CapabilitiesPage />}
    <button onClick={() => open('documents', 'switcher')}>Switch</button>
  </>
}

describe('capability routing', () => {
  it('routes switcher actions to Settings with the requested initialTab and ignores unknown targets', () => {
    function Actions() {
      const { open } = useContext(CapabilitiesContext)
      const { route } = useCapabilityRoute()
      return <>
        {['settings/general', 'settings/activity', 'settings/unknown', 'capabilities'].map(id =>
          <button key={id} onClick={() => open(id, 'switcher')}>{id}</button>)}
        <output aria-label="Route">{JSON.stringify(route)}</output>
      </>
    }
    window.agent24 = { backendProxy: vi.fn().mockResolvedValue({ ok: false }) } as unknown as Window['agent24']
    render(<CapabilitiesProvider><Actions /></CapabilitiesProvider>)
    for (const [key, initialTab] of ['general', 'activity'].entries()) {
      fireEvent.click(screen.getByRole('button', { name: `settings/${initialTab}` }))
      expect(screen.getByLabelText('Route')).toHaveTextContent(JSON.stringify({ kind: 'page', page: 'settings', initialTab, key: key + 1 }))
    }
    fireEvent.click(screen.getByRole('button', { name: 'settings/unknown' }))
    expect(screen.getByLabelText('Route')).toHaveTextContent('"initialTab":"activity"')
    fireEvent.click(screen.getByRole('button', { name: 'capabilities' }))
    expect(screen.getByLabelText('Route')).toHaveTextContent(JSON.stringify({ kind: 'page', page: 'capabilities' }))
  })

  it('polls manifests through backendProxy, lists dynamic UI in domainOs and never auto-pins it', async () => {
    const backendProxy = vi.fn().mockImplementation(({ path }: { path: string }) => Promise.resolve({ ok: true, data:
      path === '/api/modules' ? [{ id: 'hello', type: 'ui', enabled: true, name: 'Hello' }] : { approvals: [{}, {}] },
    }))
    window.agent24 = { backendProxy } as unknown as Window['agent24']
    render(<CapabilitiesProvider><NavPrefsProvider><RouteView /></NavPrefsProvider></CapabilitiesProvider>)
    await waitFor(() => expect(screen.getByRole('button', { name: /Capabilities/ })).toHaveTextContent('2'))
    expect(screen.queryByRole('button', { name: 'Hello' })).not.toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: /Capabilities/ }))
    await waitFor(() => expect(within(screen.getByRole('region', { name: 'Domain OS' })).getByRole('button', { name: 'Hello' })).toBeInTheDocument())
    expect(within(screen.getByRole('navigation', { name: 'Main navigation' })).queryByText('Hello')).not.toBeInTheDocument()
    expect(backendProxy).toHaveBeenCalledWith({ method: 'GET', path: '/api/modules' })
  })

  it('refreshes every five seconds, clears settled approval badges and tolerates unavailable manifests', async () => {
    vi.useFakeTimers()
    let pending = true
    const backendProxy = vi.fn().mockImplementation(({ path }: { path: string }) => Promise.resolve(
      path === '/api/modules' ? { ok: false, status: 404 } : { ok: true, data: { approvals: pending ? [{ id: 'a' }] : [] } },
    ))
    window.agent24 = { backendProxy } as unknown as Window['agent24']
    const view = render(<CapabilitiesProvider><NavPrefsProvider><RouteView /></NavPrefsProvider></CapabilitiesProvider>)
    try {
      await act(async () => {})
      expect(screen.getByRole('button', { name: /Capabilities/ })).toHaveTextContent('1')
      pending = false
      await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
      expect(screen.getByRole('button', { name: 'Capabilities' })).toBeInTheDocument()
      expect(backendProxy.mock.calls.filter(([req]) => req.path === '/api/modules')).toHaveLength(2)
      fireEvent.click(screen.getByRole('button', { name: 'Capabilities' }))
      expect(within(screen.getByRole('main')).getByRole('button', { name: 'Documents' })).toBeInTheDocument()
    } finally {
      view.unmount()
      vi.useRealTimers()
    }
  })

  it('returns to the center when the currently open dynamic module is removed', async () => {
    vi.useFakeTimers()
    let installed = true
    window.agent24 = { backendProxy: vi.fn().mockImplementation(({ path }: { path: string }) => Promise.resolve({ ok: true, data:
      path === '/api/modules' ? installed ? [{ id: 'hello', type: 'ui', enabled: true, name: 'Hello' }] : [] : { approvals: [] },
    })) } as unknown as Window['agent24']
    const view = render(<CapabilitiesProvider><NavPrefsProvider><RouteView /></NavPrefsProvider></CapabilitiesProvider>)
    try {
      await act(async () => {})
      fireEvent.click(screen.getByRole('button', { name: 'Capabilities' }))
      fireEvent.click(screen.getByRole('button', { name: 'Hello' }))
      expect(screen.getByRole('navigation', { name: 'Breadcrumb' })).toHaveTextContent('Hello')
      installed = false
      await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
      expect(screen.getByRole('main')).toBeInTheDocument()
      expect(screen.queryByRole('navigation', { name: 'Breadcrumb' })).not.toBeInTheDocument()
    } finally {
      view.unmount()
      vi.useRealTimers()
    }
  })
})

function PrefsEditor() {
  const { prefs, setPrefs } = useContext(NavPrefsContext)
  return <><output aria-label="Preferences">{JSON.stringify(prefs)}</output><button onClick={() => void setPrefs({ pinned: ['assistant', 'voice', 'voice', 'memory', 'unknown'], slotCount: 1, showOverview: false }).then(ids => {
    document.title = ids.join(',')
  })}>Save</button></>
}

describe('NavPrefsProvider', () => {
  function renderEditor(children = <CapabilitiesPage />) {
    return render(<CapabilitiesProvider><NavPrefsProvider>{children}<PrefsEditor /></NavPrefsProvider></CapabilitiesProvider>)
  }

  function installPrefs(getNavPrefs: () => Promise<NavPrefs>, setNavPrefs = vi.fn().mockResolvedValue(undefined), backendProxy = vi.fn().mockResolvedValue({ ok: false })) {
    Object.defineProperty(window, 'agent24', { configurable: true, writable: true, value: {
      getNavPrefs, setNavPrefs, backendProxy,
    } })
    return setNavPrefs
  }

  it.each(['capabilities', 'settings'])('preserves offline dynamic pins when %s toggles Overview', async page => {
    const setNavPrefs = installPrefs(async () => ({ pinned: ['documents', 'acme'], slotCount: 3, showOverview: true }))
    renderEditor(page === 'settings' ? <SidebarPrefs /> : <CapabilitiesPage />)
    await waitFor(() => expect(screen.getByLabelText('Preferences')).toHaveTextContent('"pinned":["documents"]'))
    fireEvent.click(screen.getByRole(page === 'settings' ? 'switch' : 'checkbox', { name: 'Show Overview in sidebar' }))
    await waitFor(() => expect(setNavPrefs).toHaveBeenCalledWith({ pinned: ['documents', 'acme'], slotCount: 3, showOverview: false }))
  })

  it('keeps hidden pins in order and counts them against slots when adding a pin', async () => {
    const setNavPrefs = installPrefs(async () => ({ pinned: ['acme', 'documents'], slotCount: 3, showOverview: true }))
    renderEditor()
    await waitFor(() => expect(screen.getByLabelText('Preferences')).toHaveTextContent('"pinned":["documents"]'))
    fireEvent.click(screen.getByRole('button', { name: 'Pin Design' }))
    await waitFor(() => expect(setNavPrefs).toHaveBeenCalledWith({ pinned: ['acme', 'documents', 'design'], slotCount: 3, showOverview: true }))
    fireEvent.click(screen.getByRole('button', { name: /(?:Pin|Slots full) Create/ }))
    await act(async () => {})
    expect(setNavPrefs).toHaveBeenCalledTimes(1)
    fireEvent.click(screen.getByRole('button', { name: 'Pinned Documents' }))
    await waitFor(() => expect(setNavPrefs).toHaveBeenLastCalledWith({ pinned: ['acme', 'design'], slotCount: 3, showOverview: true }))
  })

  it('merges hidden pins into value snapshots and lets an updater explicitly unpin them', async () => {
    const setNavPrefs = installPrefs(async () => ({ pinned: ['acme', 'documents'], slotCount: 3, showOverview: true }))
    function Editor() {
      const { prefs, setPrefs } = useContext(NavPrefsContext)
      return <>
        <button onClick={() => void setPrefs({ ...prefs, showOverview: false })}>Snapshot</button>
        <button onClick={() => void setPrefs(prev => ({ ...prev, pinned: prev.pinned.filter(id => id !== 'acme') }))}>Unpin acme</button>
      </>
    }
    renderEditor(<Editor />)
    await waitFor(() => expect(screen.getByLabelText('Preferences')).toHaveTextContent('"pinned":["documents"]'))
    fireEvent.click(screen.getByRole('button', { name: 'Snapshot' }))
    await waitFor(() => expect(setNavPrefs).toHaveBeenLastCalledWith({ pinned: ['acme', 'documents'], slotCount: 3, showOverview: false }))
    fireEvent.click(screen.getByRole('button', { name: 'Unpin acme' }))
    await waitFor(() => expect(setNavPrefs).toHaveBeenLastCalledWith({ pinned: ['documents'], slotCount: 3, showOverview: false }))
  })

  it('retains hidden pins when reducing slots and reports the visible pins removed', async () => {
    const setNavPrefs = installPrefs(async () => ({ pinned: ['documents', 'acme', 'design'], slotCount: 3, showOverview: true }))
    renderEditor(<SidebarPrefs />)
    await waitFor(() => expect(screen.getByLabelText('Preferences')).toHaveTextContent('"pinned":["documents","design"]'))
    fireEvent.click(screen.getByRole('button', { name: 'Fewer pinned slots' }))
    await waitFor(() => expect(setNavPrefs).toHaveBeenLastCalledWith({ pinned: ['documents', 'acme'], slotCount: 2, showOverview: true }))
    expect(screen.getByText('Unpinned Design. They are still in Capabilities.')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Fewer pinned slots' }))
    await waitFor(() => expect(setNavPrefs).toHaveBeenLastCalledWith({ pinned: ['acme'], slotCount: 1, showOverview: true }))
    expect(screen.getByText('Unpinned Documents. They are still in Capabilities.')).toBeInTheDocument()
  })

  it('shows the saved dynamic pin again when its manifest recovers after an offline edit', async () => {
    vi.useFakeTimers()
    let online = false
    const backendProxy = vi.fn().mockImplementation(({ path }: { path: string }) => Promise.resolve({ ok: online,
      data: path === '/api/modules' ? [{ id: 'acme', type: 'ui', name: 'Acme' }] : { approvals: [] },
    }))
    const setNavPrefs = installPrefs(async () => ({ pinned: ['documents', 'acme'], slotCount: 3, showOverview: true }), vi.fn().mockResolvedValue(undefined), backendProxy)
    const view = renderEditor()
    try {
      await act(async () => {})
      fireEvent.click(screen.getByRole('checkbox', { name: 'Show Overview in sidebar' }))
      await act(async () => {})
      expect(setNavPrefs).toHaveBeenCalledWith({ pinned: ['documents', 'acme'], slotCount: 3, showOverview: false })
      online = true
      await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
      expect(screen.getByLabelText('Preferences')).toHaveTextContent('"pinned":["documents","acme"]')
      expect(screen.getByRole('button', { name: 'Pinned Acme' })).toBeInTheDocument()
    } finally {
      view.unmount()
      vi.useRealTimers()
    }
  })

  it('retains both rapid Pin Design and Pin Create edits while the first write is pending', async () => {
    let finishWrite = () => {}
    const write = new Promise<void>(resolve => { finishWrite = resolve })
    const setNavPrefs = installPrefs(async () => ({ pinned: ['documents'], slotCount: 3, showOverview: true }),
      vi.fn().mockReturnValueOnce(write).mockResolvedValue(undefined))
    renderEditor()
    await waitFor(() => expect(screen.getByLabelText('Preferences')).toHaveTextContent('"pinned":["documents"]'))
    fireEvent.click(screen.getByRole('button', { name: 'Pin Design' }))
    fireEvent.click(screen.getByRole('button', { name: 'Pin Create' }))
    await waitFor(() => expect(setNavPrefs).toHaveBeenCalledTimes(1))
    await act(async () => { finishWrite() })
    expect(setNavPrefs.mock.calls.map(([prefs]) => prefs.pinned)).toEqual([
      ['documents', 'design'], ['documents', 'design', 'create'],
    ])
    expect(screen.getByLabelText('Preferences')).toHaveTextContent('"pinned":["documents","design","create"]')
  })

  it('queues early edits until the initial read and applies them to saved preferences', async () => {
    let finishRead: (prefs: NavPrefs) => void = () => {}
    const read = new Promise<NavPrefs>(resolve => { finishRead = resolve })
    const setNavPrefs = installPrefs(() => read)
    renderEditor()
    fireEvent.click(screen.getByRole('checkbox', { name: 'Show Overview in sidebar' }))
    await act(async () => {})
    expect(setNavPrefs).not.toHaveBeenCalled()
    await act(async () => { finishRead({ pinned: ['memory'], slotCount: 5, showOverview: true }) })
    expect(setNavPrefs).toHaveBeenCalledWith({ pinned: ['memory'], slotCount: 5, showOverview: false })
  })

  it('uses defaults and permits queued and subsequent edits after an initial read failure', async () => {
    let failRead: (error: Error) => void = () => {}
    const read = new Promise<NavPrefs>((_resolve, reject) => { failRead = reject })
    const setNavPrefs = installPrefs(() => read)
    renderEditor()
    fireEvent.click(screen.getByRole('checkbox', { name: 'Show Overview in sidebar' }))
    await act(async () => {})
    expect(setNavPrefs).not.toHaveBeenCalled()
    await act(async () => { failRead(new Error('IPC unavailable')) })
    expect(setNavPrefs).toHaveBeenCalledWith({ ...NAV_PREFS_DEFAULT, showOverview: false })
    fireEvent.click(screen.getByRole('button', { name: 'Pinned Design' }))
    await waitFor(() => expect(setNavPrefs).toHaveBeenLastCalledWith({ pinned: ['documents', 'create'], slotCount: 3, showOverview: false }))
  })

  it('loads persisted preferences and clamps before saving, returning the unpinned ids', async () => {
    const setNavPrefs = vi.fn().mockResolvedValue(undefined)
    window.agent24 = { getNavPrefs: vi.fn().mockResolvedValue({ pinned: ['memory'], slotCount: 4, showOverview: true }), setNavPrefs } as unknown as Window['agent24']
    render(<CapabilitiesContext.Provider value={{ defs: CAPABILITIES, open: vi.fn(), approvalsWaiting: 0 }}><NavPrefsProvider><PrefsEditor /></NavPrefsProvider></CapabilitiesContext.Provider>)
    await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('memory'))
    fireEvent.click(screen.getByText('Save'))
    await waitFor(() => expect(document.title).toBe('memory'))
    expect(setNavPrefs).toHaveBeenCalledWith({ pinned: ['voice'], slotCount: 1, showOverview: false })
    expect(screen.getByRole('status')).toHaveTextContent('voice')
  })

  it('preserves stored dynamic pins while waiting for the first manifest response', async () => {
    let resolveModules: (value: unknown) => void = () => {}
    const modules = new Promise(resolve => { resolveModules = resolve })
    window.agent24 = {
      getNavPrefs: vi.fn().mockResolvedValue({ pinned: ['hello', 'voice'], slotCount: 2, showOverview: true }),
      backendProxy: vi.fn().mockImplementation(({ path }: { path: string }) => path === '/api/modules' ? modules : Promise.resolve({ ok: true, data: { approvals: [] } })),
    } as unknown as Window['agent24']
    render(<CapabilitiesProvider><NavPrefsProvider><PrefsEditor /></NavPrefsProvider></CapabilitiesProvider>)
    await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('voice'))
    expect(screen.getByRole('status')).not.toHaveTextContent('hello')
    await act(async () => { resolveModules({ ok: true, data: [{ id: 'hello', name: 'Hello', type: 'ui', enabled: true }] }) })
    expect(screen.getByRole('status')).toHaveTextContent('"pinned":["hello","voice"]')
  })

  it('waits for startup preferences and retains the last successful write if a queued write fails', async () => {
    let resolveRead: (value: NavPrefs) => void = () => {}
    const read = new Promise<NavPrefs>(resolve => { resolveRead = resolve })
    const setNavPrefs = vi.fn().mockResolvedValueOnce(undefined).mockRejectedValueOnce(new Error('disk full'))
    window.agent24 = { getNavPrefs: () => read, setNavPrefs } as unknown as Window['agent24']
    let api: NavPrefsApi | undefined
    function Editor() {
      api = useContext(NavPrefsContext)
      return <output>{JSON.stringify(api.prefs)}</output>
    }
    render(<CapabilitiesContext.Provider value={{ defs: CAPABILITIES, open: vi.fn(), approvalsWaiting: 0 }}><NavPrefsProvider><Editor /></NavPrefsProvider></CapabilitiesContext.Provider>)
    const editor = api
    if (!editor) throw new Error('Missing preferences context')
    await act(async () => {
      const first = editor.setPrefs({ pinned: ['voice'], slotCount: 1, showOverview: false })
      const second = editor.setPrefs({ pinned: ['memory'], slotCount: 1, showOverview: false })
      expect(setNavPrefs).not.toHaveBeenCalled()
      resolveRead(NAV_PREFS_DEFAULT)
      await first
      await expect(second).rejects.toThrow('disk full')
    })
    expect(setNavPrefs.mock.calls.map(([prefs]) => prefs.pinned)).toEqual([['voice'], ['memory']])
    expect(screen.getByRole('status')).toHaveTextContent('"pinned":["voice"]')
  })
})
