// Agent24 main process entry — M2: integrates BackendManager daemon.

import { app, BrowserWindow, Menu, Tray, nativeImage, session } from 'electron'
import path from 'node:path'
import { registerIpcHandlers } from './ipc/index'
import { BackendManager } from './backend-manager'

const isDev = process.env.NODE_ENV === 'development'
const backendManager = new BackendManager()

// Keep tray reference alive — GC would destroy it otherwise
let tray: Tray | null = null
// Mutable reference so tray handlers always point to the current window
let mainWin: BrowserWindow | null = null
// Set to true by before-quit so win.on('close') guard is skipped on app exit
let isQuitting = false

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

  // ── System tray (M2) ─────────────────────────────────────────────────────
  // Empty image + setTitle works on macOS (menu-bar text); M3 will add a
  // proper multi-resolution icon asset for Windows/Linux.
  tray = new Tray(nativeImage.createEmpty())
  if (process.platform === 'darwin') {
    tray.setTitle('⚡A24')
  }
  tray.setToolTip('Agent24 — 本地 AI 助理')

  const contextMenu = Menu.buildFromTemplate([
    {
      label: 'Show Window',
      // Uses showOrCreateWindow so it's safe even after window recreation
      click: () => showOrCreateWindow(),
    },
    { type: 'separator' },
    {
      label: 'Quit Agent24',
      click: () => {
        // Clear tray ref so win.on('close') guard is skipped and app can quit
        tray = null
        app.quit()
      },
    },
  ])
  tray.setContextMenu(contextMenu)
  tray.on('double-click', () => showOrCreateWindow())
  // ─────────────────────────────────────────────────────────────────────────

  // macOS: re-open window when dock icon clicked with no windows open
  app.on('activate', () => showOrCreateWindow())
})

app.on('before-quit', () => {
  isQuitting = true
})

app.on('will-quit', () => {
  backendManager.stop()
})

// window-all-closed fires only if tray is null (i.e., user chose Quit from
// tray menu), because win.on('close') hides and prevents close while tray active.
app.on('window-all-closed', () => {
  if (!tray) app.quit()
})
