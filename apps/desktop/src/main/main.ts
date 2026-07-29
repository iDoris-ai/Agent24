// Agent24 main process entry — M2: integrates BackendManager daemon.

import { app, BrowserWindow, Menu, Tray, nativeImage, session, type MenuItemConstructorOptions } from 'electron'
import path from 'node:path'
import { registerIpcHandlers } from './ipc/index'
import { BackendManager, type BackendStatus } from './backend-manager'

const isDev = process.env.NODE_ENV === 'development'
const backendManager = new BackendManager()

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
    void win.loadURL('http://localhost:5173')
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
  backendManager.start()
  session.defaultSession.webRequest.onHeadersReceived((details, callback) => {
    callback({
      responseHeaders: {
        ...details.responseHeaders,
        'Content-Security-Policy': [
          "default-src 'self'; " +
          "script-src 'self'" + (isDev ? " 'unsafe-inline' 'unsafe-eval' http://localhost:5173" : "") + "; " +
          "style-src 'self' 'unsafe-inline'; " +
          "img-src 'self' data:; " +
          "connect-src 'self'" + (isDev ? " http://localhost:5173 ws://localhost:5173 http://localhost:8765" : "") + "; " +
          "font-src 'self'",
        ],
      },
    })
  })

  registerIpcHandlers()
  mainWin = createMainWindow()

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
  backendManager.stop()
})

// window-all-closed fires only if tray is null (i.e., user chose Quit from
// tray menu), because win.on('close') hides and prevents close while tray active.
app.on('window-all-closed', () => {
  if (!tray) app.quit()
})
