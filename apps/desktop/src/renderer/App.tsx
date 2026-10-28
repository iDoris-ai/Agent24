import { useContext, useEffect, useRef, useState } from 'react'
import type { Agent24API } from '../main/preload'

declare global {
  interface Window { agent24: Agent24API }
}
import ChatPage from './pages/Chat'
import HomePage from './pages/Home'
import ModelsPage from './pages/Models'
import SettingsPage from './pages/Settings'
import RunsPage from './pages/Runs'
import SchedulesPage from './pages/Schedules'
import CommunicationPage from './pages/comm/Communication'
import ApprovalsPage from './pages/Approvals'
import VoicePanel from './pages/voice/VoicePanel'
import CreativePage from './pages/Creative'
import MemoryPage from './pages/Memory'
import DocumentsPage from './pages/documents/Documents'
import ModulesManagerPage from './pages/ModulesManager'
import HelloModule from './pages/modules/HelloModule'
import { Sidebar } from './capabilities/Sidebar'
import { Breadcrumb } from './capabilities/Breadcrumb'
import { CapabilityPage } from './capabilities/CapabilityPage'
import CapabilitiesPage from './pages/Capabilities'
import { useCapabilityStatus } from './capabilities/useCapabilityStatus'
import { CapabilitiesProvider, NavPrefsProvider, useCapabilityRoute } from './capabilities/providers'
import { CapabilitiesContext, NavPrefsContext } from './capabilities/types'
import { capabilitiesMessages } from './capabilities/capabilities.i18n'
import logoSidebar from './assets/logo-sidebar.png'
import { useT } from './i18n'
import { appMessages } from './App.i18n'
import { QuickSwitch } from './components/QuickSwitch'

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

// Sentinels for the oMLX-derived fallback label; translated at render time so
// switching language relabels them without re-detecting.
const LLM_DETECTING = '\u0000detecting'
const LLM_NO_RUNTIME = '\u0000no-runtime'
const NO_CHAT_MODEL_LABEL = '\u0000no-chat-model'
const LLM_NEEDS_KEY = '\u0000needs-key'

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
  return <CapabilitiesProvider><NavPrefsProvider><AppShell /></NavPrefsProvider></CapabilitiesProvider>
}

