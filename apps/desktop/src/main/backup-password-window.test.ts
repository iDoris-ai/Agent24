// The main-owned backup-password window: hardened webPreferences, and IPC that
// only this window's own webContents can use.
import fs from 'node:fs'
import path from 'node:path'
import { beforeEach, describe, expect, it, vi } from 'vitest'

const h = vi.hoisted(() => {
type Listener = (event: unknown, ...args: unknown[]) => void
  const ipcListeners = new Map<string, Set<Listener>>()
  const windows: FakeWindow[] = []
  
  class FakeContents {
    static nextId = 1
    id = FakeContents.nextId++
    mainFrame = { id: 'main' }
    sent: unknown[][] = []
    destroyed = false
    isDestroyed() { return this.destroyed }
    send(...args: unknown[]) { this.sent.push(args) }
    setWindowOpenHandler = vi.fn()
    on = vi.fn()
  }
  class FakeWindow {
    static options: Record<string, unknown>[] = []
    webContents = new FakeContents()
    closed = false
    handlers = new Map<string, () => void>()
    loadedUrl = ''
    constructor(options: Record<string, unknown>) {
      FakeWindow.options.push(options)
      windows.push(this)
    }
    on(event: string, fn: () => void) { this.handlers.set(event, fn) }
    once(event: string, fn: () => void) { this.handlers.set(event, fn) }
    show() {}
    isDestroyed() { return this.closed }
    close() { this.closed = true; this.handlers.get('closed')?.() }
    loadURL(url: string) { this.loadedUrl = url; return Promise.resolve() }
  }
  return { ipcListeners, windows, FakeWindow, FakeContents }
})
const { ipcListeners, windows, FakeWindow, FakeContents } = h

vi.mock('electron', () => ({
  app: { isPackaged: true },
  BrowserWindow: h.FakeWindow,
  ipcMain: {
    on: (channel: string, fn: Listener) => {
      if (!h.ipcListeners.has(channel)) h.ipcListeners.set(channel, new Set())
      h.ipcListeners.get(channel)!.add(fn)
    },
    removeListener: (channel: string, fn: Listener) => { h.ipcListeners.get(channel)?.delete(fn) },
  },
}))

import { setMainLocale } from './main-i18n'
import {
  BACKUP_PW_BUSY, BACKUP_PW_CANCEL, BACKUP_PW_ERROR, BACKUP_PW_SUBMIT, backupPasswordHtml, promptBackupPassword, promptHomeImportPassword, promptRestorePassword, promptUnlockPassword,
} from './backup-password-window'

const emit = (channel: string, event: unknown, ...args: unknown[]) => {
  for (const fn of [...(ipcListeners.get(channel) ?? [])]) fn(event, ...args)
}
const fromWindow = (w: FakeWindow) => ({ sender: w.webContents, senderFrame: w.webContents.mainFrame })

beforeEach(() => {
  ipcListeners.clear()
  windows.length = 0
  FakeWindow.options.length = 0
  setMainLocale('en')
})

