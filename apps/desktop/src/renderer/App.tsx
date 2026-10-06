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
import ServiceBoxDemoPage from './pages/ServiceBoxDemo'
import RunsPage from './pages/Runs'
import SchedulesPage from './pages/Schedules'
import ApprovalsPage from './pages/Approvals'
import VoicePanel from './pages/voice/VoicePanel'
import CreativePage from './pages/Creative'
import MemoryPage from './pages/Memory'
import logoSidebar from './assets/logo-sidebar.png'

// Static module route map — M2 will replace this with dynamic import()
const MODULE_PAGES: Record<string, React.ComponentType> = {
  '/modules/hello': HelloModule,
  'codebox': CodeSandboxPage,
  'service-box': ServiceBoxDemoPage,
}

type BuiltinPage =
  | 'chat'
  | 'workbench'
  | 'runs'
  | 'schedules'
  | 'approvals'
  | 'voice'
  | 'creative'
  | 'models'
  | 'memory'
  | 'settings'
  | 'modules-manager'
type Page = BuiltinPage | string  // string = module route

const BUILTIN_NAV: { id: BuiltinPage; icon: string; label: string }[] = [
  { id: 'chat',            icon: '💬', label: '对话' },
  { id: 'workbench',       icon: '🔧', label: '工作台' },
  { id: 'runs',            icon: '📋', label: '任务' },
  { id: 'schedules',       icon: '⏰', label: '调度' },
  { id: 'approvals',       icon: '🔐', label: '审批' },
  { id: 'voice',           icon: '🎙️', label: '语音' },
  { id: 'creative',        icon: '🎨', label: 'Creative' },
  { id: 'models',          icon: '🤖', label: '模型' },
  { id: 'modules-manager', icon: '🧩', label: '模块管理' },
  // M1-T11: 记忆页紧挨设置，和它共享"这是你对 Agent24 的控制面"这同一个位置。
  { id: 'memory',          icon: '🧠', label: '记忆' },
  { id: 'settings',        icon: '⚙️', label: '设置' },
]

const BUILTIN_TITLES: Record<BuiltinPage, string> = {
  chat: '对话', workbench: '工作台', runs: '运行任务',
  schedules: '定时调度', approvals: '待审批', voice: '语音',
  creative: 'Creative', models: '模型管理', 'modules-manager': '模块管理',
  memory: '记忆', settings: '设置',
}

// FU-89: oMLX's `/v1/models` carries no type/capability field — only bare ids
// (see fetchOmlxModels in main/ipc/index.ts) — so a real "is this a chat
// model" flag isn't available to filter on. Until it is, exclude known
// non-chat model families by name so the chat page's default model is never
// silently a text-to-image / video / ASR / TTS / embedding / rerank model
// (FU-89 was filed because it picked `models[0]` unconditionally and that
// happened to be a FLUX image model).
const NON_CHAT_MODEL_PATTERNS: RegExp[] = [
  /flux/i, /stable-?diffusion/i, /\bsdxl\b/i, /\bsd3\b/i, /kolors/i, /pixart/i, // image gen
  /\bltx\b/i, /cogvideo/i, /hunyuan-?video/i, /\bwan-?2/i,                     // video gen
  /whisper/i, /cosyvoice/i, /\btts\b/i, /xtts/i, /\bvits\b/i, /\bf5-tts\b/i, /\bbark\b/i, // ASR/TTS
  /\bbge\b/i, /\bgte\b/i, /\be5\b/i, /embed/i, /rerank/i,                      // embedding/rerank
  // FU-89 follow-up: a real demo picked GLM-OCR-bf16 as the "chat" model —
  // OCR/vision-language models answer with layout/caption text, not
  // conversation, and must be excluded the same way image-gen models are.
  /ocr/i, /-vl\b/i, /\bvl-/i, /paddleocr/i, /mage-vl/i,                        // OCR / vision-language
]

/** First model id that doesn't look like a known non-chat model, or
 * `undefined` if every candidate is (FU-89). Exported for its unit test. */
export function pickChatModel(models: string[]): string | undefined {
  return models.find((m) => !NON_CHAT_MODEL_PATTERNS.some((re) => re.test(m)))
}

const NO_CHAT_MODEL_LABEL = '无可用对话模型（仅检测到生图/嵌入等模型）'

/** ME4-desktop-model-ui: `GET /api/v1/models`'s `default_model` field
 * (`rust/apps/agent24d/src/routes.rs`) — the daemon's own `DEFAULT_MODEL`
 * (`agent24-models::router::ModelRouter::from_env`), not a client-side guess.
 * `null`/missing/non-string all mean "the daemon has none to report" —
 * exported for its unit test. */
