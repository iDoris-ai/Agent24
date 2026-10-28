import { useContext } from 'react'
import { useT } from '../i18n'
import { appMessages } from '../App.i18n'
import { capabilitiesMessages } from './capabilities.i18n'
import { CapabilitiesContext, NavPrefsContext } from './types'

export function Sidebar({ activeId, navigate }: { activeId: string; navigate: (page: string) => void }) {
  const { defs, open, approvalsWaiting } = useContext(CapabilitiesContext)
  const { prefs } = useContext(NavPrefsContext)
  const t = useT(appMessages)
  const c = useT(capabilitiesMessages)
  const pinned = prefs.pinned.flatMap(id => {
    const def = defs.find(d => d.id === id && !d.fixed)
    return def ? [def] : []
  })
  const item = (id: string, label: string, onClick: () => void, badge = 0) => <button
    key={id} className={`nav-item${activeId === id ? ' active' : ''}`}
    aria-current={activeId === id ? 'page' : undefined} onClick={onClick}
  ><span>{label}</span>{badge > 0 && <span className="nav-badge">{badge}</span>}</button>
  return <nav className="sidebar-nav" aria-label={c('navigation')}>
    {prefs.showOverview && item('home', t('nav.home'), () => navigate('home'), approvalsWaiting)}
    {item('assistant', c('assistant.name'), () => open('assistant', 'sidebar'))}
    <div className="nav-section">{c('pinned', { count: pinned.length, slots: prefs.slotCount })}</div>
    {pinned.map(def => item(def.id, c(def.nameKey), () => open(def.id, 'sidebar')))}
    {Array.from({ length: Math.max(0, prefs.slotCount - pinned.length) }, (_, i) => <div className="nav-empty-slot" key={i}>{c('emptySlot')}</div>)}
    <div className="nav-bottom">
      {item('capabilities', t('nav.capabilities'), () => navigate('capabilities'), approvalsWaiting)}
      {item('settings', t('nav.settings'), () => navigate('settings'))}
    </div>
  </nav>
}
