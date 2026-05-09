// Agent24-Desktop main process entry — M2: integrates BackendManager daemon.
// Capability modules will register IPC handlers via the loader (M1 next tasks).

import { app, BrowserWindow, session } from 'electron'
import path from 'node:path'
import { registerIpcHandlers } from './ipc/index'
import { BackendManager } from './backend-manager'

const isDev = process.env.NODE_ENV === 'development'
const backendManager = new BackendManager()

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
  createMainWindow()

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) {
      createMainWindow()
    }
  })
})

app.on('will-quit', () => {
  backendManager.stop()
})

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit()
})