export function extractDaemonDefaultModel(res: { ok: boolean; data: unknown }): string | null {
  if (!res.ok) return null
  const data = res.data as { default_model?: unknown } | null
  const dm = data?.default_model
  return typeof dm === 'string' && dm.trim() !== '' ? dm : null
}

export function App(): JSX.Element {
  const [page, setPage] = useState<Page>('chat')
  const [backendOk, setBackendOk] = useState<boolean | null>(null)
  const [version, setVersion] = useState<string>('')
  const [sidebarOpen, setSidebarOpen] = useState(true)
  const [darkMode, setDarkMode] = useState(true)
  const [modules, setModules] = useState<ModuleInfo[]>([])
  // Renamed from `llmLabel`: this is now only the oMLX-derived FALLBACK,
  // used when the daemon has no `default_model` to report. The topbar's
  // actual displayed label is the `llmLabel` derived value below.
  const [fallbackLlmLabel, setFallbackLlmLabel] = useState('Detecting…')
  const [daemonDefaultModel, setDaemonDefaultModel] = useState<string | null>(null)
  const [backendPort, setBackendPort] = useState<number | null>(null)
  const initDone = useRef(false)

  // ME4-desktop-model-ui: the daemon's OWN default model always wins over a
  // client-side guess from a locally-detected oMLX model list — that guess
  // is what let an OCR/VL model reach the topbar in the first place (see the
  // NON_CHAT_MODEL_PATTERNS comment above). Falls back to the oMLX-derived
  // label only when the daemon has none to report (e.g. an older daemon
  // build, or the router was never built via `from_env`).
  const llmLabel = daemonDefaultModel ? `${daemonDefaultModel} · Agent24 默认` : fallbackLlmLabel

  useEffect(() => {
    if (initDone.current) return
    initDone.current = true

    void window.agent24.getAppVersion().then(setVersion)

    // Backend health + module list polling
    const checkBackend = () => {
      window.agent24.backendProxy({ method: 'GET', path: '/api/v1/health' })
        .then((res) => {
          setBackendOk(res.ok)
          if (res.ok) {
            void window.agent24.modulesList().then(setModules)
            // FU-93: show the daemon's real port, not a hardcoded guess.
            void window.agent24.backendEndpoint().then((e) => setBackendPort(e?.port ?? null))
            // ME4-desktop-model-ui: ask the daemon what its OWN default
            // model is, rather than only ever guessing from a locally
            // detected oMLX list (see `llmLabel` above).
            void window.agent24.backendProxy({ method: 'GET', path: '/api/v1/models' })
              .then((r) => {
                const dm = extractDaemonDefaultModel(r)
                if (dm) setDaemonDefaultModel(dm)
              })
              .catch(() => { /* fall back to the oMLX-derived label */ })
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
        const chatModel = pickChatModel(detected.models)
        setFallbackLlmLabel(chatModel ? `${chatModel} · oMLX` : NO_CHAT_MODEL_LABEL)
        return
      }
      // Try to start oMLX
      const started = await window.agent24.omlxStart(8088, 'xiaobao8088')
      if (started.ok) {
        // Poll until ready
        for (let i = 0; i < 5; i++) {
          await new Promise((r) => setTimeout(r, 2000))
          const r = await window.agent24.omlxModels(started.url, 'xiaobao8088')
          if (r.ok && r.models.length > 0) {
            const chatModel = pickChatModel(r.models)
            setFallbackLlmLabel(chatModel ? `${chatModel} · oMLX` : NO_CHAT_MODEL_LABEL)
            if (chatModel) break
          }
        }
      } else {
        setFallbackLlmLabel('No AI runtime')
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
          <img className="sidebar-logo-img" src={logoSidebar} alt="" />
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
            {backendOk === true && (
              <span>后端服务运行中{backendPort !== null ? ` :${backendPort}` : ''}</span>
            )}
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

        {/* Review M3: the chat suffix no longer takes a default-model prop —
            it shows ONLY the server's own reported model_id (chat-latency.ts),
            never the topbar's daemon-default guess standing in for it. */}
        {page === 'chat'             && <ChatPage />}
        {page === 'workbench'        && <WorkbenchPage />}
        {page === 'runs'             && <RunsPage />}
        {page === 'schedules'        && <SchedulesPage />}
        {page === 'approvals'        && <ApprovalsPage />}
        {page === 'voice'            && <VoicePanel />}
        {page === 'creative'         && <CreativePage />}
        {page === 'models'           && <ModelsPage />}
        {page === 'modules-manager'  && <ModulesManagerPage />}
        {page === 'memory'           && <MemoryPage />}
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
