// Agent24-Desktop main process entry — M2: integrates BackendManager daemon.
// Capability modules will register IPC handlers via the loader (M1 next tasks).

import { app, BrowserWindow, Menu, Tray, nativeImage, session } from 'electron'
import path from 'node:path'
import { registerIpcHandlers } from './ipc/index'
import { BackendManager } from './backend-manager'

const isDev = process.env.NODE_ENV === 'development'
const backendManager = new BackendManager()

// Keep tray reference alive — GC would destroy it otherwise
let tray: Tray | null = null

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

  if (isDev) {
    void win.loadURL('http://localhost:5173')
    win.webContents.openDevTools({ mode: 'detach' })
  } else {
    void win.loadFile(path.join(__dirname, '../renderer/index.html'))
  }

  return win
}

process.on('uncaughtException', (err) => {
  console.error('[main] uncaughtException', err)
})
process.on('unhandledRejection', (reason) => {
  console.error('[main] unhandledRejection', reason)
})

app.whenReady().then(() => {
  backendManager.start()
  // Enforce a strict Content-Security-Policy. 'unsafe-inline' on style-src
  // is required for React's inline styles; remove if switching to CSS modules.
  // M2 will tighten this further (no 'unsafe-inline' on style-src) once the
  // full UI design is settled.
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
  const win = createMainWindow()

  // ── System tray (M2) ─────────────────────────────────────────────────────
  tray = new Tray(nativeImage.createEmpty())
  if (process.platform === 'darwin') {
    tray.setTitle('⚡A24')
  }
  tray.setToolTip('Agent24 — 本地 AI 助理')

  const contextMenu = Menu.buildFromTemplate([
    {
      label: 'Show Window',
      click: () => {
        win.show()
        if (process.platform === 'darwin') app.focus()
      },
    },
    { type: 'separator' },
    {
      label: 'Quit Agent24',
      click: () => {
        app.quit()
      },
    },
  ])
  tray.setContextMenu(contextMenu)
  tray.on('double-click', () => {
    win.show()
    if (process.platform === 'darwin') app.focus()
  })
  // ─────────────────────────────────────────────────────────────────────────

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) {
      createMainWindow()
    } else {
      win.show()
    }
  })
})

app.on('will-quit', () => {
  backendManager.stop()
})

// M2: When all windows are closed, hide rather than quit — the daemon (tray)
// keeps running. User must use tray "Quit Agent24" to fully exit.
app.on('window-all-closed', () => {
  // Hide all windows to keep the daemon alive in the background.
  // On all platforms: quit only via tray context menu.
  BrowserWindow.getAllWindows().forEach((w) => w.hide())
})
