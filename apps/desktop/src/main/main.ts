// Agent24 main process entry — M2: integrates BackendManager daemon.

import { app, BrowserWindow, dialog, Menu, Tray, WebContentsView, nativeImage, net, safeStorage, session, ipcMain, shell, type MenuItemConstructorOptions } from 'electron'
import path from 'node:path'
import { registerIpcHandlers } from './ipc/index'
import { onChatHistoryChange } from './chat-history'
import { createScheduleConversations, type ScheduleConversations } from './schedule-conversations'
import { createAssistantApprovals, type AssistantApprovals, type RunOwner } from './assistant-approvals'
import { registerCliTool } from './ipc/cli-tool'
import { getMainLocale, initMainLocale, mt, onMainLocaleChange } from './main-i18n'
import { buildTrayView, type TrayAction } from './tray-menu'
import { createAppUrlPolicy, createIpcGuard, installNavigationGuards } from './ipc/guard'
import { BackendManager, daemonAuthMode, daemonConnection, launchAgentPlistExists, onDaemonReady, readExternalDaemonState, resolveBundledRustBinary, resolveRustBinary, setBundledPackaged } from './backend-manager'
import { bundledDaemonVersion, consentPath, CrashLoopDetector, installService, readConsent, runConsent, serviceInstallDeps, serviceRecordPid, serviceRuns, ServiceUpgrader, serviceUpgradeDeps, shouldAskConsent, upgradeService, upgradesFromBundle, writeConsent } from './resident-service'
import { PresencePoller, defaultPresenceDeps, presenceDialogOptions, showPresenceDialog } from './presence-poller'
import { PresenceConnection } from './presence-connection'
import { AgentEarEventBridge } from './agentear-events'
import { documentsEvent } from './documents'
import { cliBinary } from './documents-cli'
import {
  CODESIGN,
  type BundledModuleDeps,
  documentsModuleOnDisk,
  ensureBundledDocumentsModule,
  execCli,
  installBundledDocumentsModule,
  packagesRootFor,
  type ModuleActionResult,
  uninstallDocumentsModule,
} from './documents-module'
import { tryDataDir } from './data-dir'
import { AgentEarEventLog } from './agentear-log'
import { ModelCallLog } from './model-call-log'
import { IpcChannels } from '../shared/ipc-types'
import type { CreativeViewBounds, CreativeViewResult } from '../shared/ipc-types'
import {
  CREATIVE_SESSION_PARTITION,
  OPEN_DESIGN_PIN_VERSION,
  CreativeServeWeb,
  CreativeViewRequestFence,
  classifyCreativeUrl,
  loadCreativeView,
  resolveNodeRuntimeExecutable,
} from './creative-serve-web'
import { createCreativeShutdown } from './creative-quit'
import { OpenDesignComponentInstaller } from './open-design-component'
import { OpenDesignStatusReporter, mapOpenDesignReport, postOpenDesignReport } from './open-design-report'
import { KnowledgeHost } from './knowledge-host'
import { KnowledgeLoginStore } from './knowledge-logins'
import { KNOWLEDGE_BINDING, KnowledgeController, KnowledgeSettingsStore } from './knowledge-settings'
import { knowledgeAppInstanceId } from './knowledge-app-instance'
import { bindingTag, KnowledgeUiClient } from './knowledge-ui-client'
import { KnowledgeUiInstance } from './knowledge-ui-instance'
import { KNOWLEDGE_UI_ROUTES } from './knowledge-ui-routes'
// KB-C2-03: the KnowledgeView lives in its own module; the lines that wire it
// in below are additions only and leave the Creative view's paths untouched.
import { KnowledgeViewHost, registerKnowledgeViewIpc, resolveKnowledgeViewTarget } from './knowledge-view'
// KB-SOP-01a (#1669): the knowledge entry admission, additions only.
import { createChatHistoryStore } from './chat-history'
import { daemonFloorReader } from './knowledge-entry-floor'
import { registerKnowledgeEntryIpc } from './knowledge-entry-ipc'
import { createEntryDraftStore } from './knowledge-entry-draft'
import { entryTarget } from './knowledge-entry-extract'
import { CHAT_TIMEOUT_MS, proxyError, proxyRequest } from './backend-proxy'

