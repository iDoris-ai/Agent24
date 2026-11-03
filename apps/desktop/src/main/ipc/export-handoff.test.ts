// The export IPC's password step is the real promptBackupPassword, not the
// seam in index.test.ts. Electron itself is a fake (vitest has no window
// server). This covers the handoff: the password window closes, then the
// save dialog opens on that same parent.

import http from 'node:http'
import os from 'node:os'
import type { AddressInfo } from 'node:net'
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'

const h = vi.hoisted(() => {
  type Listener = (event: unknown, ...args: unknown[]) => void
  const trace: string[] = []
  const ipcListeners = new Map<string, Set<Listener>>()
  const handlers = new Map<string, (event: unknown, ...args: unknown[]) => unknown>()
  const showMessageBox = vi.fn()
  const showSaveDialog = vi.fn()
  const endpoint = { port: 0, token: 'daemon-secret' }

  class Emitter {
    listeners = new Map<string, Array<(...args: unknown[]) => void>>()
    on(event: string, fn: (...args: unknown[]) => void): this {
      this.listeners.set(event, [...(this.listeners.get(event) ?? []), fn])
      return this
    }
    once(event: string, fn: (...args: unknown[]) => void): this { return this.on(event, fn) }
    removeListener(event: string, fn: (...args: unknown[]) => void): this {
      this.listeners.set(event, (this.listeners.get(event) ?? []).filter((f) => f !== fn))
      return this
    }
    emit(event: string, ...args: unknown[]): void {
      for (const fn of [...(this.listeners.get(event) ?? [])]) fn(...args)
    }
  }

  class FakeContents extends Emitter {
    mainFrame = { id: 'password-main' }
    destroyed = false
    isDestroyed(): boolean { return this.destroyed }
    send(): void {}
    setWindowOpenHandler = vi.fn()
  }

  class FakeWindow {
    static options: Record<string, unknown>[] = []
    webContents = new FakeContents()
    closed = false
    listeners = new Map<string, Array<(...args: unknown[]) => void>>()
    constructor(options: Record<string, unknown>) {
      FakeWindow.options.push(options)
      windows.push(this)
    }
    on(event: string, fn: (...args: unknown[]) => void): void {
      this.listeners.set(event, [...(this.listeners.get(event) ?? []), fn])
    }
    once(event: string, fn: (...args: unknown[]) => void): void { this.on(event, fn) }
    removeListener(event: string, fn: (...args: unknown[]) => void): void {
      this.listeners.set(event, (this.listeners.get(event) ?? []).filter((f) => f !== fn))
    }
    show(): void {}
    isDestroyed(): boolean { return this.closed }
    close(): void {
      this.closed = true
      trace.push('password-closed')
      for (const fn of [...(this.listeners.get('closed') ?? [])]) fn()
    }
    destroy(): void { this.close() }
    loadURL(): Promise<void> { return Promise.resolve() }
  }

  const windows: FakeWindow[] = []

  class FakeParent extends Emitter {
    enabled = true
    visible = true
    minimized = false
    isDestroyed(): boolean { return false }
    isEnabled(): boolean { return this.enabled }
    isVisible(): boolean { return this.visible }
    isMinimized(): boolean { return this.minimized }
    setEnabled(value: boolean): void { this.enabled = value }
    show(): void { this.visible = true }
    restore(): void { this.minimized = false }
    focus(): void { trace.push('parent-focus') }
  }

  const fakeApp = Object.assign(new Emitter(), {
    isPackaged: true,
    getVersion: () => '0.0.0-test',
    getPath: () => os.tmpdir(),
    focus: (opts?: { steal?: boolean }) => { trace.push(opts?.steal ? 'app-focus-steal' : 'app-focus') },
  })

  let parent: FakeParent | null = null
  return {
    trace, ipcListeners, handlers, showMessageBox, showSaveDialog, endpoint, windows, FakeWindow, FakeParent, fakeApp,
    get parent(): FakeParent | null { return parent },
    set parent(value: FakeParent | null) { parent = value },
  }
})

const { trace, ipcListeners, handlers, showMessageBox, showSaveDialog, endpoint, windows, FakeWindow, FakeParent, fakeApp } = h

vi.mock('electron', () => ({
  app: h.fakeApp,
  BrowserWindow: Object.assign(h.FakeWindow, { fromWebContents: () => h.parent }),
  dialog: {
    showMessageBox: (...args: unknown[]) => h.showMessageBox(...args),
    showSaveDialog: (...args: unknown[]) => h.showSaveDialog(...args),
    showOpenDialog: vi.fn(),
  },
  ipcMain: {
    handle: (channel: string, fn: (event: unknown, ...args: unknown[]) => unknown) => { h.handlers.set(channel, fn) },
    on: (channel: string, fn: (event: unknown, ...args: unknown[]) => void) => {
      if (!h.ipcListeners.has(channel)) h.ipcListeners.set(channel, new Set())
      h.ipcListeners.get(channel)!.add(fn)
    },
    removeListener: (channel: string, fn: (event: unknown, ...args: unknown[]) => void) => {
      h.ipcListeners.get(channel)?.delete(fn)
    },
  },
  shell: { openExternal: vi.fn(() => Promise.resolve()), showItemInFolder: vi.fn() },
}))