function AppShell(): JSX.Element {
  const { route, navigate } = useCapabilityRoute()
  const { defs, open } = useContext(CapabilitiesContext)
  const { prefs } = useContext(NavPrefsContext)
  const selected = route.kind === 'capability' ? defs.find(d => d.id === route.id) : undefined
  const page = route.kind === 'page' ? route.page : selected?.page
  const c = useT(capabilitiesMessages)
  const statusFor = useCapabilityStatus(page === 'capabilities')
  useEffect(() => window.agent24.onCliToolSettings?.(() => navigate('settings')), [navigate])
  useEffect(() => {
    if (!prefs.showOverview && route.kind === 'page' && route.page === 'home') open('assistant', 'other')
  }, [prefs.showOverview, route, open])
  const [backendOk, setBackendOk] = useState<boolean | null>(null)
  const [version, setVersion] = useState<string>('')
  const [sidebarOpen, setSidebarOpen] = useState(true)
  const [darkMode, setDarkMode] = useState(true)
  // Renamed from `llmLabel`: this is now only the oMLX-derived FALLBACK,
  // used when the daemon has no `default_model` to report. The topbar's
  // actual displayed label is the `llmLabel` derived value below.
  const [fallbackLlmLabel, setFallbackLlmLabel] = useState(LLM_DETECTING)
  const [daemonDefaultModel, setDaemonDefaultModel] = useState<string | null>(null)
  const [backendPort, setBackendPort] = useState<number | null>(null)
  const initDone = useRef(false)

  // ME4-desktop-model-ui: the daemon's OWN default model always wins over a
  // client-side guess from a locally-detected oMLX model list — that guess
  // is what let an OCR/VL model reach the topbar in the first place (see the
  // NON_CHAT_MODEL_PATTERNS comment above). Falls back to the oMLX-derived
  // label only when the daemon has none to report (e.g. an older daemon
  // build, or the router was never built via `from_env`).
  const t = useT(appMessages)
  const FALLBACK_KEYS = { [LLM_DETECTING]: 'llm.detecting', [LLM_NO_RUNTIME]: 'llm.noRuntime', [NO_CHAT_MODEL_LABEL]: 'llm.noChatModel', [LLM_NEEDS_KEY]: 'llm.needsKey' } as const
  const fallbackKey = FALLBACK_KEYS[fallbackLlmLabel as keyof typeof FALLBACK_KEYS]
  const llmLabel = daemonDefaultModel
    ? t('llm.daemonDefault', { model: daemonDefaultModel })
    : fallbackKey ? t(fallbackKey) : fallbackLlmLabel

  useEffect(() => {
    if (initDone.current) return
    initDone.current = true

    void window.agent24.getAppVersion().then(setVersion)

    // Backend health polling
    const checkBackend = () => {
      window.agent24.backendProxy({ method: 'GET', path: '/api/v1/health' })
        .then((res) => {
          setBackendOk(res.ok)
          if (res.ok) {
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
      if (detected && 'authRequired' in detected) {
        // oMLX is running but wants an API key: starting another one would
        // only collide with it. Say so; Settings has the key field.
        setFallbackLlmLabel(LLM_NEEDS_KEY)
        return
      }
      if (detected) {
        const chatModel = pickChatModel(detected.models)
        setFallbackLlmLabel(chatModel ? `${chatModel} · oMLX` : NO_CHAT_MODEL_LABEL)
        return
      }
      // Try to start oMLX
      const started = await window.agent24.omlxStart(8088, '')
      if (started.ok) {
        // Poll until ready
        for (let i = 0; i < 5; i++) {
          await new Promise((r) => setTimeout(r, 2000))
          const r = await window.agent24.omlxModels(started.url, '')
          if (r.ok && r.models.length > 0) {
            const chatModel = pickChatModel(r.models)
            setFallbackLlmLabel(chatModel ? `${chatModel} · oMLX` : NO_CHAT_MODEL_LABEL)
            if (chatModel) break
          }
        }
      } else {
        setFallbackLlmLabel(LLM_NO_RUNTIME)
      }
    })()

    return () => clearInterval(backendTimer)
  }, [])

  const pageTitle = selected ? c(selected.nameKey)
    : page === 'capabilities' ? t('nav.capabilities')
      : page === 'chat' ? c('assistant.name')
        : page === 'home' ? t('title.home')
          : page === 'settings' ? t('title.settings')
            : page === 'runs' ? t('title.runs') : t('nav.capabilities')
  const activeId = selected?.id ?? (page === 'chat' ? 'assistant' : page ?? '')

  return (
    <div className={`app${darkMode ? '' : ' light'}`}>
      <QuickSwitch />
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

        <Sidebar activeId={activeId} navigate={navigate} />

        <div className="sidebar-footer">
          <div className="backend-status">
            <div className={`status-dot ${backendOk === true ? 'online' : backendOk === false ? 'offline' : ''}`} />
            {backendOk === true && (
              <span>{t('backend.online')}{backendPort !== null ? ` :${backendPort}` : ''}</span>
            )}
            {backendOk === false && <span>{t('backend.offline')}</span>}
            {backendOk === null && <span>{t('backend.checking')}</span>}
          </div>
        </div>
      </aside>

      {/* ── Main ── */}
      <div className={`main${sidebarOpen ? '' : ' sidebar-hidden'}`}>
        <div className="topbar">
          <span className="topbar-title">{pageTitle}</span>
          {(page === 'chat' || page === 'home') && (
            <span className="topbar-sub">{llmLabel}</span>
          )}
          <button
            className="theme-toggle-btn"
            onClick={() => setDarkMode(d => !d)}
            title={darkMode ? t('theme.toLight') : t('theme.toDark')}
          >
            {darkMode ? '🌙' : '☀️'}
          </button>
        </div>

        {selected && route.kind === 'capability' && <Breadcrumb capability={selected} origin={route.origin} onBack={() => navigate('capabilities')} />}

        {/* Review M3: the chat suffix no longer takes a default-model prop —
            it shows ONLY the server's own reported model_id (chat-latency.ts),
            never the topbar's daemon-default guess standing in for it.
            M1-T14: ChatPage stays MOUNTED for the app's whole lifetime — only
            hidden via `display: none` — so navigating away to another page
            and back never unmounts it. Unmounting would drop its message
            list, input draft and per-conversation session_id (see Chat.tsx's
            `useState(() => genChatSessionId())`), silently starting a new D1
            memory session on every round trip through another page. */}
        <div
          style={
            page === 'chat'
              ? { display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0 }
              : { display: 'none' }
          }
        >
          <ChatPage />
        </div>
        {page === 'home' && (
          <HomePage
            backendOk={backendOk}
            backendPort={backendPort}
            moduleCount={defs.filter(d => d.page.startsWith('module:')).length}
            onNavigate={(target) => {
              const def = defs.find(d => d.page === target || d.id === target)
              if (def) open(def.id, 'other')
              else navigate(target)
            }}
          />
        )}
        {page === 'documents'        && <DocumentsPage />}
        {page === 'runs'             && <RunsPage />}
        {page === 'schedules'        && <SchedulesPage />}
        {page === 'communication'    && <CommunicationPage />}
        {page === 'approvals'        && <ApprovalsPage />}
        {page === 'voice'            && <VoicePanel />}
        {page === 'creative'         && <CreativePage />}
        {page === 'models'           && <ModelsPage />}
        {page === 'memory'           && <MemoryPage />}
        {page === 'settings'         && <SettingsPage initialTab={route.kind === 'page' ? route.initialTab : undefined} />}
        {page === 'capabilities'     && <CapabilitiesPage statusFor={statusFor} onOpenSettings={() => open('settings/general', 'capabilities')} />}
        {page === 'modules-manager'  && <ModulesManagerPage />}
        {page === 'module:hello'     && <HelloModule />}
        {selected && !['chat', 'documents', 'schedules', 'communication', 'approvals', 'voice', 'creative', 'models', 'memory', 'modules-manager', 'module:hello'].includes(selected.page)
          && <CapabilityPage capability={selected} />}
      </div>
    </div>
  )
}