const isDev = process.env.NODE_ENV === 'development'
setBundledPackaged(app.isPackaged)
const backendManager = new BackendManager({ isPackaged: app.isPackaged })
// M5 B5: the service daemon brought up to the agent24d this app ships.
let bundledVersion: string | null = null
if (upgradesFromBundle(app.isPackaged, process.env)) void bundledDaemonVersion(resolveRustBinary(app.isPackaged)).then((v) => { bundledVersion = v })
const serviceUpgrader = new ServiceUpgrader({
  bundled: () => bundledVersion,
  running: () => backendManager.serviceDaemonVersion(),
  installed: launchAgentPlistExists,
  pid: () => (backendManager.presenceTarget() ? backendManager.externalDaemonPid() ?? null : null),
  run: async (pid) => {
    const dataDir = tryDataDir()
    if (dataDir === null) return { ok: false, stage: 'copy', code: 'no_data_dir' }
    const hold = <T>(work: () => Promise<T>) => backendManager.whileServiceRestarts(work)
    return upgradeService(serviceUpgradeDeps({ bin: resolveRustBinary(app.isPackaged), dataDir, pid, hold }))
  },
})
const crashLoop = new CrashLoopDetector({
  now: () => Date.now(),
  installed: () => process.platform === 'darwin' && launchAgentPlistExists(),
  enrolled: () => backendManager.presenceTarget() !== null,
  recordPid: () => serviceRecordPid(readExternalDaemonState(), (pid) => backendManager.spawned(pid)),
  runs: () => serviceRuns(process.getuid!()),
})
// A3-4 review M5: the sequenced/capped event log lives here, in main, so it
// survives the renderer's VoicePanel unmounting when the user switches
// sidebar pages (design §7.2's intent — a host-side log, not a per-tab one).
const agentEarLog = new AgentEarEventLog()
// ME4-desktop-model-ui: same "log lives in main, survives the panel
// unmounting" reasoning as agentEarLog above, for the kernel's `model.call`
// WS events instead of AgentEar's own transcript.
const modelCallLog = new ModelCallLog()
// Forwards raw WS-unwrapped agentear.event/1 objects into the log; the log's
// own subscribe() (wired below, in whenReady) fans newly-delivered envelopes
// out to the renderer. The second callback does the same for `model.call`
// events scoped to AgentEar (agentear-events.ts's `parseModelCallFrame`).
const agentEarBridge = new AgentEarEventBridge(
  (envelope) => agentEarLog.ingest(envelope),
  (call) => modelCallLog.ingest(call),
  // The Documenting OS's events go to the renderer as they come (ADR-DOC-01
  // D5); hints only, so nothing is kept for a window that is not there.
  (event) => {
    const doc = documentsEvent(event)
    if (doc && mainWin && !mainWin.isDestroyed()) mainWin.webContents.send(IpcChannels.DocumentsEvent, doc)
  },
  // M4 A3: runs' changes, for the history list's filters.
  (event) => {
    if (mainWin && !mainWin.isDestroyed()) mainWin.webContents.send(IpcChannels.RunEvent, event)
    // M4 A6: an approval of a run no conversation has, to the assistant's.
    if (event.type === 'approval.required') {
      assistantApprovals().route(event.run_id)
        .catch((err) => console.error('[main] approval run not routed', event.run_id, err))
    }
  },
  // M4 A5: a schedule's run goes to the schedule's conversation, window or not.
  (run) => {
    scheduleConversations().record(run.schedule_id, run.run_id)
      .catch((err) => console.error('[main] schedule run not recorded', run.schedule_id, run.run_id, err))
  },
  // ...and those started while main was not listening.
  () => {
    scheduleConversations().catchUp(async () => {
      const conn = daemonConnection()
      if (!conn) throw new Error('backend_not_ready')
      // The daemon's list has every run, with its input and output, and no
      // paging: main's own request takes more than the window's 8 MB.
      const res = await proxyRequest(conn, { method: 'GET', path: '/api/v1/runs' }, { host: '127.0.0.1', maxBytes: RUNS_CATCH_UP_MAX_BYTES })
      const list = (res.data as { runs?: unknown } | null)?.runs
      if (!res.ok || !Array.isArray(list)) throw new Error(`runs_unavailable_${res.status}`)
      return list
    }).catch((err) => console.error('[main] schedule runs not caught up', err))
    assistantApprovals().catchUp(async () => {
      const res = await daemonGet('/api/v1/approvals?status=pending')
      const list = (res.data as { approvals?: unknown } | null)?.approvals
      if (!res.ok || !Array.isArray(list)) throw new Error(`approvals_unavailable_${res.status}`)
      return list
    }).catch((err) => console.error('[main] pending approvals not caught up', err))
  },
)

// M4 A6: made on the first approval, once userData is settled.
async function daemonGet(apiPath: string) {
  const conn = daemonConnection()
  if (!conn) throw new Error('backend_not_ready')
  return proxyRequest(conn, { method: 'GET', path: apiPath }, { host: '127.0.0.1' })
}
let assistantApprovalsStore: AssistantApprovals | null = null
function assistantApprovals(): AssistantApprovals {
  assistantApprovalsStore ??= createAssistantApprovals({
    dir: app.getPath('userData'),
    history: createChatHistoryStore(app.getPath('userData')),
    ownerOf: async (runId): Promise<RunOwner> => {
      const run = await daemonGet(`/api/v1/runs/${encodeURIComponent(runId)}`)
      const data = run.data as { schedule_id?: unknown; session_id?: unknown } | null
      if (!run.ok || data === null) throw new Error(`run_unavailable_${run.status}`)
      if (typeof data.schedule_id === 'string' && data.schedule_id !== '') {
        return (await scheduleConversations().claim(data.schedule_id, runId)) ? 'schedule' : 'other'
      }
      if (typeof data.session_id !== 'string' || data.session_id === '') return 'other'
      const session = await daemonGet(`/api/v1/sessions/${encodeURIComponent(data.session_id)}`)
      // A session the daemon no longer has is no window's.
      if (session.status === 404) return 'other'
      const channel = (session.data as { channel?: unknown } | null)?.channel
      if (!session.ok || typeof channel !== 'string') throw new Error(`session_unavailable_${session.status}`)
      return channel === 'desktop' ? 'desktop' : 'other'
    },
    settled: (runId, recorded) => scheduleConversations().settle(runId, recorded),
    title: () => mt('assistant.conversationTitle'),
    onCatchUpFailure: (runId, err) => console.error('[main] approval run not caught up', runId, err),
  })
  return assistantApprovalsStore
}

