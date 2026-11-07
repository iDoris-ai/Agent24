import { useContext } from 'react'
import { useT } from '../i18n'
import { appMessages } from '../App.i18n'
import { capabilitiesMessages } from './capabilities.i18n'
import { CapabilitiesContext, NavPrefsContext } from './types'
import { ChatHistoryList } from '../pages/chat/ChatHistoryList'

export function Sidebar({ activeId, navigate }: { activeId: string; navigate: (page: string) => void }) {
  const { defs, open, approvalsWaiting } = useContext(CapabilitiesContext)
  const { prefs, pinnedCount = prefs.pinned.length } = useContext(NavPrefsContext)
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
    <ChatHistoryList showChat={() => open('assistant', 'sidebar')} />
    {/* David 11-08: the group says what it is, not how full it is; the
        limit shows only where it stops a pin (Capabilities, Settings). */}
    <div className="nav-section">{c('pinned')}</div>
    {pinned.map(def => item(def.id, c(def.nameKey), () => open(def.id, 'sidebar')))}
    {pinnedCount < prefs.slotCount && (
      <button className="nav-pin-more" onClick={() => navigate('capabilities')}>{c('pinMore')}</button>
    )}
    <div className="nav-bottom">
      {item('capabilities', t('nav.capabilities'), () => navigate('capabilities'), approvalsWaiting)}
      {item('settings', t('nav.settings'), () => navigate('settings'))}
    </div>
  </nav>
}
