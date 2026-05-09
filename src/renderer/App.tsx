import { useEffect, useState } from 'react'
import type { Agent24API } from '../main/preload'

declare global {
  interface Window { agent24: Agent24API }
}
import ChatPage from './pages/Chat'
import WorkbenchPage from './pages/Workbench'
import ModelsPage from './pages/Models'
import SettingsPage from './pages/Settings'

type Page = 'chat' | 'workbench' | 'models' | 'settings'

const NAV: { id: Page; icon: string; label: string }[] = [
  { id: 'chat',      icon: '💬', label: '对话' },
  { id: 'workbench', icon: '🔧', label: '工作台' },
  { id: 'models',    icon: '🤖', label: '模型' },
  { id: 'settings',  icon: '⚙️', label: '设置' },
]

const PAGE_TITLES: Record<Page, string> = {
  chat:      '对话',
  workbench: '工作台',
  models:    '模型管理',
  settings:  '设置',
}

export function App(): JSX.Element {
  const [page, setPage] = useState<Page>('chat')
  const [backendOk, setBackendOk] = useState<boolean | null>(null)
  const [version, setVersion] = useState<string>('')
  const [sidebarOpen, setSidebarOpen] = useState(true)
  const [darkMode, setDarkMode] = useState(true)

  useEffect(() => {
    void window.agent24.getAppVersion().then(setVersion)

    const check = () => {
      window.agent24.backendProxy({ method: 'GET', path: '/health' })
        .then((res) => setBackendOk(res.ok))
        .catch(() => setBackendOk(false))
    }
    check()
    const t = setInterval(check, 5000)
    return () => clearInterval(t)
  }, [])

  return (
    <div className={`app${darkMode ? '' : ' light'}`}>
      {/* Floating expand button shown only when sidebar is collapsed */}
      {!sidebarOpen && (
        <button className="sidebar-expand-btn" onClick={() => setSidebarOpen(true)}>›</button>
      )}

      {/* ── Sidebar ── */}
      <aside className={`sidebar${sidebarOpen ? '' : ' collapsed'}`}>
        <div className="sidebar-logo">
          <div className="sidebar-logo-text">
            Agent24
            <span>v{version || '…'}</span>
          </div>
          <button className="sidebar-collapse-btn" onClick={() => setSidebarOpen(false)}>‹</button>
        </div>

        <nav className="sidebar-nav">
          {NAV.map(item => (
            <button
              key={item.id}
              className={`nav-item ${page === item.id ? 'active' : ''}`}
              onClick={() => setPage(item.id)}
            >
              <span className="icon">{item.icon}</span>
              <span>{item.label}</span>
            </button>
          ))}

          <div className="nav-section">能力模块</div>
          <button className="nav-item" style={{ opacity: 0.5, cursor: 'default' }}>
            <span className="icon">📚</span>
            <span>小黑书 <span style={{ fontSize: 10 }}>模块</span></span>
          </button>
          <button className="nav-item" style={{ opacity: 0.5, cursor: 'default' }}>
            <span className="icon">➕</span>
            <span>安装模块</span>
          </button>
        </nav>

        <div className="sidebar-footer">
          <div className="backend-status">
            <div className={`status-dot ${backendOk === true ? 'online' : backendOk === false ? 'offline' : ''}`} />
            {backendOk === true && <span>后端服务运行中 :8765</span>}
            {backendOk === false && <span>后端服务离线</span>}
            {backendOk === null && <span>检测中…</span>}
          </div>
        </div>
      </aside>

      {/* ── Main ── */}
      <div className={`main${sidebarOpen ? '' : ' sidebar-hidden'}`}>
        <div className="topbar">
          <span className="topbar-title">{PAGE_TITLES[page]}</span>
          {page === 'chat' && (
            <span className="topbar-sub">Qwen3-30B-A3B · oMLX</span>
          )}
          <button
            className="theme-toggle-btn"
            onClick={() => setDarkMode(d => !d)}
            title={darkMode ? '切换白天模式' : '切换夜间模式'}
          >
            {darkMode ? '🌙' : '☀️'}
          </button>
        </div>

        {page === 'chat'      && <ChatPage />}
        {page === 'workbench' && <WorkbenchPage />}
        {page === 'models'    && <ModelsPage />}
        {page === 'settings'  && <SettingsPage />}
      </div>
    </div>
  )
}
