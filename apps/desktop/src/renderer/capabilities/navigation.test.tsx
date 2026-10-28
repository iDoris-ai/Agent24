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
  return <><output>{JSON.stringify(prefs)}</output><button onClick={() => void setPrefs({ pinned: ['assistant', 'voice', 'voice', 'memory', 'unknown'], slotCount: 1, showOverview: false }).then(ids => {
    document.title = ids.join(',')
  })}>Save</button></>
}

describe('NavPrefsProvider', () => {
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

  it('ignores a stale startup read and retains the last successful write if a queued write fails', async () => {
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
      await first
      await expect(second).rejects.toThrow('disk full')
      resolveRead(NAV_PREFS_DEFAULT)
    })
    expect(setNavPrefs.mock.calls.map(([prefs]) => prefs.pinned)).toEqual([['voice'], ['memory']])
    expect(screen.getByRole('status')).toHaveTextContent('"pinned":["voice"]')
  })
})