// M4 A5: made on the first schedule's run, once userData is settled.
const RUNS_CATCH_UP_MAX_BYTES = 64 * 1024 * 1024
let scheduleConversationsStore: ScheduleConversations | null = null
function scheduleConversations(): ScheduleConversations {
  scheduleConversationsStore ??= createScheduleConversations({
    dir: app.getPath('userData'),
    history: createChatHistoryStore(app.getPath('userData')),
    scheduleName: async (scheduleId) => {
      const conn = daemonConnection()
      if (!conn) return null
      const res = await proxyRequest(conn, { method: 'GET', path: `/api/v1/schedules/${encodeURIComponent(scheduleId)}` }, { host: '127.0.0.1' })
      const name = (res.data as { name?: unknown } | null)?.name
      return res.ok && typeof name === 'string' ? name : null
    },
    title: (name) => mt('schedule.conversationTitle', { name }),
    onCatchUpFailure: (runId, err) => console.error('[main] schedule run not caught up', runId, err),
  })
  return scheduleConversationsStore
}

// FU-91: was hardcoded to 'http://localhost:5173' — if that port were taken
// by another project, the window silently loaded whatever else was listening
// there. `vite.config.ts` sets `strictPort: true` so `pnpm dev:vite` now
// fails loudly instead of picking a different port; this env var lets the
// main process agree with a non-default vite dev server URL when one is
// configured, while still defaulting to the same 5173 vite.config.ts uses.
const DEV_SERVER_URL = process.env.VITE_DEV_SERVER_URL ?? 'http://localhost:5173'
const DEV_SERVER_ORIGIN = new URL(DEV_SERVER_URL).origin
const DEV_SERVER_WS_ORIGIN = DEV_SERVER_ORIGIN.replace(/^http/, 'ws')

// A3: what counts as "the app's own page" for IPC sender checks and the main
// window's navigation guard — the vite dev server in dev, exactly the bundled
// renderer index.html when packaged.
const isAppUrl = createAppUrlPolicy({
  ...(isDev ? { devServerUrl: DEV_SERVER_URL } : {}),
  rendererIndexPath: path.join(__dirname, '../renderer/index.html'),
})

// Keep tray reference alive — GC would destroy it otherwise
let tray: Tray | null = null
// Mutable reference so tray handlers always point to the current window
let mainWin: BrowserWindow | null = null
// A3: wraps every ipcMain.handle — the call must come from mainWin's main frame
// while it shows the app's own page. The Creative WebContentsView (a different
// WebContents, remote content) therefore can never reach these handlers.
const guard = createIpcGuard({ getMainWindow: () => mainWin, isTrustedUrl: isAppUrl })
// Set to true by before-quit so win.on('close') guard is skipped on app exit
let isQuitting = false
// F1b: periodic tray refresh so the menu-bar reflects live daemon status
let trayTimer: NodeJS.Timeout | null = null
let cliTool: ReturnType<typeof registerCliTool> | null = null
let cliCheckTicks = 0
let creativeServeWeb: CreativeServeWeb | null = null
// KB-HOST-01: Knowledge logins and the daemon's private Knowledge interface.
let knowledgeHost: KnowledgeHost | null = null
let knowledge: KnowledgeController | null = null
let knowledgeView: KnowledgeViewHost | null = null
// CORE-2 AgentEar S3: long-polls the daemon for native confirmation challenges.
let presencePoller: PresencePoller | null = null
// M5 B3: the same, over the presence connection, with the daemon the LaunchAgent runs.
let presenceConnection: PresenceConnection | null = null
let openDesignComponent: OpenDesignComponentInstaller | null = null
let openDesignReporter: OpenDesignStatusReporter | null = null
let stopOpenDesignReady: (() => void) | null = null
let creativeView: WebContentsView | null = null
let creativeOrigin: string | null = null
// The sidecar instance the view last loaded; a new instance forces a reload even on the same port.
let creativeLoadedInstanceId: string | null = null
const creativeViewFence = new CreativeViewRequestFence()

/** Resolves once the daemon has announced readiness (now, or within `timeoutMs`). */
function daemonReady(timeoutMs: number): Promise<void> {
  if (daemonConnection()) return Promise.resolve()
  return new Promise((resolve) => {
    const done = (): void => {
      clearTimeout(timer)
      off()
      resolve()
    }
    const timer = setTimeout(done, timeoutMs)
    const off = onDaemonReady(done)
  })
}

function normalizedBounds(bounds: CreativeViewBounds): Electron.Rectangle {
  return {
    x: Math.max(0, Math.round(bounds.x)),
    y: Math.max(0, Math.round(bounds.y)),
    width: Math.max(1, Math.round(bounds.width)),
    height: Math.max(1, Math.round(bounds.height)),
  }
}

function hideCreativeView(): void {
  creativeViewFence.invalidate()
  creativeOrigin = null
  if (!creativeView) return
  creativeView.setBounds({ x: 0, y: 0, width: 1, height: 1 })
}

