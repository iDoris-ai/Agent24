import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from 'react'
import {
  CapabilitiesContext, NavPrefsContext, NAV_PREFS_DEFAULT, clampPrefs,
  type CapabilityId, type NavPrefs, type OpenOrigin,
} from './types'
import { CAPABILITIES, withDynamicCapabilities } from './registry'

type Route = { kind: 'page'; page: string } | { kind: 'capability'; id: CapabilityId; origin: OpenOrigin }
const RouteContext = createContext<{ route: Route; navigate: (page: string) => void }>({
  route: { kind: 'page', page: 'home' }, navigate: () => {},
})
export const useCapabilityRoute = () => useContext(RouteContext)

export function CapabilitiesProvider({ children }: { children: ReactNode }) {
  const [defs, setDefs] = useState(CAPABILITIES)
  const [route, setRoute] = useState<Route>({ kind: 'page', page: 'home' })
  const [approvalsWaiting, setApprovalsWaiting] = useState(0)
  const navigate = useCallback((page: string) => setRoute({ kind: 'page', page }), [])
  const open = useCallback((id: CapabilityId, origin: OpenOrigin) => {
    if (defs.some(d => d.id === id)) setRoute({ kind: 'capability', id, origin })
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
  // Keep raw ids until manifests arrive, so startup cannot discard a pinned dynamic module.
  const [stored, setStored] = useState(NAV_PREFS_DEFAULT)
  const revision = useRef(0)
  const writes = useRef<Promise<void>>(Promise.resolve())
  useEffect(() => {
    let disposed = false
    const initialRevision = revision.current
    void window.agent24.getNavPrefs?.().then(value => {
      if (!disposed && revision.current === initialRevision) setStored(value)
    }).catch(() => { /* Main supplies defaults for damaged files; IPC failure uses defaults too. */ })
    return () => { disposed = true }
  }, [])

  const setPrefs = (next: NavPrefs): Promise<CapabilityId[]> => {
    const normalized = clampPrefs(next, defs)
    ++revision.current
    const write = writes.current.catch(() => {}).then(async () => {
      await window.agent24.setNavPrefs(normalized.prefs)
      setStored(normalized.prefs)
    })
    writes.current = write
    return write.then(() => normalized.dropped)
  }
  return <NavPrefsContext.Provider value={{ prefs: clampPrefs(stored, defs).prefs, setPrefs }}>{children}</NavPrefsContext.Provider>
}
