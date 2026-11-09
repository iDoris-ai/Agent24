import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from 'react'
import {
  CapabilitiesContext, NavPrefsContext, NAV_PREFS_DEFAULT, clampPrefs,
  type CapabilitiesApi, type CapabilityId, type NavPrefsApi, type OpenOptions, type OpenOrigin,
} from './types'
import { CAPABILITIES, withDynamicCapabilities } from './registry'
import { useT } from '../i18n'
import { settingsMessages } from '../pages/Settings.i18n'
import { capabilitiesMessages } from './capabilities.i18n'

type Route = { kind: 'page'; page: string; initialTab?: 'general' | 'activity' | 'connections'; key?: number; focus?: 'slots' } | { kind: 'capability'; id: CapabilityId; origin: OpenOrigin }
const RouteContext = createContext<{ route: Route; navigate: (page: string) => void }>({
  route: { kind: 'page', page: 'home' }, navigate: () => {},
})
export const useCapabilityRoute = () => useContext(RouteContext)

/** M4 A6 (#1688): what the pages showing say of unsaved changes. */
interface LeaveGuard { dirty: () => boolean; message: () => string | undefined }
const LeaveGuardContext = createContext<Set<LeaveGuard> | null>(null)
/** For a page with changes it would lose: while it is mounted (shown or
 *  hidden), going to another page asks first when `dirty` says there are
 *  some, once however many guards say so, with the first such guard's
 *  `message` or a plain one (Knowledge #1681). The page showing already is
 *  not asked about; closing the window or quitting the app is not. */
export function useLeaveGuard(dirty: boolean | (() => boolean), message?: string): void {
  const guards = useContext(LeaveGuardContext)
  const latest = useRef({ dirty, message })
  latest.current = { dirty, message }
  useEffect(() => {
    if (!guards) return
    const guard: LeaveGuard = {
      dirty: () => { const d = latest.current.dirty; return typeof d === 'function' ? d() : d },
      message: () => latest.current.message,
    }
    guards.add(guard)
    return () => { guards.delete(guard) }
  }, [guards])
}
const samePage = (a: Route, b: Route) =>
  a.kind === b.kind && (a.kind === 'page' ? a.page === (b as typeof a).page : a.id === (b as typeof a).id)

export function CapabilitiesProvider({ children }: { children: ReactNode }) {
  const [{ defs, moduleInventory }, setRegistry] = useState<Pick<CapabilitiesApi, 'defs' | 'moduleInventory'>>({ defs: CAPABILITIES })
  const [route, setRoute] = useState<Route>({ kind: 'page', page: 'home' })
  const [approvalsWaiting, setApprovalsWaiting] = useState(0)
  const settingsVisit = useRef(0)
  const t = useT(capabilitiesMessages)
  const [guards] = useState(() => new Set<LeaveGuard>())
  const current = useRef(route)
  current.current = route
  // Another page is gone to only once the user agrees to lose what a page
  // there has not saved; the one showing is not asked about (#1688).
  const go = useCallback((next: Route) => {
    const dirty = samePage(current.current, next) ? [] : [...guards].filter(g => g.dirty())
    if (dirty.length > 0 && !window.confirm(dirty.map(g => g.message()).find(Boolean) ?? t('leave.unsaved'))) return
    setRoute(next)
  }, [guards, t])
  const navigate = useCallback((page: string) => go({ kind: 'page', page }), [go])
  const open = useCallback((id: CapabilityId, origin: OpenOrigin, options?: OpenOptions) => {
    if (id === 'settings/general' || id === 'settings/activity' || id === 'settings/connections') {
      // Only an explicit { focus: 'slots' } lands on the pinned-slots row. Other entries stay at the top of the tab.
      // settings/connections: the Knowledge binding, opened from the knowledge page (#1625).
      const focus = id === 'settings/general' ? options?.focus : undefined
      go({ kind: 'page', page: 'settings', initialTab: id === 'settings/general' ? 'general' : id === 'settings/activity' ? 'activity' : 'connections', key: ++settingsVisit.current, ...(focus ? { focus } : {}) })
    } else if (id === 'capabilities') {
      go({ kind: 'page', page: 'capabilities' })
    } else if (defs.some(d => d.id === id)) go({ kind: 'capability', id, origin })
  }, [defs, go])
  useEffect(() => {
    // The module is gone, and its page with it: nothing to ask.
    if (route.kind === 'capability' && !defs.some(d => d.id === route.id)) setRoute({ kind: 'page', page: 'capabilities' })
  }, [defs, route])
  useEffect(() => {
    let disposed = false
    let modulesBusy = false
    let approvalsBusy = false
    const poll = () => {
      if (!modulesBusy) {
        modulesBusy = true
        void window.agent24.backendProxy({ method: 'GET', path: '/api/modules' }).then(res => {
          if (disposed || !res.ok || !Array.isArray(res.data)) return
          const ids = new Set<CapabilityId>()
          const disabledIds = new Set<CapabilityId>()
          for (const value of res.data) {
            const m: unknown = value
            if (!m || typeof m !== 'object' || !('id' in m) || typeof m.id !== 'string') continue
            ids.add(m.id)
            if ('enabled' in m && m.enabled === false) disabledIds.add(m.id)
          }
          const defs = withDynamicCapabilities(res.data)
          setRegistry(prev => ({ defs, moduleInventory: { ids, disabledIds, previousIds: prev.moduleInventory?.ids } }))
        }).catch(() => { /* Keep the last successful registry while offline. */ }).finally(() => { modulesBusy = false })
      }
      if (!approvalsBusy) {
        approvalsBusy = true
        void window.agent24.backendProxy({ method: 'GET', path: '/api/v1/approvals?status=pending' }).then(res => {
          if (!disposed && res.ok && res.data && typeof res.data === 'object'
            && 'approvals' in res.data && Array.isArray(res.data.approvals)) setApprovalsWaiting(res.data.approvals.length)
        }).catch(() => { /* Keep the last successful count while offline. */ }).finally(() => { approvalsBusy = false })
      }
    }
    poll()
    const timer = setInterval(poll, 5000)
    return () => { disposed = true; clearInterval(timer) }
  }, [])

  return <CapabilitiesContext.Provider value={{
    defs, moduleInventory, manifestsLoaded: moduleInventory !== undefined, approvalsWaiting,
    open,
  }}>
    <RouteContext.Provider value={{ route, navigate }}>
      <LeaveGuardContext.Provider value={guards}>{children}</LeaveGuardContext.Provider>
    </RouteContext.Provider>
  </CapabilitiesContext.Provider>
}