async function showCreativeView(
  bounds: CreativeViewBounds,
  forceReload = false,
  requestGeneration = creativeViewFence.begin(),
): Promise<CreativeViewResult> {
  const win = mainWin
  if (!win || win.isDestroyed()) return { ok: false, error: 'Agent24 window is unavailable' }
  if (!creativeViewFence.isCurrent(requestGeneration)) return { ok: false, error: 'Creative view request superseded' }

  const service = creativeServeWeb
  if (!service) return { ok: false, error: 'Open Design host is unavailable' }
  const status = await service.start()
  if (!creativeViewFence.isCurrent(requestGeneration)) return { ok: false, error: 'Creative view request superseded' }
  if (status.state === 'needs-download') {
    return { ok: false, needsDownload: true, size: status.size }
  }
  if (status.state === 'failed' && status.notInstalled) {
    return { ok: false, notInstalled: true, error: status.error }
  }
  if (status.state !== 'ready' || !status.origin) {
    return { ok: false, error: status.error ?? 'Open Design did not become ready' }
  }
  creativeOrigin = status.origin

  if (!creativeView || creativeView.webContents.isDestroyed()) {
    creativeView = new WebContentsView({
      webPreferences: {
        contextIsolation: true,
        nodeIntegration: false,
        sandbox: true,
        partition: CREATIVE_SESSION_PARTITION,
      },
    })
    win.contentView.addChildView(creativeView)
    creativeView.webContents.setWindowOpenHandler(({ url }) => {
      const disposition = creativeOrigin ? classifyCreativeUrl(url, creativeOrigin) : 'blocked'
      // Preserve Open Design's existing desktop behavior: every HTTP(S)
      // target=_blank/window.open goes to the system browser, including
      // same-origin live-artifact preview URLs. Electron never gets a child
      // window for this surface.
      if (disposition === 'same-origin' || disposition === 'external-http') void shell.openExternal(url)
      return { action: 'deny' }
    })
    creativeView.webContents.on('will-navigate', (event, url) => {
      const disposition = creativeOrigin ? classifyCreativeUrl(url, creativeOrigin) : 'blocked'
      if (disposition === 'same-origin') return
      event.preventDefault()
      if (disposition === 'external-http') void shell.openExternal(url)
    })
    creativeView.webContents.on('will-redirect', (event, url, _isInPlace, isMainFrame) => {
      if (!isMainFrame) return
      const disposition = creativeOrigin ? classifyCreativeUrl(url, creativeOrigin) : 'blocked'
      if (disposition !== 'same-origin') event.preventDefault()
    })
  }

  creativeView.setBounds(normalizedBounds(bounds))
  const view = creativeView
  await loadCreativeView({
    forceReload,
    currentUrl: view.webContents.getURL(),
    origin: status.origin,
    loadedInstanceId: creativeLoadedInstanceId,
    instanceId: status.instanceId,
    loadURL: (url) => view.webContents.loadURL(url),
    isCurrent: () => creativeView === view && creativeViewFence.isCurrent(requestGeneration),
    rememberInstance: (instanceId) => { creativeLoadedInstanceId = instanceId },
  })
  return { ok: true, origin: status.origin }
}

async function restartCreativeView(bounds: CreativeViewBounds): Promise<CreativeViewResult> {
  const requestGeneration = creativeViewFence.begin()
  const service = creativeServeWeb
  if (!service) return { ok: false, error: 'Open Design host is unavailable' }
  await service.stop()
  creativeOrigin = null
  return showCreativeView(bounds, true, requestGeneration)
}

function createMainWindow(): BrowserWindow {
  const win = new BrowserWindow({
    width: 1280,
    height: 800,
    titleBarStyle: 'hiddenInset',
    show: false,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
      // sandbox disabled: preload uses require('../shared/ipc-types') which
      // Electron's sandboxed require blocks. Re-enable after bundling preload.
      sandbox: false,
    },
  })

  win.once('ready-to-show', () => win.show())

  // A3: the main window only ever shows the app itself. Navigation elsewhere is
  // blocked; http(s) targets open in the system browser, window.open is denied.
  installNavigationGuards(win.webContents, isAppUrl, (url) => {
    shell.openExternal(url).catch((err) => console.error('openExternal failed', err))
  })

  // While the tray is active and app is not quitting, intercept close → hide.
  // isQuitting is set in before-quit so Cmd+Q / app-menu Quit work normally.
  win.on('close', (e) => {
    if (tray && !isQuitting) {
      e.preventDefault()
      win.hide()
    }
  })

  if (isDev) {
    void win.loadURL(DEV_SERVER_URL)
    win.webContents.openDevTools({ mode: 'detach' })
  } else {
    void win.loadFile(path.join(__dirname, '../renderer/index.html'))
  }

  return win
}

// Safe show-or-recreate: always uses the current mainWin reference.
function showOrCreateWindow(): void {
  if (!mainWin || mainWin.isDestroyed()) {
    mainWin = createMainWindow()
  } else if (mainWin.isMinimized()) {
    mainWin.restore()
  } else {
    mainWin.show()
  }
  if (process.platform === 'darwin') app.focus()
}

process.on('uncaughtException', (err) => {
  console.error('[main] uncaughtException', err)
})
process.on('unhandledRejection', (reason) => {
  console.error('[main] unhandledRejection', reason)
})

// One desktop per user. Its daemon runs in capability mode, so a second copy
// could neither share that daemon nor start its own while the first holds the
// daemon's singleton lock; a second launch brings the first window forward.
const isPrimaryInstance = app.requestSingleInstanceLock()
if (!isPrimaryInstance) app.quit()
// Until whenReady below has created the first window (after the CSP hook and
// IPC handlers are in place), a second launch has nothing to bring forward and
// must not create a window of its own.
app.on('second-instance', () => {
  if (mainWin) showOrCreateWindow()
})

