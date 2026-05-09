// Agent24-Desktop main process entry — M1 scaffolding.
// Capability modules will register IPC handlers via the loader (M1 next tasks).

import { app, BrowserWindow, dialog, session } from 'electron'
import path from 'node:path'
import { registerIpcHandlers } from './ipc/index'
import { initKernel, getKernel } from './kernel'

const isDev = process.env.NODE_ENV === 'development'

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
      sandbox: true,
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

app.whenReady().then(() => {
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
          "script-src 'self'; " +
          "style-src 'self' 'unsafe-inline'; " +
          "img-src 'self' data:; " +
          "connect-src 'self'" + (isDev ? " ws://localhost:5173" : "") + "; " +
          "font-src 'self'",
        ],
      },
    })
  })

  initKernel()
  registerIpcHandlers()
  createMainWindow()

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) {
      createMainWindow()
    }
  })
}).catch((err: Error) => {
  dialog.showErrorBox('Agent24 Startup Error', err.message)
  app.quit()
})

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit()
})

app.on('before-quit', (e) => {
  const k = (() => { try { return getKernel() } catch { return null } })()
  if (!k) return
  e.preventDefault()
  void k.shutdown().finally(() => app.quit())
})
