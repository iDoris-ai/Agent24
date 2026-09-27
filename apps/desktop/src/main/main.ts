// Agent24 main process entry — M2: integrates BackendManager daemon.

import { app, BrowserWindow, Menu, Tray, nativeImage, session, ipcMain, type MenuItemConstructorOptions } from 'electron'
import path from 'node:path'
import { registerIpcHandlers } from './ipc/index'
import { BackendManager, type BackendStatus } from './backend-manager'
import { AgentEarEventBridge } from './agentear-events'
import { AgentEarEventLog } from './agentear-log'
import { ModelCallLog } from './model-call-log'
import { IpcChannels } from '../shared/ipc-types'

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

  backendManager.start()
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
  const meta = STATUS_META[status]
  if (process.platform === 'darwin') tray.setTitle(meta.title)
  tray.setToolTip(`Agent24 — ${meta.label.replace(/^[●○✕]\s*/, '')}（${backendManager.backendKind()}）`)

  const daemonItems: MenuItemConstructorOptions[] =
    status === 'stopped'
      ? [{ label: '启动 daemon', click: () => { backendManager.startDaemon(); refreshTray() } }]
      : [
          { label: '重启 daemon', click: () => { backendManager.restart(); refreshTray() } },
          { label: '停止 daemon', click: () => { backendManager.stopDaemon(); refreshTray() } },
        ]

  tray.setContextMenu(Menu.buildFromTemplate([
    { label: meta.label, enabled: false },
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
})

// window-all-closed fires only if tray is null (i.e., user chose Quit from
// tray menu), because win.on('close') hides and prevents close while tray active.
app.on('window-all-closed', () => {
  if (!tray) app.quit()
})