app.whenReady().then(async () => {
  if (!isPrimaryInstance) return
  // UI language for the tray and native dialogs: English unless the user chose
  // otherwise (the renderer reports its setting; the system locale is ignored).
  initMainLocale(app.getPath('userData'))
  onMainLocaleChange(() => refreshTray())
  // Owner decision 2026-10-05: Open Design is downloaded on demand (see
  // open-design-component.ts) instead of being bundled in the installer.
  // CreativeServeWeb only ever reads this accessor's current status — it
  // never triggers a download itself.
  //
  // PR #670 review (L1): the installer's own manifest loading already
  // degrades a corrupt baked manifest to `unavailable` instead of throwing,
  // but this constructor call is still wrapped — nothing here runs inside a
  // try/catch of its own caller (`app.whenReady().then()`), and an uncaught
  // throw at this point would have skipped registering every IPC handler
  // below it, not just the Open Design ones.
  try {
    openDesignComponent = new OpenDesignComponentInstaller({ resourcesPath: process.resourcesPath })
  } catch (error) {
    console.error('[main] failed to construct the Open Design component installer', error)
    openDesignComponent = null
  }
  creativeServeWeb = new CreativeServeWeb({
    resourcesPath: process.resourcesPath,
    stateRoot: path.join(app.getPath('userData'), 'creative', 'open-design'),
    runtimeExecutable: resolveNodeRuntimeExecutable(),
    pinVersion: OPEN_DESIGN_PIN_VERSION,
    component: openDesignComponent ?? undefined,
  })
  // ADR-038 C3b: main owns the ProductHost credential and reports Open Design.
  // The renderer never receives this client or the report route.
  openDesignReporter = new OpenDesignStatusReporter({
    snapshot: () => {
      const install = openDesignComponent?.status()
      return mapOpenDesignReport({
        installState: install?.state ?? 'unavailable',
        reason: install?.reason ?? install?.error,
        version: openDesignComponent?.reportedVersion() ?? null,
        sidecar: creativeServeWeb?.headlessPhaseForReport(install?.state ?? 'unavailable') ?? null,
      })
    },
    connection: daemonConnection,
    authMode: daemonAuthMode,
    send: postOpenDesignReport,
  })
  creativeServeWeb.onStatus(() => openDesignReporter?.notify())
  openDesignReporter.start()
  // Ready lands after start(), and a restarted daemon has not seen the last
  // payload. reportNow skips dedupe and ends a 403/404 suspension.
  stopOpenDesignReady = onDaemonReady(() => openDesignReporter?.reportNow())
  // Dev-only: show the real app icon in the dock immediately, without
  // waiting for an electron-builder packaged build (which is where mac.icon
  // in package.json normally takes effect).
  if (isDev && process.platform === 'darwin' && app.dock) {
    const devIcon = nativeImage.createFromPath(path.join(__dirname, '../../assets/icon.png'))
    if (!devIcon.isEmpty()) app.dock.setIcon(devIcon)
  }

  // DEP-A5: async (it probes daemon.json + does one health check before
  // deciding to reuse vs. spawn) — nothing here awaits it; tray/IPC callers
  // read `backendManager.status()` / `getBackendEndpoint()` live, so this
  // resolving a beat later than the old synchronous spawn is harmless.
  //
  // T1-1 (#1159 Q1 = A): the documents module the mac app carries is
  // installed first, so the daemon this start spawns mounts it: start() waits
  // for it, and tray actions in the meantime only record what the user wants.
  // Never rejects; a failure is a warning and the daemon starts all the same.
  const documentsDeps: BundledModuleDeps = {
    bundleDir: app.isPackaged && process.platform === 'darwin'
      ? path.join(process.resourcesPath, 'documents-module')
      : null,
    // Contents/Resources → the .app whose signature sets the rule.
    appBundle: app.isPackaged ? path.resolve(process.resourcesPath, '..', '..') : null,
    codesign: execCli(CODESIGN),
    packagesRoot: packagesRootFor(process.env, tryDataDir()),
    userDataDir: app.getPath('userData'),
    runCli: execCli(cliBinary()),
    warn: (message) => console.warn(message),
    info: (message) => console.log(message),
  }
  const documentsModuleReady = ensureBundledDocumentsModule(documentsDeps)
  backendManager.holdStartUntil(documentsModuleReady)
  const documentsModuleAction = async (run: () => Promise<ModuleActionResult>): Promise<ModuleActionResult> => {
    await documentsModuleReady
    const external = backendManager.isExternalDaemon()
    let result: ModuleActionResult
    try {
      result = await backendManager.whileStopped(run)
    } catch (error) {
      result = { ok: false, code: 'daemon_running', message: error instanceof Error ? error.message : String(error) }
    }
    if (!external && backendManager.status() !== 'stopped') await daemonReady(30_000)
    return result
  }
  void backendManager.start()
  // CORE-2 AgentEar S3: native confirmation for elevated operations. Idle until
  // the daemon runs in capabilities mode (main then holds the ProductHost
  // token); a daemon without the routes answers 404 and is re-checked rarely.
  const showBox = (opts: Parameters<typeof showPresenceDialog>[1], signal: AbortSignal) => {
    if (!mainWin || mainWin.isDestroyed()) mainWin = createMainWindow()
    return showPresenceDialog(mainWin, opts, signal, (parent, options) => dialog.showMessageBox(parent, options))
  }
  presencePoller = new PresencePoller(defaultPresenceDeps({
    // Enrolled with the service daemon, main holds no host token to poll as:
    // the presence connection below asks instead.
    connection: () => (backendManager.presenceTarget() ? null : daemonConnection()),
    showDialog: showBox,
  }))
  presencePoller.start()
  // M5 B3: idle until enrolled with the daemon the LaunchAgent runs.
  presenceConnection = new PresenceConnection({
    target: () => backendManager.presenceTarget(),
    ask: async (challenge, signal) => ((await showBox(presenceDialogOptions(challenge, mt), signal)).response === 0 ? 'allow' : 'deny'),
  })
  presenceConnection.start()
  // safeStorage needs the app ready. The logins load once, here, before
  // anything can change them; the reason is logged, never the contents.
  const loginStore = new KnowledgeLoginStore(app.getPath('userData'), safeStorage)
  knowledgeHost = new KnowledgeHost(loginStore)
  const knowledgeLogins = knowledgeHost.start()
  if (!knowledgeLogins.ok) console.warn(`[knowledge] logins unavailable: ${knowledgeLogins.reason}`)
  // The user's Knowledge settings drive the one binding; Off keeps it inactive.
  knowledge = new KnowledgeController(knowledgeHost, loginStore, new KnowledgeSettingsStore(app.getPath('userData')))
  void knowledge.apply()
  session.defaultSession.webRequest.onHeadersReceived((details, callback) => {
    callback({
      responseHeaders: {
        ...details.responseHeaders,
        'Content-Security-Policy': [
          "default-src 'self'; " +
          "script-src 'self'" + (isDev ? ` 'unsafe-inline' 'unsafe-eval' ${DEV_SERVER_ORIGIN}` : "") + "; " +
          "style-src 'self' 'unsafe-inline'; " +
          "img-src 'self' data:; " +
          "connect-src 'self'" + (isDev ? ` ${DEV_SERVER_ORIGIN} ${DEV_SERVER_WS_ORIGIN} http://localhost:8765` : "") + "; " +
          "font-src 'self'",
        ],
      },
    })
  })

  onChatHistoryChange(summary => { if (mainWin && !mainWin.isDestroyed()) mainWin.webContents.send(IpcChannels.ChatHistoryChanged, summary) })
  registerIpcHandlers({
    guard,
    knowledge: () => knowledge,
    scheduleConversation: (scheduleId) => scheduleConversations().conversationOf(scheduleId),
    // T1-1/T1-2: what is on disk now. A daemon reads installed packages only
    // when it starts, so the pages compare this with what it serves (§9 Q2).
    documentsModule: () => ({
      ...documentsModuleOnDisk(documentsDeps.bundleDir, documentsDeps.packagesRoot),
      externalDaemon: backendManager.externalDaemonPid() !== null,
    }),
    // T1-2: after the launch check, never alongside it. The daemon this app
    // spawned is stopped around the CLI run (it holds the database lock the
    // CLI needs to revoke consent) and comes back with the change applied;
    // the page reloads once it is ready again.
    documentsModuleActions: {
      uninstall: () => documentsModuleAction(() => uninstallDocumentsModule({ ...documentsDeps, now: () => new Date() })),
      install: () => documentsModuleAction(() => installBundledDocumentsModule(documentsDeps)),
    },
  })
  cliTool = registerCliTool({
    guard, executable: process.execPath, resources: process.resourcesPath, packaged: app.isPackaged,
    changed: refreshTray,
    openSettings: () => {
      showOrCreateWindow()
      const contents = mainWin?.webContents
      if (contents?.isLoading()) contents.once('did-finish-load', () => contents.send(IpcChannels.CliToolOpenSettings))
      else contents?.send(IpcChannels.CliToolOpenSettings)
    },
  })
  void cliTool.refresh()
  mainWin = createMainWindow()
  void askResidentConsent(documentsModuleReady).catch((err) => console.error('[main] resident consent', err))
  // KB-C2-04a: main is the UI session's Provider in M2 (design §6).
  const knowledgeBindingTag = bindingTag(knowledgeAppInstanceId(app.getPath('userData')), KNOWLEDGE_BINDING)
  knowledgeView = new KnowledgeViewHost({
    window: () => mainWin,
    createView: (webPreferences) => new WebContentsView({ webPreferences }),
    sessionFor: (partition) => session.fromPartition(partition, { cache: false }),
    target: () => resolveKnowledgeViewTarget(process.env, app.isPackaged, knowledge?.status().endpoint),
    binding: () => knowledge?.viewBinding() ?? null,
    packaged: app.isPackaged,
    ui: (context) => {
      const inputs = () => knowledge?.uiSessionInputs(context.binding) ?? null
      return new KnowledgeUiInstance({
        client: new KnowledgeUiClient((url, init) => net.fetch(url, init), context.origin),
        bindingTag: knowledgeBindingTag,
        partition: context.partition,
        origin: context.origin,
        routes: KNOWLEDGE_UI_ROUTES,
        ledger: context.ledger,
        credential: () => inputs()?.credential ?? null,
        generation: () => inputs()?.generation ?? -1,
        servable: () => inputs() !== null,
        now: () => performance.now(),
        sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
        setTimer: (fn, ms) => setTimeout(fn, ms),
        clearTimer: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
        onEvent: context.onEvent,
        selected: () => knowledge?.status().knowledgeBaseIds ?? [],
        fetch: (url, init) => net.fetch(url, init),
      })
    },
    locale: () => app.getLocale(),
    onState: (state) => {
      if (mainWin && !mainWin.isDestroyed()) mainWin.webContents.send(IpcChannels.KnowledgeViewState, state)
    },
  })
  knowledge.onChange(() => knowledgeView?.bindingChanged())
  registerKnowledgeViewIpc(ipcMain, guard, {
    show: IpcChannels.KnowledgeViewShow,
    bounds: IpcChannels.KnowledgeViewBounds,
    hide: IpcChannels.KnowledgeViewHide,
    reload: IpcChannels.KnowledgeViewReload,
  }, () => knowledgeView)
  registerKnowledgeEntryIpc(ipcMain, guard, {
    conversation: (id) => createChatHistoryStore(app.getPath('userData')).get(id),
    reader: daemonFloorReader(),
    confirm: async (event, ask) => {
      const win = BrowserWindow.fromWebContents(event.sender)
      const options: Electron.MessageBoxOptions = {
        type: 'question', buttons: [ask.confirm, ask.cancel], defaultId: 1, cancelId: 1, message: ask.message, detail: ask.detail, noLink: true,
      }
      const result = win ? await dialog.showMessageBox(win, options) : await dialog.showMessageBox(options)
      return result.response === 0
    },
    locale: getMainLocale,
    bound: () => knowledge?.status().active === true,
  }, {
    // KB-SOP-01b-2: local drafts, drafted by the local model (no session_id: LocalOnly, no memory, no tools).
    store: createEntryDraftStore(app.getPath('userData')),
    chat: (body) => {
      const conn = daemonConnection()
      if (!conn) return Promise.resolve(proxyError('backend_not_ready', 'Backend daemon is not ready'))
      return proxyRequest(conn, { method: 'POST', path: '/api/v1/chat', body }, { host: '127.0.0.1', timeoutMs: CHAT_TIMEOUT_MS })
    },
    target: () => {
      if (!knowledge || knowledge.bindingForRun() === null) return null
      const s = knowledge.status()
      return entryTarget(s.endpoint, s.knowledgeBaseIds)
    },
    locale: getMainLocale,
  })
  ipcMain.handle(IpcChannels.CreativeShow, guard((_event, bounds: CreativeViewBounds) => showCreativeView(bounds)))
  ipcMain.handle(IpcChannels.CreativeRestart, guard((_event, bounds: CreativeViewBounds) => restartCreativeView(bounds)))
  ipcMain.handle(IpcChannels.CreativeBounds, guard((_event, bounds: CreativeViewBounds) => {
    if (creativeViewFence.wantsVisible() && creativeOrigin && creativeView && !creativeView.webContents.isDestroyed()) {
      creativeView.setBounds(normalizedBounds(bounds))
    }
  }))
  ipcMain.handle(IpcChannels.CreativeHide, guard(() => hideCreativeView()))
  ipcMain.handle(IpcChannels.OpenDesignComponentStatus, guard(() => openDesignComponent?.status()
    ?? { state: 'unavailable', reason: 'Open Design component installer is unavailable' }))
  ipcMain.handle(IpcChannels.OpenDesignComponentInstall, guard(() => openDesignComponent?.install()
    ?? Promise.resolve({ state: 'unavailable', reason: 'Open Design component installer is unavailable' })))
  ipcMain.handle(IpcChannels.OpenDesignComponentPause, guard(() => {
    openDesignComponent?.pause()
    return openDesignComponent?.status() ?? { state: 'unavailable', reason: 'Open Design component installer is unavailable' }
  }))
  openDesignComponent?.onProgress((status) => {
    openDesignReporter?.notify()
    if (mainWin && !mainWin.isDestroyed()) {
      mainWin.webContents.send(IpcChannels.OpenDesignComponentProgress, status)
    }
  })
  agentEarBridge.start()

  // A3-4 review M5: pull (snapshot on mount) + push (ongoing) for the voice
  // panel's log. Registered once here — the log itself is process-lifetime,
  // not per-window, so no cleanup needed on window recreation.
  ipcMain.handle(IpcChannels.AgentEarSnapshot, guard(() => agentEarLog.snapshot()))
  agentEarLog.subscribe((envelope) => {
    if (mainWin && !mainWin.isDestroyed()) {
      mainWin.webContents.send(IpcChannels.AgentEarEvent, envelope)
    }
  })

  // ME4-desktop-model-ui: same pull+push pair, for model-call-log.ts.
  ipcMain.handle(IpcChannels.ModelCallSnapshot, guard(() => modelCallLog.snapshot()))
  modelCallLog.subscribe((call) => {
    if (mainWin && !mainWin.isDestroyed()) {
      mainWin.webContents.send(IpcChannels.ModelCallEvent, call)
    }
  })

  // ── System tray (M2 base; F1b: live daemon status + start/stop/restart) ────
  // Empty image + setTitle works on macOS (menu-bar text); M3 will add a
  // proper multi-resolution icon asset for Windows/Linux.
  tray = new Tray(nativeImage.createEmpty())
  tray.on('double-click', () => showOrCreateWindow())
  refreshTray()
  // Poll so the menu-bar title/tooltip and menu track the daemon as it starts,
  // crashes, or is toggled from the menu itself.
  trayTimer = setInterval(() => {
    serviceUpgrader.tick()
    void crashLoop.sample()
    refreshTray()
    if (++cliCheckTicks % 15 === 0) void cliTool?.refresh()
    // Replays the wanted Knowledge bindings on a restarted daemon and retries
    // changes that did not land.
    knowledgeHost?.sync()
    void knowledge?.tick()
  }, 4_000)
  // ─────────────────────────────────────────────────────────────────────────

  // macOS: re-open window when dock icon clicked with no windows open
  app.on('activate', () => showOrCreateWindow())
})

