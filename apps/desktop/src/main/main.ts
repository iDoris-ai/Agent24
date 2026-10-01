// Agent24 main process entry — M2: integrates BackendManager daemon.

import { app, BrowserWindow, Menu, Tray, WebContentsView, nativeImage, session, ipcMain, shell, type MenuItemConstructorOptions } from 'electron'
import path from 'node:path'
import { registerIpcHandlers } from './ipc/index'
import { BackendManager, type BackendStatus } from './backend-manager'
import { AgentEarEventBridge } from './agentear-events'
import { AgentEarEventLog } from './agentear-log'
import { ModelCallLog } from './model-call-log'
import { IpcChannels } from '../shared/ipc-types'
import type { CreativeViewBounds, CreativeViewResult } from '../shared/ipc-types'
import {
  CREATIVE_SESSION_PARTITION,
  CreativeServeWeb,
  classifyCreativeUrl,
} from './creative-serve-web'

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

// Keep tray reference alive — GC would destroy it otherwise
let tray: Tray | null = null
// Mutable reference so tray handlers always point to the current window
let mainWin: BrowserWindow | null = null
// Set to true by before-quit so win.on('close') guard is skipped on app exit
let isQuitting = false
// F1b: periodic tray refresh so the menu-bar reflects live daemon status
let trayTimer: NodeJS.Timeout | null = null
const creativeServeWeb = new CreativeServeWeb()
let creativeView: WebContentsView | null = null
let creativeOrigin: string | null = null

function normalizedBounds(bounds: CreativeViewBounds): Electron.Rectangle {
  return {
    x: Math.max(0, Math.round(bounds.x)),
    y: Math.max(0, Math.round(bounds.y)),
    width: Math.max(1, Math.round(bounds.width)),
    height: Math.max(1, Math.round(bounds.height)),
  }
}

function hideCreativeView(): void {
  if (!creativeView) return
  creativeView.setBounds({ x: 0, y: 0, width: 1, height: 1 })
}

async function showCreativeView(bounds: CreativeViewBounds): Promise<CreativeViewResult> {
  const win = mainWin
  if (!win || win.isDestroyed()) return { ok: false, error: 'Agent24 window is unavailable' }

  const status = await creativeServeWeb.start()
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
    creativeView.webContents.on('will-redirect', (event, url) => {
      const disposition = creativeOrigin ? classifyCreativeUrl(url, creativeOrigin) : 'blocked'
      if (disposition !== 'same-origin') event.preventDefault()
    })
  }

  creativeView.setBounds(normalizedBounds(bounds))
  const current = creativeView.webContents.getURL()
  if (classifyCreativeUrl(current, status.origin) !== 'same-origin') {
    await creativeView.webContents.loadURL(status.origin)
  }
  return { ok: true, origin: status.origin }
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

app.whenReady().then(() => {
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

  registerIpcHandlers()
  mainWin = createMainWindow()
  ipcMain.handle(IpcChannels.CreativeShow, (_event, bounds: CreativeViewBounds) => showCreativeView(bounds))
  ipcMain.handle(IpcChannels.CreativeBounds, (_event, bounds: CreativeViewBounds) => {
    if (creativeView && !creativeView.webContents.isDestroyed()) creativeView.setBounds(normalizedBounds(bounds))
  })
  ipcMain.handle(IpcChannels.CreativeHide, () => hideCreativeView())
  agentEarBridge.start()

  // A3-4 review M5: pull (snapshot on mount) + push (ongoing) for the voice
  // panel's log. Registered once here — the log itself is process-lifetime,
  // not per-window, so no cleanup needed on window recreation.
  ipcMain.handle(IpcChannels.AgentEarSnapshot, () => agentEarLog.snapshot())
  agentEarLog.subscribe((envelope) => {
    if (mainWin && !mainWin.isDestroyed()) {
      mainWin.webContents.send(IpcChannels.AgentEarEvent, envelope)
    }
  })

  // ME4-desktop-model-ui: same pull+push pair, for model-call-log.ts.
  ipcMain.handle(IpcChannels.ModelCallSnapshot, () => modelCallLog.snapshot())
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
  trayTimer = setInterval(refreshTray, 4_000)
  // ─────────────────────────────────────────────────────────────────────────

  // macOS: re-open window when dock icon clicked with no windows open
  app.on('activate', () => showOrCreateWindow())
})

// F1b: labels/glyphs for the current daemon status.
const STATUS_META: Record<BackendStatus, { title: string; label: string }> = {
  running: { title: '⚡A24', label: '● daemon：运行中' },
  starting: { title: '…A24', label: '○ daemon：启动中…' },
  stopped: { title: '⚠️A24', label: '✕ daemon：已停止' },
}

// Rebuild the tray title, tooltip, and context menu from the live daemon status.
// Cheap and idempotent — safe to call on a timer.
function refreshTray(): void {
  if (!tray) return
  const status = backendManager.status()
  const external = backendManager.isExternalDaemon()
  const meta = STATUS_META[status]
  // DEP-A5: an external daemon (reused from `agent24 daemon start`, not
  // spawned by this app) gets a visibly different label — the whole point of
  // requirement #2 is that the user can tell them apart before hitting stop.
  const label = external ? `${meta.label}（外部 daemon · pid ${backendManager.externalDaemonPid() ?? '?'}）` : meta.label
  if (process.platform === 'darwin') tray.setTitle(meta.title)
  tray.setToolTip(`Agent24 — ${label.replace(/^[●○✕]\s*/, '')}（${backendManager.backendKind()}）`)

  // DEP-A5 requirement #2: stop/restart must only ever act on a daemon this
  // app spawned. BackendManager.stopDaemon()/restart() already guard against
  // killing an external daemon at the method level (see backend-manager.ts),
  // but the external case also gets its own menu wording here — "断开" instead
  // of "停止/重启" — so the tray itself never implies a kill that won't happen.
  const daemonItems: MenuItemConstructorOptions[] =
    status === 'stopped'
      ? [{ label: '启动 daemon', click: () => { backendManager.startDaemon(); refreshTray() } }]
      : external
        ? [{ label: '断开外部 daemon（不会终止它）', click: () => { backendManager.stopDaemon(); refreshTray() } }]
        : [
            { label: '重启 daemon', click: () => { backendManager.restart(); refreshTray() } },
            { label: '停止 daemon', click: () => { backendManager.stopDaemon(); refreshTray() } },
          ]

  tray.setContextMenu(Menu.buildFromTemplate([
    { label, enabled: false },
    { type: 'separator' },
    { label: '显示窗口', click: () => showOrCreateWindow() },
    { type: 'separator' },
    ...daemonItems,
    { type: 'separator' },
    {
      label: '退出 Agent24',
      click: () => {
        // Clear tray ref so win.on('close') guard is skipped and app can quit
        tray = null
        app.quit()
      },
    },
  ]))
}

app.on('before-quit', () => {
  isQuitting = true
})

app.on('will-quit', () => {
  if (trayTimer) { clearInterval(trayTimer); trayTimer = null }
  agentEarBridge.stop()
  agentEarLog.stop()
  backendManager.stop()
  void creativeServeWeb.stop()
})

// window-all-closed fires only if tray is null (i.e., user chose Quit from
// tray menu), because win.on('close') hides and prevents close while tray active.
app.on('window-all-closed', () => {
  if (!tray) app.quit()
})