vi.mock('../backend-manager', () => ({
  getBackendEndpoint: () => ({ port: h.endpoint.port }),
  daemonConnection: () => ({ port: h.endpoint.port, authHeaders: { Authorization: `Bearer ${h.endpoint.token}` } }),
  daemonCliEndpoint: () => ({ port: h.endpoint.port, token: h.endpoint.token }),
}))

import { IpcChannels } from '../../shared/ipc-types'
import { BACKUP_PW_SUBMIT } from '../backup-password-window'
import { createIpcGuard } from './guard'
import { registerIpcHandlers } from './index'

const APP_URL = 'file:///app/renderer/index.html'
const mainFrame = { url: APP_URL }
const trustedEvent = { senderFrame: mainFrame, sender: { isDestroyed: () => false, send: vi.fn() } }

let daemon: http.Server

beforeAll(async () => {
  daemon = http.createServer((req, res) => {
    req.resume()
    req.on('end', () => {
      if (req.method === 'GET' && req.url === '/api/v1/comm/identity') {
        res.end(JSON.stringify({ ok: true, data: [{ nickname: 'alice', npub: 'npub1alice', default: true }] }))
        return
      }
      res.statusCode = 404
      res.end('{}')
    })
  })
  await new Promise<void>((resolve) => daemon.listen(0, '127.0.0.1', resolve))
  endpoint.port = (daemon.address() as AddressInfo).port
  registerIpcHandlers({
    omlxSettingsPath: os.tmpdir(),
    guard: createIpcGuard({
      getMainWindow: () => ({ isDestroyed: () => false, webContents: { mainFrame } }) as never,
      isTrustedUrl: (url) => url === APP_URL,
    }),
  })
})

afterAll(() => new Promise<void>((resolve) => daemon.close(() => resolve())))

beforeEach(() => {
  trace.length = 0
  windows.length = 0
  FakeWindow.options.length = 0
  ipcListeners.clear()
  showMessageBox.mockReset()
  showSaveDialog.mockReset()
  h.parent = null
})

const until = async (cond: () => boolean): Promise<void> => {
  for (let i = 0; i < 500 && !cond(); i++) await new Promise((r) => setImmediate(r))
  expect(cond()).toBe(true)
}

describe('export password window to save dialog', () => {
  it('uses the real password window: it closes, then the save sheet opens on that parent', async () => {
    const parent = new FakeParent()
    h.parent = parent
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    showSaveDialog.mockImplementation(async (...args: unknown[]) => {
      trace.push(windows[0]?.isDestroyed() ? 'save-after-close' : 'save-while-open')
      trace.push(args[0] === parent ? 'save-on-parent' : 'save-without-parent')
      trace.push('save-dialog')
      return { canceled: true }
    })
    const invoke = handlers.get(IpcChannels.CommIdentityExport)
    if (!invoke) throw new Error('export handler missing')
    const pending = Promise.resolve().then(() => invoke(trustedEvent, 'alice'))
    await until(() => windows.length === 1)
    const prefs = FakeWindow.options[0]?.['webPreferences'] as Record<string, unknown>
    expect(prefs).toMatchObject({ sandbox: true, contextIsolation: true, nodeIntegration: false })
    expect(FakeWindow.options[0]?.['modal']).toBe(true)
    expect(FakeWindow.options[0]?.['parent']).toBe(parent)
    const win = windows[0]!
    for (const fn of ipcListeners.get(BACKUP_PW_SUBMIT) ?? []) {
      fn({ sender: win.webContents, senderFrame: win.webContents.mainFrame }, 'good-pass-1', 'good-pass-1')
    }
    const result = await pending as { data?: { error?: { code?: string } } }
    expect(result.data?.error?.code).toBe('cancelled')
    expect(trace).not.toContain('save-while-open')
    expect(trace).not.toContain('save-without-parent')
    const closedAt = trace.indexOf('password-closed')
    const saveAt = trace.indexOf('save-dialog')
    expect(closedAt).toBeGreaterThanOrEqual(0)
    expect(saveAt).toBeGreaterThan(closedAt)
    expect(trace.indexOf('save-after-close')).toBeGreaterThan(closedAt)
    expect(trace.indexOf('save-on-parent')).toBeGreaterThan(closedAt)
    expect(win.isDestroyed()).toBe(true)
    expect(showSaveDialog).toHaveBeenCalledTimes(1)

    showMessageBox.mockResolvedValueOnce({ response: 1 })
    const again = await Promise.resolve().then(() => invoke(trustedEvent, 'alice')) as { data?: { error?: { code?: string } } }
    expect(again.data?.error?.code).not.toBe('busy')
    expect(fakeApp.listeners.get('before-quit')?.length).toBe(1)
  })
})