// M5 B4b: asked once, with the window up. Allow installs the LaunchAgent,
// after the bundled documents module's launch install, with this app's own
// daemon stopped meanwhile; a failed install is undone and not
// kept, so the next launch asks again (PM r1 ②).
async function askResidentConsent(documentsReady: Promise<unknown>): Promise<void> {
  const dataDir = tryDataDir()
  if (dataDir === null) return
  const file = consentPath(dataDir)
  const fromBundle = upgradesFromBundle(app.isPackaged, process.env)
  if (!shouldAskConsent({ platform: process.platform, fromBundle, installed: launchAgentPlistExists(), consent: readConsent(file) })) return
  const outcome = await runConsent({
    ask: async () => {
      if (!mainWin || mainWin.isDestroyed()) mainWin = createMainWindow()
      const { response } = await dialog.showMessageBox(mainWin, {
        type: 'question',
        buttons: [mt('consent.allow'), mt('consent.deny')],
        defaultId: 0,
        cancelId: 1,
        message: mt('consent.message'),
        detail: mt('consent.detail'),
      })
      return response === 0 ? 'allow' : 'declined'
    },
    install: () => installService(serviceInstallDeps({
      cli: resolveBundledRustBinary('agent24', app.isPackaged),
      daemonBin: resolveRustBinary(app.isPackaged),
      ready: documentsReady,
      hold: (work) => backendManager.whileStopped(work),
      spawned: (pid) => backendManager.spawned(pid),
    })),
    record: (answer) => writeConsent(file, answer, new Date()),
  })
  if (typeof outcome === 'object') {
    console.warn('[main] resident service not installed:', outcome.failed)
    if (mainWin && !mainWin.isDestroyed()) void dialog.showMessageBox(mainWin, { type: 'warning', message: mt('consent.failed', { code: outcome.failed }) })
  }
  refreshTray()
}

