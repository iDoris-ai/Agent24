// Agent24 main process entry — M2: integrates BackendManager daemon.

import { app, BrowserWindow, Menu, Tray, WebContentsView, nativeImage, safeStorage, session, ipcMain, shell, type MenuItemConstructorOptions } from 'electron'
import path from 'node:path'
import { registerIpcHandlers } from './ipc/index'
import { registerCliTool } from './ipc/cli-tool'
import { initMainLocale, onMainLocaleChange } from './main-i18n'
import { buildTrayView, type TrayAction } from './tray-menu'
import { createAppUrlPolicy, createIpcGuard, installNavigationGuards } from './ipc/guard'
import { BackendManager } from './backend-manager'
import { AgentEarEventBridge } from './agentear-events'
import { documentsEvent } from './documents'
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
} from './creative-serve-web'
import { OpenDesignComponentInstaller } from './open-design-component'
import { KnowledgeHost } from './knowledge-host'
import { KnowledgeLoginStore } from './knowledge-logins'

const isDev = process.env.NODE_ENV === 'development'
const backendManager = new BackendManager()
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
)

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
let openDesignComponent: OpenDesignComponentInstaller | null = null
let creativeView: WebContentsView | null = null
let creativeOrigin: string | null = null
const creativeViewFence = new CreativeViewRequestFence()

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
  const current = creativeView.webContents.getURL()
  if (forceReload || classifyCreativeUrl(current, status.origin) !== 'same-origin') {
    await creativeView.webContents.loadURL(status.origin)
  }
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
    runtimeExecutable: process.execPath,
    pinVersion: OPEN_DESIGN_PIN_VERSION,
    component: openDesignComponent ?? undefined,
  })
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
  void backendManager.start()
  // safeStorage needs the app ready. The logins load once, here, before
  // anything can change them; the reason is logged, never the contents.
  knowledgeHost = new KnowledgeHost(new KnowledgeLoginStore(app.getPath('userData'), safeStorage))
  const knowledgeLogins = knowledgeHost.start()
  if (!knowledgeLogins.ok) console.warn(`[knowledge] logins unavailable: ${knowledgeLogins.reason}`)
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

  registerIpcHandlers({ guard })
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
    refreshTray()
    if (++cliCheckTicks % 15 === 0) void cliTool?.refresh()
    // Replays the wanted Knowledge bindings on a restarted daemon and retries
    // changes that did not land.
    knowledgeHost?.sync()
  }, 4_000)
  // ─────────────────────────────────────────────────────────────────────────

  // macOS: re-open window when dock icon clicked with no windows open
  app.on('activate', () => showOrCreateWindow())
})

// Rebuild the tray title, tooltip, and context menu from the live daemon status
// and the current UI language. Cheap and idempotent — safe to call on a timer,
// and called again whenever the language changes.
function refreshTray(): void {
  if (!tray) return
  const view = buildTrayView({
    status: backendManager.status(),
    external: backendManager.isExternalDaemon(),
    externalPid: backendManager.externalDaemonPid() ?? null,
    foreignPid: backendManager.foreignDaemonPid(),
    backendKind: backendManager.backendKind(),
    cliTool: cliTool?.status(),
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

app.on('before-quit', () => {
  isQuitting = true
  // M5: cancel an in-flight Open Design component download rather than
  // leaving it (and its partial files) orphaned past app exit.
  openDesignComponent?.abort('Agent24 is quitting')
})

app.on('will-quit', () => {
  if (trayTimer) { clearInterval(trayTimer); trayTimer = null }
  agentEarBridge.stop()
  agentEarLog.stop()
  backendManager.stop()
  void creativeServeWeb?.stop()
})

// window-all-closed fires only if tray is null (i.e., user chose Quit from
// tray menu), because win.on('close') hides and prevents close while tray active.
app.on('window-all-closed', () => {
  if (!tray) app.quit()
})