describe('backup password window', () => {
  it('is isolated: sandboxed, no node, context-isolated, no devtools when packaged, own preload, modal on the parent', () => {
    const parent = { id: 'parent' }
    void promptBackupPassword(parent as never)
    const o = FakeWindow.options[0]!
    const prefs = o['webPreferences'] as Record<string, unknown>
    expect(prefs).toMatchObject({ contextIsolation: true, sandbox: true, nodeIntegration: false, webSecurity: true, devTools: false })
    expect(String(prefs['preload'])).toMatch(/backup-password-preload\.js$/)
    expect(o['parent']).toBe(parent)
    expect(o['modal']).toBe(true)
    expect(windows[0]!.webContents.setWindowOpenHandler).toHaveBeenCalled()
  })

  it('loads only an inline document whose CSP forbids scripts and any remote content', () => {
    void promptBackupPassword(null)
    const url = windows[0]!.loadedUrl
    expect(url.startsWith('data:text/html')).toBe(true)
    const html = backupPasswordHtml()
    expect(html).toContain("default-src 'none'")
    expect(html).not.toMatch(/<script/i)
    expect(html).not.toMatch(/https?:\/\//)
    expect(html).toContain('Backup password')
    expect(html).toContain('Agent24 does not store it')
  })

  it('is localised', () => {
    setMainLocale('zh')
    expect(backupPasswordHtml()).toContain('备份口令')
    setMainLocale('th')
    expect(backupPasswordHtml()).toContain('รหัสผ่านสำรอง')
  })

  it('resolves with the confirmed password from its own window and closes', async () => {
    const result = promptBackupPassword(null)
    const w = windows[0]!
    emit(BACKUP_PW_SUBMIT, fromWindow(w), 'good-pass-1', 'good-pass-1')
    await expect(result).resolves.toBe('good-pass-1')
    expect(w.closed).toBe(true)
    expect(ipcListeners.get(BACKUP_PW_SUBMIT)?.size ?? 0).toBe(0)
  })

  it('ignores submit and cancel from any other webContents (including another BrowserWindow, the page, a stale frame)', async () => {
    const result = promptBackupPassword(null)
    const w = windows[0]!
    const other = new FakeContents()
    emit(BACKUP_PW_SUBMIT, { sender: other, senderFrame: other.mainFrame }, 'attacker-pass-1', 'attacker-pass-1')
    emit(BACKUP_PW_CANCEL, { sender: other, senderFrame: other.mainFrame })
    emit(BACKUP_PW_SUBMIT, { sender: w.webContents, senderFrame: { id: 'subframe' } }, 'iframe-pass-1', 'iframe-pass-1')
    let settled = false
    void result.then(() => { settled = true })
    await Promise.resolve()
    expect(settled).toBe(false)
    expect(w.closed).toBe(false)
    emit(BACKUP_PW_SUBMIT, fromWindow(w), 'real-pass-12', 'real-pass-12')
    await expect(result).resolves.toBe('real-pass-12')
  })

  it('rejects a mismatch or an unusable password in main, with a message back to the window, and stays open', async () => {
    const result = promptBackupPassword(null)
    const w = windows[0]!
    emit(BACKUP_PW_SUBMIT, fromWindow(w), 'good-pass-1', 'good-pass-2')
    emit(BACKUP_PW_SUBMIT, fromWindow(w), 'short', 'short')
    emit(BACKUP_PW_SUBMIT, fromWindow(w), 'line\nbreak-pass', 'line\nbreak-pass')
    emit(BACKUP_PW_SUBMIT, fromWindow(w), 123, 456)
    expect(w.webContents.sent.map((a) => a[0])).toEqual([BACKUP_PW_ERROR, BACKUP_PW_ERROR, BACKUP_PW_ERROR])
    expect(w.closed).toBe(false)
    emit(BACKUP_PW_CANCEL, fromWindow(w))
    await expect(result).resolves.toBeNull()
  })

  it('cancel (button or Escape, both send the cancel channel) and closing the window resolve null', async () => {
    const a = promptBackupPassword(null)
    emit(BACKUP_PW_CANCEL, fromWindow(windows[0]!))
    await expect(a).resolves.toBeNull()
    const b = promptBackupPassword(null)
    windows[1]!.close()
    await expect(b).resolves.toBeNull()
  })
})

describe('unlock and home-import password windows (L4)', () => {
  it('both ask once with their own wording, no script, in en/zh/th', () => {
    for (const mode of ['unlock', 'homeImport'] as const) {
      const html = backupPasswordHtml(mode)
      expect(html).toContain('id="p1"')
      expect(html).not.toContain('id="p2"')
      expect(html).not.toMatch(/<script/i)
      expect(html).toContain("default-src 'none'")
    }
    expect(backupPasswordHtml('unlock')).toContain('Unlock communication')
    expect(backupPasswordHtml('homeImport')).toContain('Import a Hyphae folder')
    setMainLocale('zh')
    expect(backupPasswordHtml('unlock')).toContain('解锁通信')
    expect(backupPasswordHtml('homeImport')).toContain('导入 Hyphae 文件夹')
    setMainLocale('th')
    expect(backupPasswordHtml('unlock')).toContain('ปลดล็อกการสื่อสาร')
    expect(backupPasswordHtml('homeImport')).toContain('นำเข้าโฟลเดอร์ Hyphae')
  })
  it('unlock says the password will be saved in the keychain only when asked to remember', () => {
    expect(backupPasswordHtml('unlock', { remember: true })).toContain('saved in your system keychain')
    expect(backupPasswordHtml('unlock')).not.toContain('saved in your system keychain')
  })
  it('runs the work with the typed password and resolves with its result', async () => {
    const work = vi.fn(async (pw: string) => ({ done: true as const, value: `unlocked:${pw}` }))
    const result = promptUnlockPassword(null, work)
    const w = windows[0]!
    emit(BACKUP_PW_SUBMIT, fromWindow(w), 'ks-pass', 'ks-pass')
    await expect(result).resolves.toBe('unlocked:ks-pass')
    expect(w.webContents.sent[0]).toEqual([BACKUP_PW_BUSY, 'Working…'])
    const other = promptHomeImportPassword(null, async (pw) => ({ done: true as const, value: `home:${pw}` }))
    emit(BACKUP_PW_SUBMIT, fromWindow(windows[1]!), 'old-pass', 'old-pass')
    await expect(other).resolves.toBe('home:old-pass')
  })
  it('rejects an empty or multi-line password in main', async () => {
    const work = vi.fn(async () => ({ done: true as const, value: 1 }))
    void promptUnlockPassword(null, work)
    const w = windows[0]!
    for (const bad of ['', 'a\nb']) emit(BACKUP_PW_SUBMIT, fromWindow(w), bad, bad)
    expect(work).not.toHaveBeenCalled()
    expect(w.webContents.sent.every(([channel]) => channel === BACKUP_PW_ERROR)).toBe(true)
  })
})

describe('restore password window', () => {
  const tick = () => new Promise((r) => setTimeout(r, 0))

  it('asks once (no repeat field), is as hardened as the export window, and has no script', () => {
    void promptRestorePassword(null, async () => ({ done: true, value: 1 }))
    const prefs = FakeWindow.options[0]!['webPreferences'] as Record<string, unknown>
    expect(prefs).toMatchObject({ contextIsolation: true, sandbox: true, nodeIntegration: false, devTools: false })
    const html = backupPasswordHtml('restore')
    expect(html).toContain('id="p1"')
    expect(html).not.toContain('id="p2"')
    expect(html).not.toMatch(/<script/i)
    expect(html).toContain("default-src 'none'")
    expect(html).toContain('Restore from backup')
    setMainLocale('zh')
    expect(backupPasswordHtml('restore')).toContain('从备份恢复')
    setMainLocale('th')
    expect(backupPasswordHtml('restore')).toContain('กู้คืนจากไฟล์สำรอง')
  })

  it('runs the work with the typed password, shows "Decrypting…" meanwhile, and closes with the result', async () => {
    let release!: () => void
    const work = vi.fn((pw: string) => new Promise<{ done: true; value: string }>((resolve) => { release = () => resolve({ done: true, value: `ok:${pw}` }) }))
    const result = promptRestorePassword(null, work)
    const w = windows[0]!
    emit(BACKUP_PW_SUBMIT, fromWindow(w), 'whatever-it-was', 'whatever-it-was')
    expect(work).toHaveBeenCalledWith('whatever-it-was')
    expect(w.webContents.sent[0]).toEqual([BACKUP_PW_BUSY, 'Decrypting…'])
    // while working: a second submit and a cancel are ignored, the window stays
    emit(BACKUP_PW_SUBMIT, fromWindow(w), 'second', 'second')
    emit(BACKUP_PW_CANCEL, fromWindow(w))
    expect(work).toHaveBeenCalledTimes(1)
    expect(w.closed).toBe(false)
    release()
    await expect(result).resolves.toBe('ok:whatever-it-was')
    expect(w.closed).toBe(true)
  })

  it('lets the user retry in place when the work asks for it, then finishes', async () => {
    const work = vi.fn()
      .mockResolvedValueOnce({ done: false, error: 'wrong, try again' })
      .mockResolvedValueOnce({ done: true, value: 'second-time' })
    const result = promptRestorePassword<string>(null, work)
    const w = windows[0]!
    emit(BACKUP_PW_SUBMIT, fromWindow(w), 'bad-one', 'bad-one')
    await tick()
    expect(w.webContents.sent.map((a) => a[0])).toEqual([BACKUP_PW_BUSY, BACKUP_PW_ERROR])
    expect(w.webContents.sent[1]![1]).toBe('wrong, try again')
    expect(w.closed).toBe(false)
    emit(BACKUP_PW_SUBMIT, fromWindow(w), 'good-one', 'good-one')
    await expect(result).resolves.toBe('second-time')
  })

  it('ignores other webContents, rejects an empty or multi-line password in main, and cancels to null', async () => {
    const work = vi.fn()
    const result = promptRestorePassword<string>(null, work)
    const w = windows[0]!
    const other = new FakeContents()
    emit(BACKUP_PW_SUBMIT, { sender: other, senderFrame: other.mainFrame }, 'attacker', 'attacker')
    emit(BACKUP_PW_SUBMIT, fromWindow(w), '', '')
    emit(BACKUP_PW_SUBMIT, fromWindow(w), 'a\nb', 'a\nb')
    expect(work).not.toHaveBeenCalled()
    expect(w.webContents.sent.filter((a) => a[0] === BACKUP_PW_ERROR)).toHaveLength(2)
    emit(BACKUP_PW_CANCEL, fromWindow(w))
    await expect(result).resolves.toBeNull()
  })

  it('shows the "will become the default" line only when asked', () => {
    expect(backupPasswordHtml('restore')).not.toContain('will become the default')
    expect(backupPasswordHtml('restore', { willBeDefault: true })).toContain('This identity will become the default identity.')
    setMainLocale('zh')
    expect(backupPasswordHtml('restore', { willBeDefault: true })).toContain('默认身份')
    void promptRestorePassword(null, async () => ({ done: true, value: 1 }), { willBeDefault: true })
    expect(decodeURIComponent(windows.at(-1)!.loadedUrl)).toContain('默认身份')
  })

  it('says the identity MAY become the default when that could not be determined', () => {
    const html = backupPasswordHtml('restore', { mayBeDefault: true })
    expect(html).toContain('This identity may become the default identity.')
    expect(html).not.toContain('will become')
    // a definite "will" wins over "may"
    expect(backupPasswordHtml('restore', { willBeDefault: true, mayBeDefault: true })).toContain('will become the default')
  })

  it('ignores close and cancel while the daemon call is in flight, and says to wait', async () => {
    let release!: () => void
    const result = promptRestorePassword(null, () => new Promise((resolve) => { release = () => resolve({ done: true, value: 'finished' }) }))
    const w = windows.at(-1)!
    emit(BACKUP_PW_SUBMIT, fromWindow(w), 'some-password', 'some-password')
    // the window's own close attempt is vetoed...
    const veto = { prevented: false, preventDefault() { this.prevented = true } }
    w.handlers.get('close')!(veto as never)
    expect(veto.prevented).toBe(true)
    // ...Escape / Cancel are not reported as a cancel...
    emit(BACKUP_PW_CANCEL, fromWindow(w))
    expect(w.closed).toBe(false)
    expect(w.webContents.sent.filter((a) => a[0] === BACKUP_PW_BUSY).map((a) => a[1])).toEqual(
      ['Decrypting…', 'Please wait until this finishes…', 'Please wait until this finishes…'])
    // ...and the real outcome is what comes back
    release()
    await expect(result).resolves.toBe('finished')
  })

  it('lets the window close when nothing is in flight', () => {
    void promptRestorePassword(null, async () => ({ done: true, value: 1 }))
    const w = windows.at(-1)!
    const veto = { prevented: false, preventDefault() { this.prevented = true } }
    w.handlers.get('close')!(veto as never)
    expect(veto.prevented).toBe(false)
  })

  it('measures the restore password in UTF-8 bytes like the daemon', async () => {
    const work = vi.fn()
    const result = promptRestorePassword<string>(null, work)
    const w = windows.at(-1)!
    emit(BACKUP_PW_SUBMIT, fromWindow(w), 'é'.repeat(600), 'é'.repeat(600)) // 600 characters, 1200 bytes
    expect(work).not.toHaveBeenCalled()
    expect(w.webContents.sent.at(-1)![0]).toBe(BACKUP_PW_ERROR)
    emit(BACKUP_PW_CANCEL, fromWindow(w))
    await expect(result).resolves.toBeNull()
  })

  it('closing the window while idle resolves null without calling the work', async () => {
    const work = vi.fn()
    const result = promptRestorePassword<string>(null, work)
    windows[0]!.close()
    await expect(result).resolves.toBeNull()
    expect(work).not.toHaveBeenCalled()
  })
})

describe('backup password preload', () => {
  const source = fs.readFileSync(path.join(__dirname, 'backup-password-preload.ts'), 'utf8')
  it('is self-contained (a sandboxed preload cannot require siblings): only imports electron', () => {
    const imports = [...source.matchAll(/^import .* from '([^']+)'/gm)].map((m) => m[1])
    expect(imports).toEqual(['electron'])
  })
  it('uses exactly the channels the window module listens on', () => {
    expect(source).toContain(`'${BACKUP_PW_SUBMIT}'`)
    expect(source).toContain(`'${BACKUP_PW_CANCEL}'`)
    expect(source).toContain(`'${BACKUP_PW_ERROR}'`)
    expect(source).toContain(`'${BACKUP_PW_BUSY}'`)
    expect(source).toContain("event.key === 'Escape'")
  })
})