// Rebuild the tray title, tooltip, and context menu from the live daemon status
// and the current UI language. Cheap and idempotent — safe to call on a timer,
// and called again whenever the language changes.
function refreshTray(): void {
  if (!tray) return
  const view = buildTrayView({
    status: backendManager.status(),
    detail: backendManager.statusDetail(),
    external: backendManager.isExternalDaemon(),
    enrolled: backendManager.presenceTarget() !== null,
    externalPid: backendManager.externalDaemonPid() ?? null,
    foreignPid: backendManager.foreignDaemonPid(),
    backendKind: backendManager.backendKind(),
    cliTool: cliTool?.status(),
    upgrade: serviceUpgrader.hint(),
    crashLoop: crashLoop.looping(),
    serviceWaitMs: backendManager.serviceWaitMs(),
  })
  if (process.platform === 'darwin') tray.setTitle(view.title)
  tray.setToolTip(view.tooltip)

  const actions: Record<TrayAction, () => void> = {
    'cli-settings': () => cliTool?.openSettings(),
    start: () => { backendManager.startDaemon(); refreshTray() },
    disconnect: () => { backendManager.stopDaemon(); refreshTray() },
    restart: () => { backendManager.restart(); refreshTray() },
    stop: () => { backendManager.stopDaemon(); refreshTray() },
    show: () => showOrCreateWindow(),
    quit: () => {
      // Clear tray ref so win.on('close') guard is skipped and app can quit
      tray = null
      app.quit()
    },
  }
  tray.setContextMenu(Menu.buildFromTemplate(view.items.map((item): MenuItemConstructorOptions =>
    'type' in item ? { type: 'separator' }
      : 'action' in item ? { label: item.label, click: actions[item.action] }
        : { label: item.label, enabled: false })))
}

