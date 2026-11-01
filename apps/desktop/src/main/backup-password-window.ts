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
export const BACKUP_PW_BUSY = 'backup-password:busy'

const escapeHtml = (text: string): string =>
  text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;')

export type BackupPasswordMode = 'export' | 'restore'

export interface BackupWindowOptions {
  /** Restore mode: tell the user the restored identity will become the default. */
  willBeDefault?: boolean
}

export function backupPasswordHtml(mode: BackupPasswordMode = 'export', options: BackupWindowOptions = {}): string {
  const t = (key: Parameters<typeof mt>[0]): string => escapeHtml(mt(key))
  return `<!doctype html><html><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'">
<title>${t(mode === 'export' ? 'backupPw.title' : 'backupPw.restoreTitle')}</title>
<style>
body{font:13px -apple-system,system-ui,sans-serif;margin:0;padding:18px;color:#1d1d1f;background:#fff}
@media (prefers-color-scheme: dark){body{color:#f5f5f7;background:#2a2a2c}input{background:#1c1c1e;color:#f5f5f7}}
h1{font-size:15px;margin:0 0 8px}p{margin:6px 0}label{display:block;margin-top:10px}
input{display:block;width:100%;box-sizing:border-box;padding:7px;margin-top:4px}
.rules{color:#6e6e73}.warn{font-weight:600}#err{color:#d94f4f;min-height:1.2em}
.row{display:flex;justify-content:flex-end;gap:8px;margin-top:10px}
</style></head><body><form id="f">
${mode === 'export' ? `<h1>${t('backupPw.title')}</h1>
<p class="rules">${t('backupPw.rules')}</p>
<label>${t('backupPw.password')}<input id="p1" type="password" autocomplete="new-password" autofocus></label>
<label>${t('backupPw.repeat')}<input id="p2" type="password" autocomplete="new-password"></label>
<p class="warn">${t('backupPw.warning')}</p>` : `<h1>${t('backupPw.restoreTitle')}</h1>
<p class="rules">${t('backupPw.restoreRules')}</p>
${options.willBeDefault ? `<p class="warn">${t('backupPw.willBeDefault')}</p>` : ''}
<label>${t('backupPw.restorePassword')}<input id="p1" type="password" autocomplete="off" autofocus></label>`}
<p id="err" role="alert"></p>
<div class="row"><button type="button" id="cancel">${t('backupPw.cancel')}</button><button type="submit" id="go">${t('backupPw.submit')}</button></div>
</form></body></html>`
}

type Outcome<T> = { done: true; value: T } | { done: false; error: string }

/**
 * Opens the password window (modal over `parent`). In `export` mode it asks
 * for a new password twice and resolves with it (or null on cancel). In
 * `restore` mode it asks once, shows a "decrypting…" state while `work` runs
 * with that password, lets the user retry in place if `work` says so, and
 * resolves with the final value (or null on cancel). The password is
 * validated here, in main, against the same rules as the daemon.
 */
function openPasswordWindow<T>(
  parent: BrowserWindow | null,
  mode: BackupPasswordMode,
  work: (password: string) => Promise<Outcome<T>>,
  options: BackupWindowOptions = {},
): Promise<T | null> {
  return new Promise((resolve) => {
    const win = new BrowserWindow({
      width: 460,
      height: mode === 'export' ? 400 : 280,
      ...(parent ? { parent, modal: true } : {}),
      show: false,
      resizable: false,
      minimizable: false,
      maximizable: false,
      fullscreenable: false,
      title: mt(mode === 'export' ? 'backupPw.title' : 'backupPw.restoreTitle'),
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
    let working = false

    const finish = (value: T | null): void => {
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
      if (!fromWindow(event) || working) return
      if (typeof first !== 'string' || typeof second !== 'string') return
      if (mode === 'export') {
        if (first !== second) { event.sender.send(BACKUP_PW_ERROR, mt('backupPw.mismatch')); return }
        if (!isSafeBackupPassword(first)) { event.sender.send(BACKUP_PW_ERROR, mt('backupPw.invalid')); return }
      } else if (!first || Buffer.byteLength(first, 'utf8') > 1024 || /[\r\n\0]/.test(first)) {
        event.sender.send(BACKUP_PW_ERROR, mt('backupPw.restoreInvalid'))
        return
      }
      working = true
      if (mode === 'restore') event.sender.send(BACKUP_PW_BUSY, mt('backupPw.decrypting'))
      let password: string | null = first
      work(password).then(
        (outcome) => {
          password = null
          working = false
          if (outcome.done) finish(outcome.value)
          else if (!contents.isDestroyed()) event.sender.send(BACKUP_PW_ERROR, outcome.error)
        },
        () => {
          password = null
          working = false
          finish(null)
        },
      )
    }
    const onCancel = (event: IpcMainEvent): void => {
      if (!fromWindow(event)) return
      // Never report "cancelled" while the daemon call is in flight: the
      // restore may well complete, and the user must not be told it did not.
      if (working) { event.sender.send(BACKUP_PW_BUSY, mt('backupPw.wait')); return }
      finish(null)
    }
    ipcMain.on(BACKUP_PW_SUBMIT, onSubmit)
    ipcMain.on(BACKUP_PW_CANCEL, onCancel)

    contents.setWindowOpenHandler(() => ({ action: 'deny' }))
    contents.on('will-navigate', (event) => event.preventDefault())
    // Closing the window (button, Cmd+W, Escape) is ignored while the daemon
    // call is in flight, for the same reason.
    win.on('close', (event: { preventDefault: () => void }) => {
      if (working) {
        event.preventDefault()
        if (!contents.isDestroyed()) contents.send(BACKUP_PW_BUSY, mt('backupPw.wait'))
      }
    })
    win.on('closed', () => finish(null))
    win.once('ready-to-show', () => win.show())
    void win.loadURL(`data:text/html;charset=utf-8,${encodeURIComponent(backupPasswordHtml(mode, options))}`).catch(() => finish(null))
  })
}

/** Asks for a NEW backup password (twice). Resolves with it, or null on cancel. */
export function promptBackupPassword(parent: BrowserWindow | null): Promise<string | null> {
  return openPasswordWindow<string>(parent, 'export', (password) => Promise.resolve({ done: true, value: password }))
}

/**
 * Asks once for the password a backup was made with and runs `work` with it
 * while the window shows "decrypting…". `work` may ask for a retry in place.
 */
export function promptRestorePassword<T>(
  parent: BrowserWindow | null,
  work: (password: string) => Promise<Outcome<T>>,
  options: BackupWindowOptions = {},
): Promise<T | null> {
  return openPasswordWindow<T>(parent, 'restore', work, options)
}