function removedModulePins(pinned: readonly CapabilityId[], { defs, moduleInventory }: Pick<CapabilitiesApi, 'defs' | 'moduleInventory'>) {
  if (!moduleInventory?.previousIds) return []
  const { ids, previousIds } = moduleInventory
  return [...new Set(pinned.filter(id => !defs.some(d => d.id === id) && !ids.has(id) && !previousIds.has(id)))]
}

export function NavPrefsProvider({ children }: { children: ReactNode }) {
  const { defs, moduleInventory } = useContext(CapabilitiesContext)
  const registry = useRef({ defs, moduleInventory })
  registry.current = { defs, moduleInventory }
  const t = useT(settingsMessages)
  const [removedNotice, setRemovedNotice] = useState<CapabilityId[]>([])
  const [stored, setStored] = useState(NAV_PREFS_DEFAULT)
  const latest = useRef(NAV_PREFS_DEFAULT)
  const writes = useRef<Promise<void>>(Promise.resolve())
  useEffect(() => {
    let disposed = false
    writes.current = Promise.resolve().then(() => window.agent24.getNavPrefs?.()).then(value => {
      if (!disposed && value) {
        latest.current = value
        setStored(value)
      }
    }).catch(() => { /* Main supplies defaults for damaged files; IPC failure uses defaults too. */ })
    return () => { disposed = true }
  }, [])

  const setPrefs = useCallback<NavPrefsApi['setPrefs']>((next) => {
    const write = writes.current.catch(() => {}).then(async () => {
      // Read the current registry after startup and any earlier writes settle.
      const { defs } = registry.current
      const prev = latest.current
      const requested = typeof next === 'function' ? next({ ...prev, pinned: [...prev.pinned] }) : { ...next, pinned: [...next.pinned] }
      if (typeof next !== 'function') {
        // A displayed snapshot cannot explicitly remove a hidden pin.
        const known = new Set(defs.map(d => d.id))
        prev.pinned.forEach((id, index) => {
          if (!known.has(id) && !requested.pinned.includes(id)) requested.pinned.splice(index, 0, id)
        })
      }
      requested.pinned = requested.pinned.filter(id => prev.pinned.includes(id) || defs.some(d => d.id === id))
      const removed = removedModulePins(requested.pinned, registry.current)
      requested.pinned = requested.pinned.filter(id => !removed.includes(id))
      const normalized = clampPrefs(requested, defs, true)
      await window.agent24.setNavPrefs(normalized.prefs)
      latest.current = normalized.prefs
      setStored(normalized.prefs)
      setRemovedNotice(removed)
      return [...removed, ...normalized.dropped]
    })
    writes.current = write.then(() => {}, () => {})
    return write
  }, [])
  useEffect(() => {
    if (!moduleInventory?.previousIds) return
    let disposed = false
    void writes.current.then(() => {
      if (!disposed && removedModulePins(latest.current.pinned, { defs, moduleInventory }).length > 0) {
        void setPrefs(prev => prev).catch(() => { /* Retry normalization on the next write or successful poll. */ })
      }
    })
    return () => { disposed = true }
  }, [defs, moduleInventory, setPrefs])
  const raw = clampPrefs(stored, defs, true).prefs
  return <NavPrefsContext.Provider value={{ prefs: clampPrefs(raw, defs).prefs, allPinned: raw.pinned, pinnedCount: raw.pinned.length, setPrefs }}>
    {children}
    {removedNotice.length > 0 && <p className="nav-prefs-notice" role="status">{t('sidebar.removed', { names: removedNotice.join(', ') })}</p>}
  </NavPrefsContext.Provider>
}
