import { useContext, useEffect, useRef, useState } from 'react'
import type { Agent24API } from '../main/preload'
import type { DaemonStartupFailure } from '../shared/ipc-types'

declare global {
  interface Window { agent24: Agent24API }
}
import ChatPage from './pages/Chat'
import { ProactiveBanner } from './pages/chat/ProactiveBanner'
import { useSeeingOpenConversation } from './pages/chat/proactive'
import HomePage from './pages/Home'
import ModelsPage from './pages/Models'
import SettingsPage from './pages/Settings'
import { listOsModules } from './pages/agent/api'
import SchedulesPage from './pages/Schedules'
import CommunicationPage from './pages/comm/Communication'
import VoicePanel from './pages/voice/VoicePanel'
import CreativePage from './pages/Creative'
import KnowledgePage from './pages/Knowledge'
import MemoryPage from './pages/Memory'
import DocumentsPage from './pages/documents/Documents'
import ModulesManagerPage from './pages/ModulesManager'
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
import { isChatModelId } from './pages/settings/chatModels'

/** First model id that looks like a chat model, or `undefined` if every
 * candidate is filtered out. The rule lives in `chatModels.ts`. */
export function pickChatModel(models: string[]): string | undefined {
  return models.find((m) => isChatModelId(m))
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
/** A database newer than this binary (migration applied, then missing here). */
export function daemonStartupHint(failure: { error: string } | null | undefined): 'migration_newer' | null {
  if (!failure) return null
  const error = failure.error
  if (/migration/i.test(error) && /previously applied/i.test(error) && /missing/i.test(error)) return 'migration_newer'
  return null
}

export function extractDaemonDefaultModel(res: { ok: boolean; data: unknown }): string | null {
  if (!res.ok) return null
  const data = res.data as { default_model?: unknown } | null
  const dm = data?.default_model
  return typeof dm === 'string' && dm.trim() !== '' ? dm : null
}

function BackendOffline({ failure, reasonOpen, onToggleReason, onRetry }: {
  failure: DaemonStartupFailure | null
  reasonOpen: boolean
  onToggleReason: () => void
  onRetry: () => void
}): JSX.Element {
  const t = useT(appMessages)
  const hint = daemonStartupHint(failure)
  const raw = failure ? (failure.error || failure.code) : ''
  return (
    <>
      <span title={failure ? raw : undefined}>{failure ? t('backend.startFailed') : t('backend.offline')}</span>
      {failure && (
        <span className="backend-failure">
          {hint === 'migration_newer' && <span className="backend-failure-hint" title={raw}>{t('backend.migrationNewer')}</span>}
          <button type="button" className="backend-failure-toggle" aria-expanded={reasonOpen} title={raw} onClick={onToggleReason}>
            {t('backend.reason')}
          </button>
          {reasonOpen && <span className="backend-failure-raw">{raw}</span>}
        </span>
      )}
      <button type="button" className="backend-retry" onClick={onRetry}>{t('backend.retry')}</button>
    </>
  )
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
  // M6 C2: the open conversation is seen while the main chat shows and the
  // window has the focus (docs/ui/ASSISTANT-C.md §7).
  useSeeingOpenConversation(page === 'assistant')
  useEffect(() => window.agent24.onCliToolSettings?.(() => navigate('settings')), [navigate])
  useEffect(() => {
    if (!prefs.showOverview && route.kind === 'page' && route.page === 'home') open('assistant', 'other')
  }, [prefs.showOverview, route, open])
  const [backendOk, setBackendOk] = useState<boolean | null>(null)
  const [startupFailure, setStartupFailure] = useState<DaemonStartupFailure | null>(null)
  const [reasonOpen, setReasonOpen] = useState(false)
  // QA 0.6.1 §3c: the service daemon's refusal, said instead of plain offline.
  const [refusal, setRefusal] = useState<{ code: string; permanent: boolean } | null>(null)
  const [version, setVersion] = useState<string>('')
  const [sidebarOpen, setSidebarOpen] = useState(true)
  const [darkMode, setDarkMode] = useState(true)
  // Domain-OS modules actually mounted (state==='mounted') in the daemon (`GET /api/v1/os`) — the Home card.
  const [osModuleCount, setOsModuleCount] = useState(0)
  // Renamed from `llmLabel`: this is now only the oMLX-derived FALLBACK,
  // used when the daemon has no `default_model` to report. The topbar's
  // actual displayed label is the `llmLabel` derived value below.
  const [fallbackLlmLabel, setFallbackLlmLabel] = useState(LLM_DETECTING)
  const [daemonDefaultModel, setDaemonDefaultModel] = useState<string | null>(null)
  const [backendPort, setBackendPort] = useState<number | null>(null)
  const initDone = useRef(false)

  // ME4-desktop-model-ui: the daemon's OWN default model always wins over a
  // client-side guess from a locally-detected oMLX model list — that guess
  // is what let an OCR/VL model reach the topbar in the first place (see
  // `isChatModelId`). Falls back to the oMLX-derived
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

    // Backend health + domain-OS module count polling
    const readStartupFailure = () => {
      void window.agent24.backendEndpoint()
        .then((e) => setStartupFailure(e?.startupFailure ?? null))
        .catch(() => { /* keep the last reason */ })
    }
    const checkBackend = () => {
      window.agent24.backendProxy({ method: 'GET', path: '/api/v1/health' })
        .then((res) => {
          setBackendOk(res.ok)
          if (res.ok) setRefusal(null)
          else void window.agent24.backendRefusal?.().then(setRefusal, () => setRefusal(null))
          if (res.ok) {
            setStartupFailure(null)
            void listOsModules()
              .then((l) => setOsModuleCount(Array.isArray(l.modules) ? l.modules.filter((m) => m.state === 'mounted').length : 0))
              .catch(() => { /* keep the last count */ })
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
          } else {
            readStartupFailure()
          }
        })
        .catch(() => { setBackendOk(false); readStartupFailure() })
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
      : page === 'home' ? t('title.home')
        : page === 'settings' ? t('title.settings') : t('nav.capabilities')
  const activeId = selected?.id ?? page ?? ''

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
            {backendOk === false && !refusal && (
              <BackendOffline failure={startupFailure} reasonOpen={reasonOpen} onToggleReason={() => setReasonOpen((open) => !open)}
                onRetry={() => { void window.agent24.retryBackend() }} />
            )}
            {backendOk === false && refusal && (
              <span>{t(refusal.permanent ? 'backend.refused' : 'backend.enrolRetrying', { code: refusal.code })}</span>
            )}
            {backendOk === null && <span>{t('backend.checking')}</span>}
          </div>
          {backendOk === false && refusal?.permanent && <p className="backend-refused-hint">{t('backend.refusedHint')}</p>}
        </div>
      </aside>

      {/* ── Main ── */}
      <div className={`main${sidebarOpen ? '' : ' sidebar-hidden'}`}>
        <div className="topbar">
          <span className="topbar-title">{pageTitle}</span>
          {(page === 'assistant' || page === 'home') && (
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
            page === 'assistant'
              ? { display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0 }
              : { display: 'none' }
          }
        >
          {/* M6 C2: "The assistant has something to say (n)". */}
          <ProactiveBanner />
          <ChatPage />
        </div>
        {page === 'home' && (
          <HomePage
            backendOk={backendOk}
            backendPort={backendPort}
            moduleCount={osModuleCount}
            onNavigate={(target) => {
              const def = defs.find(d => d.page === target || d.id === target)
              if (def) open(def.id, 'other')
              // M4 A6: the run store's station opens Settings → Activity.
              else if (target.startsWith('settings/')) open(target, 'other')
              else navigate(target)
            }}
          />
        )}
        {page === 'documents'        && <DocumentsPage />}
        {page === 'schedules'        && <SchedulesPage />}
        {page === 'communication'    && <CommunicationPage />}
        {page === 'voice'            && <VoicePanel />}
        {page === 'creative'         && <CreativePage />}
        {page === 'knowledgeBase'    && <KnowledgePage />}
        {page === 'models'           && <ModelsPage />}
        {page === 'memory'           && <MemoryPage />}
        {page === 'settings'         && <SettingsPage key={route.kind === 'page' ? route.key : undefined} initialTab={route.kind === 'page' ? route.initialTab : undefined} />}
        {page === 'capabilities'     && <CapabilitiesPage statusFor={statusFor} grantsFor={statusFor.grantsFor} isolationFor={statusFor.isolationFor} onOpenSettings={() => open('settings/general', 'capabilities', { focus: 'slots' })} />}
        {page === 'modules-manager'  && <ModulesManagerPage />}
        {selected && !['assistant', 'documents', 'schedules', 'communication', 'voice', 'creative', 'knowledgeBase', 'models', 'memory', 'modules-manager'].includes(selected.page)
          && <CapabilityPage capability={selected} />}
      </div>
    </div>
  )
}
