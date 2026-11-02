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
import { setLocale } from '../i18n'

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

  it('opens the pinned-slots row from the capabilities slots-full link', () => {
    const scroll = vi.spyOn(HTMLElement.prototype, 'scrollIntoView').mockImplementation(() => {})
    function Pages() {
      const { open } = useContext(CapabilitiesContext)
      return <>
        <CapabilitiesPage onOpenSettings={() => open('settings/general', 'capabilities')} />
        <SidebarPrefs />
      </>
    }
    window.agent24 = { backendProxy: vi.fn().mockResolvedValue({ ok: false }) } as unknown as Window['agent24']
    try {
      render(<CapabilitiesProvider><Pages /></CapabilitiesProvider>)
      expect(screen.getByText(/All sidebar slots are in use/)).toBeInTheDocument()
      fireEvent.click(screen.getByRole('link', { name: 'Settings › General' }))
      const row = document.getElementById('settings-pinned-slots')
      expect(row).toHaveClass('is-target')
      expect(row?.contains(screen.getByTestId('slot-count'))).toBe(true)
      expect(scroll).toHaveBeenCalled()
    } finally {
      scroll.mockRestore()
    }
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

  it.each([
    { id: 'acme', manifest: { id: 'acme', type: 'ui', name: 'Acme', enabled: false }, status: 'Disabled' },
    { id: 'acme', manifest: { id: 'acme', type: 'ui' }, status: 'Unavailable' },
    { id: 'settings/acme', manifest: { id: 'settings/acme', type: 'ui', name: 'Acme' }, status: 'Unavailable' },
  ])('F7 retains a $status pin filtered from defs ($id), including after edits', async ({ id, manifest, status }) => {
    vi.useFakeTimers()
    let manifests: unknown[] = [manifest]
    const backendProxy = vi.fn().mockImplementation(() => Promise.resolve({ ok: true, data: manifests }))
    const saved = { pinned: ['documents', id], slotCount: 2, showOverview: true }
    const setNavPrefs = installPrefs(async () => saved, vi.fn().mockResolvedValue(undefined), backendProxy)
    const view = renderEditor(<><SidebarPrefs /><Sidebar activeId="settings" navigate={vi.fn()} /></>)
    try {
      await act(async () => {})
      await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
      expect(setNavPrefs).not.toHaveBeenCalled()
      expect(screen.getByText(`${id} — ${status}`)).toBeInTheDocument()
      expect(screen.getByText('Pinned 2/2')).toBeInTheDocument()
      expect(within(screen.getByRole('navigation')).queryByText(/acme/i)).not.toBeInTheDocument()
      await act(async () => { fireEvent.click(screen.getByRole('switch', { name: 'Show Overview in sidebar' })) })
      expect(setNavPrefs).toHaveBeenLastCalledWith({ ...saved, showOverview: false })
      expect(screen.queryByText(/no longer installed/)).not.toBeInTheDocument()
      if (id === 'acme') {
        manifests = [{ id, type: 'ui', name: 'Acme', enabled: true }]
        await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
        expect(within(screen.getByRole('navigation')).getByRole('button', { name: 'Acme' })).toBeInTheDocument()
        expect(setNavPrefs).toHaveBeenCalledTimes(1)
      }
    } finally {
      view.unmount()
      vi.useRealTimers()
    }
  })

  it.each([{ first: [] }, { first: [{ id: 'other', type: 'ui', name: 'Other' }] }])('F7 preserves acme when the first successful list is $first and the second recovers', async ({ first }) => {
    vi.useFakeTimers()
    let manifests = first
    const backendProxy = vi.fn().mockImplementation(() => Promise.resolve({ ok: true, data: manifests }))
    const setNavPrefs = installPrefs(async () => ({ pinned: ['documents', 'acme'], slotCount: 2, showOverview: true }), vi.fn().mockResolvedValue(undefined), backendProxy)
    const view = renderEditor(<><SidebarPrefs /><Sidebar activeId="settings" navigate={vi.fn()} /></>)
    try {
      await act(async () => {})
      expect(setNavPrefs).not.toHaveBeenCalled()
      expect(screen.getByText('Pinned 2/2')).toBeInTheDocument()
      manifests = [{ id: 'acme', type: 'ui', name: 'Acme' }]
      await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
      expect(within(screen.getByRole('navigation')).getByRole('button', { name: 'Acme' })).toBeInTheDocument()
      expect(setNavPrefs).not.toHaveBeenCalled()
      expect(screen.queryByText(/no longer installed/)).not.toBeInTheDocument()
    } finally {
      view.unmount()
      vi.useRealTimers()
    }
  })

  it('F7 releases and notifies only after two consecutive successful lists omit the pin', async () => {
    vi.useFakeTimers()
    const saved = { pinned: ['documents', 'acme'], slotCount: 2, showOverview: true }
    const setNavPrefs = installPrefs(async () => saved, vi.fn().mockResolvedValue(undefined), vi.fn().mockResolvedValue({ ok: true, data: [] }))
    const view = renderEditor(<><SidebarPrefs /><Sidebar activeId="settings" navigate={vi.fn()} /></>)
    try {
      await act(async () => {})
      expect(setNavPrefs).not.toHaveBeenCalled()
      expect(screen.getByText('Pinned 2/2')).toBeInTheDocument()
      expect(screen.queryByText(/no longer installed/)).not.toBeInTheDocument()
      await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
      expect(setNavPrefs).toHaveBeenCalledExactlyOnceWith({ ...saved, pinned: ['documents'] })
      expect(screen.getByText('Pinned 1/2')).toBeInTheDocument()
      expect(screen.getByText('Empty slot')).toBeInTheDocument()
      expect(screen.getByText('acme is no longer installed and was unpinned.')).toBeInTheDocument()
    } finally {
      view.unmount()
      vi.useRealTimers()
    }
  })

  it.each(['success', 'failure then success', 'manifest before preferences', 'module removed during session'])('F6 releases removed module slots and notifies after %s', async scenario => {
    vi.useFakeTimers()
    let online = scenario !== 'failure then success'
    let installed = scenario === 'module removed during session'
    let finishRead: (prefs: NavPrefs) => void = () => {}
    const saved = { pinned: ['documents', 'acme'], slotCount: 2, showOverview: true }
    const read = scenario === 'manifest before preferences' ? new Promise<NavPrefs>(resolve => { finishRead = resolve }) : Promise.resolve(saved)
    const backendProxy = vi.fn().mockImplementation(() => Promise.resolve({ ok: online, data: installed ? [{ id: 'acme', type: 'ui', name: 'Acme' }] : [] }))
    const setNavPrefs = installPrefs(() => read, vi.fn().mockResolvedValue(undefined), backendProxy)
    const view = renderEditor(<><SidebarPrefs /><Sidebar activeId="settings" navigate={vi.fn()} /><CapabilitiesPage /></>)
    try {
      await act(async () => {})
      if (scenario === 'failure then success' || scenario === 'module removed during session') {
        expect(setNavPrefs).not.toHaveBeenCalled()
        expect(screen.getByText('Pinned 2/2')).toBeInTheDocument()
        online = true
        installed = false
        await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
      } else if (scenario === 'manifest before preferences') {
        expect(setNavPrefs).not.toHaveBeenCalled()
        await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
        await act(async () => { finishRead(saved) })
      }
      if (scenario !== 'manifest before preferences') {
        expect(setNavPrefs).not.toHaveBeenCalled()
        await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
      }
      expect(setNavPrefs).toHaveBeenLastCalledWith({ ...saved, pinned: ['documents'] })
      expect(screen.getByText('acme is no longer installed and was unpinned.')).toBeInTheDocument()
      expect(screen.getByText('Pinned 1/2')).toBeInTheDocument()
      expect(screen.getByText('Empty slot')).toBeInTheDocument()
      expect(screen.getByRole('button', { name: 'Pin Design' })).toBeEnabled()
      await act(async () => { fireEvent.click(screen.getByRole('button', { name: 'Pin Design' })) })
      expect(setNavPrefs).toHaveBeenLastCalledWith({ ...saved, pinned: ['documents', 'design'] })
    } finally {
      view.unmount()
      vi.useRealTimers()
    }
  })

  it('F6 lists unavailable offline pins with matching sidebar counts and allows manual unpinning', async () => {
    const setNavPrefs = installPrefs(async () => ({ pinned: ['documents', 'acme'], slotCount: 2, showOverview: true }))
    renderEditor(<><SidebarPrefs /><Sidebar activeId="settings" navigate={vi.fn()} /></>)
    expect(await screen.findByText('acme — Unavailable')).toBeInTheDocument()
    expect(screen.getByText('Pinned 2/2')).toBeInTheDocument()
    expect(screen.queryByText('Empty slot')).not.toBeInTheDocument()
    expect(within(screen.getByRole('navigation')).queryByText(/acme/)).not.toBeInTheDocument()
    fireEvent.click(screen.getByRole('switch', { name: 'Show Overview in sidebar' }))
    await waitFor(() => expect(setNavPrefs).toHaveBeenLastCalledWith({ pinned: ['documents', 'acme'], slotCount: 2, showOverview: false }))
    fireEvent.click(screen.getByRole('button', { name: 'Unpin acme' }))
    await waitFor(() => expect(setNavPrefs).toHaveBeenLastCalledWith({ pinned: ['documents'], slotCount: 2, showOverview: false }))
    expect(screen.queryByText(/Unavailable/)).not.toBeInTheDocument()
    expect(screen.getByText('Pinned 1/2')).toBeInTheDocument()
  })

  it('F6 retries failed cleanup on the next write, returns dropped ids and localizes the removal notice', async () => {
    vi.useFakeTimers()
    let online = true
    const setNavPrefs = installPrefs(async () => ({ pinned: ['documents', 'acme'], slotCount: 2, showOverview: true }),
      vi.fn().mockRejectedValueOnce(new Error('disk full')).mockResolvedValue(undefined), vi.fn().mockImplementation(() => Promise.resolve({ ok: online, data: [] })))
    function Editor() {
      const { setPrefs } = useContext(NavPrefsContext)
      return <button onClick={() => void setPrefs(prev => ({ ...prev, showOverview: false })).then(ids => { document.title = ids.join(',') })}>Retry</button>
    }
    const view = renderEditor(<><SidebarPrefs /><Editor /></>)
    try {
      await act(async () => {})
      expect(setNavPrefs).not.toHaveBeenCalled()
      await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
      expect(setNavPrefs).toHaveBeenCalledTimes(1)
      expect(screen.queryByText(/no longer installed/)).not.toBeInTheDocument()
      expect(screen.getByRole('button', { name: 'Unpin acme' })).toBeInTheDocument()
      online = false
      await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
      expect(setNavPrefs).toHaveBeenCalledTimes(1)
      await act(async () => { fireEvent.click(screen.getByRole('button', { name: 'Retry' })) })
      expect(document.title).toBe('acme')
      expect(setNavPrefs).toHaveBeenLastCalledWith({ pinned: ['documents'], slotCount: 2, showOverview: false })
      expect(screen.getByText('acme is no longer installed and was unpinned.')).toBeInTheDocument()
      act(() => { setLocale('zh') })
      expect(screen.getByText('acme 已卸载，已取消钉选。')).toBeInTheDocument()
      act(() => { setLocale('th') })
      expect(screen.getByText('acme ไม่ได้ติดตั้งแล้วและถูกเลิกปักหมุด')).toBeInTheDocument()
    } finally {
      act(() => { setLocale('en') })
      view.unmount()
      vi.useRealTimers()
    }
  })

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
