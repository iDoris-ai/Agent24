// Main-owned, isolated window that collects the password encrypting a private
// key backup. The renderer (which could be compromised) never sends, sees or
// can substitute that password: it only asks for an export. The window loads
// its own inline document (no remote content, CSP forbids scripts), has its
// own minimal sandboxed preload, and its IPC channels accept messages only
// from this window's own webContents.

import path from 'node:path'
import { app, BrowserWindow, ipcMain, type IpcMainEvent } from 'electron'
import { isSafeBackupPassword } from './comm-validate'
import { mt } from './main-i18n'

export const BACKUP_PW_SUBMIT = 'backup-password:submit'
export const BACKUP_PW_CANCEL = 'backup-password:cancel'
export const BACKUP_PW_ERROR = 'backup-password:error'

const escapeHtml = (text: string): string =>
  text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;')

export function backupPasswordHtml(): string {
  const t = (key: Parameters<typeof mt>[0]): string => escapeHtml(mt(key))
  return `<!doctype html><html><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'">
<title>${t('backupPw.title')}</title>
<style>
body{font:13px -apple-system,system-ui,sans-serif;margin:0;padding:18px;color:#1d1d1f;background:#fff}
@media (prefers-color-scheme: dark){body{color:#f5f5f7;background:#2a2a2c}input{background:#1c1c1e;color:#f5f5f7}}
h1{font-size:15px;margin:0 0 8px}p{margin:6px 0}label{display:block;margin-top:10px}
input{display:block;width:100%;box-sizing:border-box;padding:7px;margin-top:4px}
.rules{color:#6e6e73}.warn{font-weight:600}#err{color:#d94f4f;min-height:1.2em}
.row{display:flex;justify-content:flex-end;gap:8px;margin-top:10px}
</style></head><body><form id="f">
<h1>${t('backupPw.title')}</h1>
<p class="rules">${t('backupPw.rules')}</p>
<label>${t('backupPw.password')}<input id="p1" type="password" autocomplete="new-password" autofocus></label>
<label>${t('backupPw.repeat')}<input id="p2" type="password" autocomplete="new-password"></label>
<p class="warn">${t('backupPw.warning')}</p>
<p id="err" role="alert"></p>
<div class="row"><button type="button" id="cancel">${t('backupPw.cancel')}</button><button type="submit">${t('backupPw.submit')}</button></div>
</form></body></html>`
}

/**
 * Opens the password window (modal over `parent`) and resolves with the
 * confirmed password, or null if the user cancels / closes it. The password is
 * validated here, in main, against the same rule as the daemon.
 */
export function promptBackupPassword(parent: BrowserWindow | null): Promise<string | null> {
  return new Promise((resolve) => {
    const win = new BrowserWindow({
      width: 460,
      height: 400,
      ...(parent ? { parent, modal: true } : {}),
      show: false,
      resizable: false,
      minimizable: false,
      maximizable: false,
      fullscreenable: false,
      title: mt('backupPw.title'),
      webPreferences: {
        preload: path.join(__dirname, 'backup-password-preload.js'),
        contextIsolation: true,
        sandbox: true,
        nodeIntegration: false,
        webSecurity: true,
        allowRunningInsecureContent: false,
        // No devtools in packaged builds: they could read the inputs.
        devTools: !app.isPackaged,
      },
    })
    const contents = win.webContents
    let settled = false

    const finish = (value: string | null): void => {
      if (settled) return
      settled = true
      ipcMain.removeListener(BACKUP_PW_SUBMIT, onSubmit)
      ipcMain.removeListener(BACKUP_PW_CANCEL, onCancel)
      if (!win.isDestroyed()) win.close()
      resolve(value)
    }
    // Only THIS window's top frame may speak on these channels.
    const fromWindow = (event: IpcMainEvent): boolean =>
      !contents.isDestroyed() && event.sender === contents &&
      (event.senderFrame === undefined || event.senderFrame === null || event.senderFrame === contents.mainFrame)

    const onSubmit = (event: IpcMainEvent, first: unknown, second: unknown): void => {
      if (!fromWindow(event)) return
      if (typeof first !== 'string' || typeof second !== 'string') return
      if (first !== second) { event.sender.send(BACKUP_PW_ERROR, mt('backupPw.mismatch')); return }
      if (!isSafeBackupPassword(first)) { event.sender.send(BACKUP_PW_ERROR, mt('backupPw.invalid')); return }
      finish(first)
    }
    const onCancel = (event: IpcMainEvent): void => {
      if (!fromWindow(event)) return
      finish(null)
    }
    ipcMain.on(BACKUP_PW_SUBMIT, onSubmit)
    ipcMain.on(BACKUP_PW_CANCEL, onCancel)

    contents.setWindowOpenHandler(() => ({ action: 'deny' }))
    contents.on('will-navigate', (event) => event.preventDefault())
    win.on('closed', () => finish(null))
    win.once('ready-to-show', () => win.show())
    void win.loadURL(`data:text/html;charset=utf-8,${encodeURIComponent(backupPasswordHtml())}`).catch(() => finish(null))
  })
}