const shutdown = createCreativeShutdown({
  prepare: () => {
    isQuitting = true
    openDesignComponent?.abort('Agent24 is quitting')
    void presencePoller?.stop()
    void presenceConnection?.stop()
    try {
      hideCreativeView()
      if (creativeView && !creativeView.webContents.isDestroyed()) {
        mainWin?.contentView.removeChildView(creativeView)
        creativeView.webContents.close()
      }
      creativeView = null
      creativeLoadedInstanceId = null
    } catch (error) {
      console.error('[main] failed to tear down the Creative view before quit', error)
    }
  },
  stop: async () => { await creativeServeWeb?.shutdown() },
  cleanup: () => {
    if (trayTimer) { clearInterval(trayTimer); trayTimer = null }
    stopOpenDesignReady?.()
    stopOpenDesignReady = null
    openDesignReporter?.stop()
    agentEarBridge.stop()
    agentEarLog.stop()
    backendManager.stop()
  },
  quit: () => app.quit(),
})
app.on('before-quit', shutdown.beforeQuit)
// KB-C2-03: app quit is binding-level destruction for the KnowledgeView (U4.8);
// from here on it refuses to open again (ADR §4: stop new calls, then destroy).
app.on('before-quit', () => knowledgeView?.shutdown())
app.on('will-quit', shutdown.willQuit)

// window-all-closed fires only if tray is null (i.e., user chose Quit from
// tray menu), because win.on('close') hides and prevents close while tray active.
app.on('window-all-closed', () => {
  if (!tray) app.quit()
})
