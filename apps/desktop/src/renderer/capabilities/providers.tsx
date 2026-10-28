import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from 'react'
import {
  CapabilitiesContext, NavPrefsContext, NAV_PREFS_DEFAULT, clampPrefs,
  type CapabilityId, type NavPrefsApi, type OpenOrigin,
} from './types'
import { CAPABILITIES, withDynamicCapabilities } from './registry'

type Route = { kind: 'page'; page: string; initialTab?: 'general' | 'activity'; key?: number } | { kind: 'capability'; id: CapabilityId; origin: OpenOrigin }
const RouteContext = createContext<{ route: Route; navigate: (page: string) => void }>({
  route: { kind: 'page', page: 'home' }, navigate: () => {},
})
export const useCapabilityRoute = () => useContext(RouteContext)

export function CapabilitiesProvider({ children }: { children: ReactNode }) {
  const [defs, setDefs] = useState(CAPABILITIES)
  const [route, setRoute] = useState<Route>({ kind: 'page', page: 'home' })
  const [approvalsWaiting, setApprovalsWaiting] = useState(0)
  const settingsVisit = useRef(0)
  const navigate = useCallback((page: string) => setRoute({ kind: 'page', page }), [])
  const open = useCallback((id: CapabilityId, origin: OpenOrigin) => {
    if (id === 'settings/general' || id === 'settings/activity') {
      setRoute({ kind: 'page', page: 'settings', initialTab: id === 'settings/general' ? 'general' : 'activity', key: ++settingsVisit.current })
    } else if (id === 'capabilities') {
      setRoute({ kind: 'page', page: 'capabilities' })
    } else if (defs.some(d => d.id === id)) setRoute({ kind: 'capability', id, origin })
  }, [defs])
  useEffect(() => {
    if (route.kind === 'capability' && !defs.some(d => d.id === route.id)) navigate('capabilities')
  }, [defs, route, navigate])
  useEffect(() => {
    let disposed = false
    let modulesBusy = false
    let approvalsBusy = false
    const poll = () => {
      if (!modulesBusy) {
        modulesBusy = true
        void window.agent24.backendProxy({ method: 'GET', path: '/api/modules' }).then(res => {
          if (!disposed && res.ok && Array.isArray(res.data)) setDefs(withDynamicCapabilities(res.data))
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
    defs, approvalsWaiting,
    open,
  }}>
    <RouteContext.Provider value={{ route, navigate }}>{children}</RouteContext.Provider>
  </CapabilitiesContext.Provider>
}

export function NavPrefsProvider({ children }: { children: ReactNode }) {
  const { defs } = useContext(CapabilitiesContext)
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

  const setPrefs: NavPrefsApi['setPrefs'] = (next) => {
    const write = writes.current.catch(() => {}).then(async () => {
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
      const normalized = clampPrefs(requested, defs, true)
      await window.agent24.setNavPrefs(normalized.prefs)
      latest.current = normalized.prefs
      setStored(normalized.prefs)
      return normalized.dropped
    })
    writes.current = write.then(() => {}, () => {})
    return write
  }
  const raw = clampPrefs(stored, defs, true).prefs
  return <NavPrefsContext.Provider value={{ prefs: clampPrefs(raw, defs).prefs, pinnedCount: raw.pinned.length, setPrefs }}>{children}</NavPrefsContext.Provider>
}
