import { useEffect, useRef, useState } from 'react'
import type { Agent24API } from '../main/preload'
import type { ModuleInfo } from '../shared/ipc-types'

declare global {
  interface Window { agent24: Agent24API }
}
import ChatPage from './pages/Chat'
import WorkbenchPage from './pages/Workbench'
import ModelsPage from './pages/Models'
import SettingsPage from './pages/Settings'
import ModulesManagerPage from './pages/ModulesManager'
import HelloModule from './pages/modules/HelloModule'
import CodeSandboxPage from './pages/CodeSandbox'

// Static module route map — M2 will replace this with dynamic import()
const MODULE_PAGES: Record<string, React.ComponentType> = {
  '/modules/hello': HelloModule,
  'codebox': CodeSandboxPage,
}

type BuiltinPage = 'chat' | 'workbench' | 'models' | 'settings' | 'modules-manager'
type Page = BuiltinPage | string  // string = module route

const BUILTIN_NAV: { id: BuiltinPage; icon: string; label: string }[] = [
  { id: 'chat',            icon: '💬', label: '对话' },
  { id: 'workbench',       icon: '🔧', label: '工作台' },
  { id: 'models',          icon: '🤖', label: '模型' },
  { id: 'modules-manager', icon: '🧩', label: '模块管理' },
  { id: 'settings',        icon: '⚙️', label: '设置' },
]

const BUILTIN_TITLES: Record<BuiltinPage, string> = {
  chat: '对话', workbench: '工作台', models: '模型管理',
  'modules-manager': '模块管理', settings: '设置',
}

export function App(): JSX.Element {
  const [page, setPage] = useState<Page>('chat')
  const [backendOk, setBackendOk] = useState<boolean | null>(null)
  const [version, setVersion] = useState<string>('')
  const [sidebarOpen, setSidebarOpen] = useState(true)
  const [darkMode, setDarkMode] = useState(true)
  const [modules, setModules] = useState<ModuleInfo[]>([])
  const [llmLabel, setLlmLabel] = useState('Detecting…')
  const initDone = useRef(false)

  useEffect(() => {
    if (initDone.current) return
    initDone.current = true

    void window.agent24.getAppVersion().then(setVersion)

    // Backend health + module list polling
    const checkBackend = () => {
      window.agent24.backendProxy({ method: 'GET', path: '/health' })
        .then((res) => {
          setBackendOk(res.ok)
          if (res.ok) {
            void window.agent24.modulesList().then(setModules)
          }
        })
        .catch(() => setBackendOk(false))
    }
    checkBackend()
    const backendTimer = setInterval(checkBackend, 5000)

    // oMLX auto-detect on startup
    void (async () => {
      const detected = await window.agent24.omlxDetect()
      if (detected) {
        setLlmLabel(`${detected.models[0] ?? 'unknown'} · oMLX`)
      } else {
        // Try to start oMLX
        const started = await window.agent24.omlxStart(8088, 'xiaobao8088')
        if (started.ok) {
          // Poll until ready
          for (let i = 0; i < 5; i++) {
            await new Promise((r) => setTimeout(r, 2000))
            const r = await window.agent24.omlxModels(started.url, 'xiaobao8088')
            if (r.ok && r.models.length > 0) {
              setLlmLabel(`${r.models[0]} · oMLX`)
              break
            }
          }
        } else {
          setLlmLabel('No AI runtime')
        }
      }
    })()

    return () => clearInterval(backendTimer)
  }, [])

  // UI modules with navItem get injected into sidebar
  const moduleNavItems = modules.filter(
    (m) => (m.type === 'ui' || m.type === 'hybrid') && m.navItem,
  )

  const pageTitle = BUILTIN_TITLES[page as BuiltinPage]
    ?? modules.find((m) => m.navItem?.route === page)?.name
    ?? page

  return (
    <div className={`app${darkMode ? '' : ' light'}`}>
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
          {BUILTIN_NAV.map(item => (
            <button
              key={item.id}
              className={`nav-item ${page === item.id ? 'active' : ''}`}
              onClick={() => setPage(item.id)}
            >
              <span className="icon">{item.icon}</span>
              <span>{item.label}</span>
            </button>
          ))}

          {/* Dynamically injected UI module nav items */}
          {moduleNavItems.length > 0 && (
            <>
              <div className="nav-section">能力模块</div>
              {moduleNavItems.map((m) => (
                <button
                  key={m.id}
                  className={`nav-item ${page === m.navItem!.route ? 'active' : ''}`}
                  onClick={() => setPage(m.navItem!.route)}
                >
                  <span className="icon">{m.navItem!.icon}</span>
                  <span>{m.navItem!.label}</span>
                </button>
              ))}
            </>
          )}

          {moduleNavItems.length === 0 && (
            <>
              <div className="nav-section">能力模块</div>
              <button
                className="nav-item"
                onClick={() => setPage('modules-manager')}
                style={{ opacity: 0.7 }}
              >
                <span className="icon">➕</span>
                <span>安装模块</span>
              </button>
            </>
          )}
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
          <span className="topbar-title">{pageTitle}</span>
          {page === 'chat' && (
            <span className="topbar-sub">{llmLabel}</span>
          )}
          <button
            className="theme-toggle-btn"
            onClick={() => setDarkMode(d => !d)}
            title={darkMode ? '切换白天模式' : '切换夜间模式'}
          >
            {darkMode ? '🌙' : '☀️'}
          </button>
        </div>

        {page === 'chat'             && <ChatPage />}
        {page === 'workbench'        && <WorkbenchPage />}
        {page === 'models'           && <ModelsPage />}
        {page === 'modules-manager'  && <ModulesManagerPage />}
        {page === 'settings'         && <SettingsPage />}
        {/* Module UI pages — static map in M1, dynamic import() in M2 */}
        {moduleNavItems.map((m) => {
          if (page !== m.navItem!.route) return null
          const ModulePage = MODULE_PAGES[m.navItem!.route]
          return ModulePage
            ? <ModulePage key={m.id} />
            : (
              <div key={m.id} className="content">
                <div className="page-title">{m.name}</div>
                <p style={{ color: 'var(--muted)', fontSize: 13 }}>Component not registered in MODULE_PAGES.</p>
              </div>
            )
        })}
      </div>
    </div>
  )
}
