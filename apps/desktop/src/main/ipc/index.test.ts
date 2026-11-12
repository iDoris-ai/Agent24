// PR-A (pre-release desktop hardening) — handler-level tests for A1/A3/A4/A5.
// Electron is mocked so registerIpcHandlers() can run under plain Node; the
// daemon is a real loopback http server so "no request reached it" is a
// factual assertion, not a mock-call count.

import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import http from 'node:http'
import { EventEmitter } from 'node:events'
import type { AddressInfo } from 'node:net'
import type { ChildProcess } from 'node:child_process'
import { spawn } from 'node:child_process'
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'

const handlers = new Map<string, (event: unknown, ...args: unknown[]) => unknown>()
const showMessageBox = vi.fn()
const showSaveDialog = vi.fn()
const showOpenDialog = vi.fn()
const showItemInFolder = vi.fn()
const exportTimeoutMs = { current: 90_000 }
const promptBackupPassword = vi.fn()
const promptRestorePassword = vi.fn()
const promptUnlockPassword = vi.fn()
const promptHomeImportPassword = vi.fn()
const execFileMock = vi.fn()
/** When set, the execFile mock returns this child instead of a dummy. */
const omlxExecFile: { current: ((...args: unknown[]) => unknown) | null } = { current: null }
/** What registerIpcHandlers' keepOwnedOmlxOnQuit answers (the resident service installed). */
const keepOmlx = { current: false }
/** What registerIpcHandlers' rememberKeptOmlx was told (ruling 乙). */
const keptOmlx: Array<{ pid: number; port: number }> = []
const rememberThrows = { current: false }
/** What registerIpcHandlers' stopKeptOmlx does and how often it was asked (ruling A). */
const keptStop: { calls: number; impl: () => Promise<void> } = { calls: 0, impl: () => Promise.resolve() }
const appListeners = new Map<string, Set<() => void>>()
const appFocus = vi.fn()
function listenApp(event: string, fn: () => void): void {
  let set = appListeners.get(event)
  if (!set) {
    set = new Set()
    appListeners.set(event, set)
  }
  set.add(fn)
}
function unlistenApp(event: string, fn: () => void): void {
  appListeners.get(event)?.delete(fn)
}
function emitApp(event: string): void {
  for (const fn of [...(appListeners.get(event) ?? [])]) fn()
}
// What BrowserWindow.fromWebContents answers: the window the export started from.
type ExportParent = {
  isDestroyed: () => boolean
  isVisible: () => boolean
  isEnabled: () => boolean
  isMinimized: () => boolean
  restore: () => void
  show: () => void
  focus: () => void
  setEnabled: (enabled: boolean) => void
  on: (event: string, fn: () => void) => void
  removeListener: (event: string, fn: () => void) => void
  emit: (event: string) => void
  listenerCount: (event: string) => number
  calls: string[]
}
let parentWindow: ExportParent | null = null
function trackableParent(state: { minimized?: boolean; visible?: boolean; enabled?: boolean; destroyed?: boolean } = {}): ExportParent {
  const listeners = new Map<string, Array<() => void>>()
  let minimized = state.minimized ?? false
  let visible = state.visible ?? true
  let enabled = state.enabled ?? true
  const destroyed = state.destroyed ?? false
  const calls: string[] = []
  return {
    calls,
    isDestroyed: () => destroyed,
    isMinimized: () => minimized,
    isVisible: () => visible,
    isEnabled: () => enabled,
    restore: () => { calls.push('restore'); minimized = false },
    show: () => { calls.push('show'); visible = true },
    setEnabled: (value: boolean) => { calls.push('setEnabled'); enabled = value },
    focus: () => { calls.push('parent.focus') },
    on: (event: string, fn: () => void) => { listeners.set(event, [...(listeners.get(event) ?? []), fn]) },
    removeListener: (event: string, fn: () => void) => {
      listeners.set(event, (listeners.get(event) ?? []).filter((f) => f !== fn))
    },
    emit: (event: string) => { for (const fn of [...(listeners.get(event) ?? [])]) fn() },
    listenerCount: (event: string) => (listeners.get(event) ?? []).length,
  }
}
// When set, the daemon's export route never answers until these are ended.
let hangExport = false
let exportReportsPath: string | null = null
const hungExports: http.ServerResponse[] = []
// T1-2: the documents module as main sees it, and its two actions.
let docStatus = { bundled: true, installed: true, externalDaemon: false }
// What else GET /api/v1/os lists (documents, for the uninstall checks).
let extraOsModules: { name: string; enabled: boolean; version: string }[] = []
const docUninstall = vi.fn()
const docInstall = vi.fn()

vi.mock('electron', () => ({
  app: {
    getVersion: () => '0.0.0-test',
    getPath: () => settingsDir,
    on: (event: string, fn: () => void) => { listenApp(event, fn) },
    removeListener: (event: string, fn: () => void) => { unlistenApp(event, fn) },
    focus: (opts?: { steal: boolean }) => { appFocus(opts) },
  },
  BrowserWindow: { fromWebContents: () => parentWindow },
  dialog: { showMessageBox: (...a: unknown[]) => showMessageBox(...a), showOpenDialog: (...a: unknown[]) => showOpenDialog(...a), showSaveDialog: (...a: unknown[]) => showSaveDialog(...a) },
  ipcMain: { handle: (channel: string, fn: (event: unknown, ...args: unknown[]) => unknown) => { handlers.set(channel, fn) } },
  shell: { openExternal: vi.fn(() => Promise.resolve()), showItemInFolder: (...a: unknown[]) => showItemInFolder(...a) },
}))

vi.mock('node:child_process', async (orig) => ({
  ...(await orig<typeof import('node:child_process')>()),
  execFile: (...a: unknown[]) => {
    execFileMock(...a)
    // Only the omlx spawn is replaced. A `pkill` call is recorded and not executed,
    // so the suite cannot signal a server the user started themselves.
    if (a[0] === 'omlx' && omlxExecFile.current) return omlxExecFile.current(...a)
    return { on: vi.fn(), kill: vi.fn() }
  },
}))

const endpoint = { port: 0, token: 'daemon-secret' }
type Elevate = (r: { method: string; path: string; body: string; summary: string }) => Promise<string>
const elevation: { current: ((fn: (elevate: Elevate, token: string) => Promise<unknown>) => Promise<unknown>) | null } = { current: null }
vi.mock('../backend-manager', () => ({
  getBackendEndpoint: () => ({ port: endpoint.port }),
  daemonConnection: () => ({ port: endpoint.port, authHeaders: { Authorization: `Bearer ${endpoint.token}` } }),
  daemonCliEndpoint: () => ({ port: endpoint.port, token: endpoint.token }),
  daemonElevation: () => elevation.current,
}))

import { IpcChannels } from '../../shared/ipc-types'
import { createIpcGuard } from './guard'
import { ensureOwnedOmlxQuit, OMLX_QUIT_STOP_BUDGET_MS, reachesHostInterface, reachesPresenceChannel, registerIpcHandlers, resetLastCommExportForTests, resetOmlxProcessForTests, setOmlxKillFallbackMsForTests, setOmlxStopGraceMsForTests, stopOwnedOmlx } from './index'

const APP_URL = 'file:///app/renderer/index.html'
const mainFrame = { url: APP_URL }
const mainWindow = { isDestroyed: () => false, webContents: { mainFrame } }
const trustedEvent = { senderFrame: mainFrame, sender: { isDestroyed: () => false, send: vi.fn() } }

let daemon: http.Server
let modelsRequireKey: string | null = null
let reflectAuthAsModel = false
let reflectedKeyEncoding: BufferEncoding | 'base64url' | null = null
const settingsDir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-omlx-settings-'))
const settingsFile = path.join(settingsDir, 'settings.json')
let seen: { method?: string; url?: string; auth?: string; body: string }[] = []
// A-3: what GET /api/v1/attached lists, and an optional forced failure for
// PATCH/DELETE /api/v1/attached/agentear.
let attachedModules: { name: string; attach_status: string }[] = []
let attachedFailure: { status: number; code: string } | null = null
// does `GET /api/v1/comm/identity` report a default identity?
let identityListHasDefault = true
let identityListFails = false
// what GET /api/v1/comm/relay answers
let currentRelays: { relays: string[]; configured: boolean } = { relays: [], configured: false }
// M5 B3: POST /api/v1/schedules needs a grant, as an Elevated route does for a daily token.
let presenceGate = false
let elevationHeaders: (string | undefined)[] = []

beforeAll(async () => {
  daemon = http.createServer((req, res) => {
    const chunks: Buffer[] = []
    req.on('data', (c: Buffer) => chunks.push(c))
    req.on('end', () => {
      if (!(req.method === 'GET' && req.url === '/api/v1/comm/identity') && !(req.method === 'GET' && req.url?.startsWith('/api/v1/approvals/')) && !(req.method === 'GET' && req.url === '/api/v1/os') && !(req.method === 'GET' && req.url === '/api/v1/attached') && !(req.method === 'GET' && req.url === '/api/v1/comm/relay')) seen.push({ method: req.method, url: req.url, auth: req.headers.authorization, body: Buffer.concat(chunks).toString() })
      res.setHeader('Content-Type', 'application/json')
      if (presenceGate && req.method === 'POST' && (req.url === '/api/v1/schedules' || req.url?.startsWith('/api/v1/approvals/'))) {
        const grant = req.headers['x-agent24-elevation']
        elevationHeaders.push(typeof grant === 'string' ? grant : undefined)
        if (grant !== 'grant-1') {
          res.statusCode = 403
          res.end(JSON.stringify({ error: { code: 'presence_required', message: 'Presence confirmation required' } }))
        } else res.end(JSON.stringify({ id: 'sch_1' }))
      } else if (req.url === '/v1/models' && modelsRequireKey && req.headers.authorization !== `Bearer ${modelsRequireKey}`) {
        res.statusCode = 401
        res.end(JSON.stringify({ error: 'unauthorized' }))
      } else if (req.url === '/v1/models' && reflectAuthAsModel) {
        const key = String(req.headers.authorization ?? '').replace(/^Bearer /, '')
        const reflected = reflectedKeyEncoding ? Buffer.from(key).toString(reflectedKeyEncoding) : String(req.headers.authorization ?? '')
        res.end(JSON.stringify({ data: [{ id: `echo-${reflected}-model` }, { id: 'm1' }, { id: 7 }] }))
      } else if (req.url === '/v1/models') res.end(JSON.stringify({ data: [{ id: 'm1' }] }))
      else if (req.method === 'GET' && req.url === '/api/v1/approvals/ap_1') {
        res.end(JSON.stringify({
          id: 'ap_1', kind: 'exec', summary: 'REAL: rm -rf /tmp/real', run_id: 'run_real',
          payload: { argv: ['rm', '-rf', '/tmp/real'] }, status: 'pending',
          available_decisions: ['approve', 'approve_for_session', 'deny', 'abort'],
        }))
      } else if (req.method === 'GET' && req.url === '/api/v1/approvals/ap_tgt') {
        res.end(JSON.stringify({
          id: 'ap_tgt', kind: 'network', summary: 'external tool', run_id: 'r', payload: {}, status: 'pending',
          available_decisions: ['approve', 'approve_for_target', 'deny', 'abort'],
        }))
      } else if (req.method === 'GET' && req.url === '/api/v1/approvals/ap_long') {
        res.end(JSON.stringify({
          id: 'ap_long', kind: 'exec', summary: 'S'.repeat(500), run_id: 'r',
          payload: { cmd: 'P'.repeat(2000) }, status: 'pending',
        }))
      } else if (req.method === 'GET' && req.url === '/api/v1/approvals/ap_done') {
        res.end(JSON.stringify({ id: 'ap_done', kind: 'exec', summary: 's', status: 'approved' }))
      } else if (req.method === 'GET' && req.url?.startsWith('/api/v1/approvals/ap_missing')) {
        res.statusCode = 404
        res.end(JSON.stringify({ error: { code: 'not_found', message: 'nope' } }))
      } else if (req.method === 'POST' && req.url === '/api/v1/comm/identity/import') {
        const sent = Buffer.concat(chunks).toString()
        if (sent.includes('wrong-pass')) {
          res.statusCode = 400
          res.end(JSON.stringify({ ok: false, error: 'bad_backup_password', message: 'nope' }))
        } else if (sent.includes('bad-input-pass')) {
          res.statusCode = 400
          res.end(JSON.stringify({ ok: false, error: 'invalid', message: 'path must be absolute' }))
        } else if (sent.includes('dupe-nick')) {
          res.statusCode = 409
          res.end(JSON.stringify({ ok: false, error: 'nickname_exists', message: 'exists' }))
        } else res.end(JSON.stringify({ ok: true, data: { status: 'imported', nickname: 'r1', npub: 'npub1x', default: false } }))
      } else if (req.method === 'GET' && req.url === '/api/v1/skills/demo') {
        res.end(JSON.stringify({
          skill_id: 'demo', generation: 2, text_consistency: null, activations: [],
          pointer: { origin: 'imported', body_revision_id: 'b1', evals_revision_id: 'e1', generation: 2 },
          revisions: [
            { revision_id: 'b1', origin: 'imported', semver: '1.0.0', kind: 'body', content_digest: 'aaaaaaaa11', created_at: 't' },
            { revision_id: 'e1', origin: 'imported', semver: '1.0.0', kind: 'evals', content_digest: 'bbbbbbbb22', created_at: 't' },
            { revision_id: 'b2', origin: 'imported', semver: '2.0.0', kind: 'body', content_digest: 'cafef00d99', created_at: 't' },
          ],
        }))
      } else if (req.method === 'POST' && req.url === '/api/v1/skills/demo/activate') {
        res.end(JSON.stringify({ result: 'activated' }))
      } else if (req.method === 'GET' && req.url === '/api/v1/comm/relay') {
        res.end(JSON.stringify({ ok: true, data: { relays: currentRelays.relays, source: 'config', configured: currentRelays.configured } }))
      } else if (req.method === 'PUT' && req.url === '/api/v1/comm/relay') {
        res.end(JSON.stringify({ ok: true, data: { relays: JSON.parse(Buffer.concat(chunks).toString()).relays, source: 'config', configured: true } }))
      } else if (req.method === 'POST' && req.url === '/api/v1/comm/unlock') {
        if (Buffer.concat(chunks).toString().includes('wrong-unlock')) {
          res.statusCode = 423
          res.end(JSON.stringify({ ok: false, error: 'locked', message: 'password verification failed' }))
        } else res.end(JSON.stringify({ ok: true, data: { unlocked: true, remembered: false } }))
      } else if (req.method === 'POST' && req.url === '/api/v1/comm/import') {
        if (Buffer.concat(chunks).toString().includes('wrong-home')) {
          res.statusCode = 423
          res.end(JSON.stringify({ ok: false, error: 'locked', message: 'incorrect password' }))
        } else res.end(JSON.stringify({ ok: true, data: { identities: 1, contacts: 2, outbox: 0, db_files: [] } }))
      } else if (req.method === 'GET' && req.url === '/api/v1/comm/identity' && identityListFails) {
        res.statusCode = 503
        res.end(JSON.stringify({ ok: false, error: 'unavailable', message: 'down' }))
      } else if (req.method === 'GET' && req.url === '/api/v1/comm/identity') {
        res.end(JSON.stringify({ ok: true, data: [{ nickname: 'alice', npub: 'npub1alice', default: identityListHasDefault, encrypted: true }] }))
      } else if (req.method === 'GET' && req.url === '/api/v1/attached') {
        res.end(JSON.stringify({ modules: attachedModules }))
      } else if (req.url?.startsWith('/api/v1/attached/') && attachedFailure) {
        res.statusCode = attachedFailure.status
        res.end(JSON.stringify({ error: { code: attachedFailure.code, message: 'daemon says no' } }))
      } else if (req.method === 'DELETE' && req.url?.startsWith('/api/v1/attached/')) {
        res.statusCode = 204
        res.end()
      } else if (req.method === 'PATCH' && req.url?.startsWith('/api/v1/attached/')) {
        res.end(JSON.stringify({ modules: attachedModules }))
      } else if (req.method === 'GET' && req.url === '/api/v1/os') {
        res.end(JSON.stringify({ modules: [{ name: 'sin90', enabled: false, version: '0.5.0' }, ...extraOsModules] }))
      } else if (req.method === 'POST' && req.url === '/api/v1/comm/identity/export' && hangExport) {
        hungExports.push(res)
      } else if (req.method === 'POST' && req.url === '/api/v1/comm/identity/export') {
        const body = JSON.parse(Buffer.concat(chunks).toString()) as { nickname?: string; path?: string }
        res.end(JSON.stringify({ ok: true, data: { nickname: body.nickname, npub: 'npub1alice', path: exportReportsPath ?? body.path, format: 'ncryptsec' } }))
      } else res.end(JSON.stringify({ ok: true, echoed: req.url }))
    })
  })
  await new Promise<void>((r) => daemon.listen(0, '127.0.0.1', r))
  endpoint.port = (daemon.address() as AddressInfo).port
  registerIpcHandlers({
    omlxSettingsPath: settingsFile,
    omlxCandidatePorts: [endpoint.port],
    promptBackupPassword: (...a: unknown[]) => promptBackupPassword(...a),
    promptRestorePassword: (...a: unknown[]) => promptRestorePassword(...a) as never,
    promptUnlockPassword: (...a: unknown[]) => promptUnlockPassword(...a) as never,
    promptHomeImportPassword: (...a: unknown[]) => promptHomeImportPassword(...a) as never,
    exportTimeoutMs,
    keepOwnedOmlxOnQuit: () => keepOmlx.current,
    rememberKeptOmlx: (k: { pid: number; port: number }) => {
      if (rememberThrows.current) throw new Error('disk full')
      keptOmlx.push(k)
    },
    stopKeptOmlx: () => { keptStop.calls++; return keptStop.impl() },
    documentsModule: () => docStatus,
    documentsModuleActions: { uninstall: () => docUninstall(), install: () => docInstall() },
    guard: createIpcGuard({
      getMainWindow: () => mainWindow as never,
      isTrustedUrl: (u) => u === APP_URL,
    }),
  })
})

afterAll(() => {
  fs.rmSync(settingsDir, { recursive: true, force: true })
  return new Promise<void>((r) => daemon.close(() => r()))
})

beforeEach(() => {
  presenceGate = false
  elevationHeaders = []
  elevation.current = null
  modelsRequireKey = null
  reflectAuthAsModel = false
  reflectedKeyEncoding = null
  fs.rmSync(settingsFile, { force: true })
  fs.rmSync(path.join(settingsDir, 'ui-prefs.json'), { force: true })
  seen = []
  attachedModules = [{ name: 'agentear', attach_status: 'attached' }, { name: 'other', attach_status: 'attached' }]
  attachedFailure = null
  extraOsModules = []
  identityListHasDefault = true
  identityListFails = false
  showMessageBox.mockReset()
  showSaveDialog.mockReset()
  promptBackupPassword.mockReset()
  promptRestorePassword.mockReset()
  promptUnlockPassword.mockReset()
  promptHomeImportPassword.mockReset()
  currentRelays = { relays: ['wss://old.example'], configured: true }
  showOpenDialog.mockReset()
  showItemInFolder.mockReset()
  exportTimeoutMs.current = 90_000
  parentWindow = null
  appFocus.mockReset()
  hangExport = false
  for (const r of hungExports.splice(0)) r.end(JSON.stringify({ ok: true, data: {} }))
  resetLastCommExportForTests()
  execFileMock.mockReset()
  omlxExecFile.current = null
  keepOmlx.current = false
  keptOmlx.length = 0
  rememberThrows.current = false
  keptStop.calls = 0
  keptStop.impl = () => Promise.resolve()
  resetOmlxProcessForTests()
})

// eslint-disable-next-line @typescript-eslint/no-explicit-any -- tests poke at several result shapes
function call(channel: string, ...args: unknown[]): Promise<any> {
  const h = handlers.get(channel)
  if (!h) throw new Error(`no handler for ${channel}`)
  return Promise.resolve().then(() => h(trustedEvent, ...args))
}

describe('A1 backendProxy allowlist', () => {
  const blocked: [string, string][] = [
    ['POST', '/api/v1/approvals/abc'],
    ['POST', '/api/v1/module-approvals/abc'],
    ['POST', '/api/v1/shutdown'],
    ['POST', '/api/v1/comm/install'],
    ['PUT', '/api/v1/tool-overrides/shell'],
    ['DELETE', '/api/v1/tool-overrides/shell'],
    ['DELETE', '/api/v1/standing-grants/g1'],
    ['POST', '/api/v1/capabilities/creative'],
    ['POST', '/api/v1/capabilities/c1/revoke'],
    ['POST', '/api/v1/attached'],
    ['PATCH', '/api/v1/attached/agentear'],
    ['DELETE', '/api/v1/attached/agentear'],
    ['PATCH', '/api/v1/os/sin90'],
    ['POST', '/api/v1/os/sin90/stop'],
    ['GET', '/api/v1/not-a-route'],
    ['POST', '/api/modules'],
    ['GET', '/api/modules/install'],
    ['GET', '/api/v1/runs/../shutdown'],
    ['GET', '/api/v1/runs/%2e%2e/shutdown'],
  ]
  it.each(blocked)('%s %s -> forbidden, never reaches the daemon', async (method, path) => {
    const res = await call(IpcChannels.BackendProxy, { method, path })
    expect(res.ok).toBe(false)
    expect(res.status).toBe(403)
    expect(res.data.error.code).toBe('forbidden')
    expect(seen).toHaveLength(0)
  })

  it('service mode: an elevated write is confirmed on the enrolment once, then retried with the grant (M5 B3)', async () => {
    presenceGate = true
    const asked: { method: string; path: string; body: string }[] = []
    elevation.current = (fn) => fn(async (r) => { asked.push({ method: r.method, path: r.path, body: r.body }); return 'grant-1' }, endpoint.token)
    const body = { name: 'daily', cron: '0 9 * * *' }
    const res = await call(IpcChannels.BackendProxy, { method: 'POST', path: '/api/v1/schedules', body })
    expect(res).toMatchObject({ ok: true, status: 200, data: { id: 'sch_1' } })
    expect(asked).toEqual([{ method: 'POST', path: '/api/v1/schedules', body: JSON.stringify(body) }])
    expect(elevationHeaders).toEqual([undefined, undefined, 'grant-1'])
    expect(seen.map((s) => s.body)).toEqual([JSON.stringify(body), JSON.stringify(body), JSON.stringify(body)])
  })

  it('with our own child (no enrolment) the 403 comes back as it is and nothing is retried (M5 B3)', async () => {
    presenceGate = true
    const res = await call(IpcChannels.BackendProxy, { method: 'POST', path: '/api/v1/schedules', body: { name: 'x' } })
    expect(res).toMatchObject({ ok: false, status: 403, data: { error: { code: 'presence_required' } } })
    expect(elevationHeaders).toEqual([undefined])
  })

  it('forwards an allowed read with the daemon bearer token', async () => {
    const res = await call(IpcChannels.BackendProxy, { method: 'GET', path: '/api/v1/runs/r1?x=1' })
    expect(res.ok).toBe(true)
    expect(seen).toHaveLength(1)
    expect(seen[0]?.auth).toBe('Bearer daemon-secret')
    expect(seen[0]?.url).toBe('/api/v1/runs/r1?x=1')
  })

  describe('ApprovalDecide (dedicated IPC + native confirm)', () => {
    it('sends the decision only after the user confirms', async () => {
      showMessageBox.mockResolvedValue({ response: 0 })
      const res = await call(IpcChannels.ApprovalDecide, 'ap_1', { type: 'approve' })
      expect(showMessageBox).toHaveBeenCalledTimes(1)
      expect(res.ok).toBe(true)
      expect(seen).toHaveLength(1)
      expect(seen[0]?.method).toBe('POST')
      expect(seen[0]?.url).toBe('/api/v1/approvals/ap_1')
      expect(JSON.parse(seen[0]!.body)).toEqual({ type: 'approve' })
    })
    it('service mode: one box, the daemon\'s presence box; its own is skipped (M5 B3, General 11-09)', async () => {
      presenceGate = true
      const presenceBoxes: string[] = []
      elevation.current = (fn) => fn(async (r) => { presenceBoxes.push(`${r.method} ${r.path}`); return 'grant-1' }, endpoint.token)
      const res = await call(IpcChannels.ApprovalDecide, 'ap_1', { type: 'approve' })
      expect(showMessageBox).not.toHaveBeenCalled()
      expect(presenceBoxes).toEqual(['POST /api/v1/approvals/ap_1'])
      expect(res.ok).toBe(true)
      expect(elevationHeaders).toEqual([undefined, undefined, 'grant-1'])
    })
    it('service mode, the presence box refused: nothing approved, still no box of its own', async () => {
      presenceGate = true
      elevation.current = (fn) => fn(async () => { throw new Error('denied') }, endpoint.token)
      const res = await call(IpcChannels.ApprovalDecide, 'ap_1', { type: 'approve' })
      expect(showMessageBox).not.toHaveBeenCalled()
      expect(res).toMatchObject({ ok: false, status: 403 })
      expect(elevationHeaders).toEqual([undefined, undefined])
    })
    it('service mode, then our own child published before the turn: no box, nothing approved (Codex B3b r2 C1)', async () => {
      presenceGate = true
      const presenceBoxes: string[] = []
      // The turn is the old enrolment's; the connection now carries another token.
      elevation.current = (fn) => fn(async (r) => { presenceBoxes.push(r.path); return 'grant-1' }, 'old-daily-token')
      const res = await call(IpcChannels.ApprovalDecide, 'ap_1', { type: 'approve' })
      expect(showMessageBox).not.toHaveBeenCalled()
      expect(presenceBoxes).toEqual([])
      expect(res).toMatchObject({ ok: false, status: 503 })
      expect(elevationHeaders).toEqual([undefined])
    })
    it('our own child: one box, its own, and no presence asked', async () => {
      showMessageBox.mockResolvedValue({ response: 0 })
      const res = await call(IpcChannels.ApprovalDecide, 'ap_1', { type: 'approve' })
      expect(showMessageBox).toHaveBeenCalledTimes(1)
      expect(res.ok).toBe(true)
      expect(elevation.current).toBeNull()
    })
    it('sends nothing when the user cancels', async () => {
      showMessageBox.mockResolvedValue({ response: 1 })
      const res = await call(IpcChannels.ApprovalDecide, 'ap_1', { type: 'approve' })
      expect(res.ok).toBe(false)
      expect(res.data.error.code).toBe('cancelled')
      expect(seen).toHaveLength(0)
    })
    it('the dialog shows the daemon record, not page-supplied text; page reason is only a note', async () => {
      showMessageBox.mockResolvedValue({ response: 0 })
      await call(IpcChannels.ApprovalDecide, 'ap_1', { type: 'approve', reason: 'harmless readme tweak' })
      const opts = showMessageBox.mock.calls[0]!.at(-1) as { message: string; detail: string }
      expect(opts.message).toContain('ap_1')
      expect(opts.detail).toContain('REAL: rm -rf /tmp/real')
      expect(opts.detail).toContain('run_real')
      expect(opts.detail).toContain('exec')
      expect(opts.detail).toContain('/tmp/real')
      expect(opts.detail).toContain('Note from the page (for reference only): harmless readme tweak')
    })
    it('approve_for_target (offered by the daemon) goes through: one dialog, one POST', async () => {
      showMessageBox.mockResolvedValue({ response: 0 })
      const res = await call(IpcChannels.ApprovalDecide, 'ap_tgt', { type: 'approve_for_target' })
      expect(res.ok).toBe(true)
      expect(showMessageBox).toHaveBeenCalledTimes(1)
      expect(seen).toHaveLength(1)
      expect(JSON.parse(seen[0]!.body)).toEqual({ type: 'approve_for_target' })
    })
    it('a decision the record does not offer is refused before any dialog', async () => {
      const res = await call(IpcChannels.ApprovalDecide, 'ap_1', { type: 'approve_for_target' })
      expect(res.data.error.code).toBe('invalid_request')
      expect(showMessageBox).not.toHaveBeenCalled()
      expect(seen).toHaveLength(0)
    })
    it('long fields are cut with an explicit truncation note giving the full length', async () => {
      showMessageBox.mockResolvedValue({ response: 1 })
      await call(IpcChannels.ApprovalDecide, 'ap_long', { type: 'approve' })
      const opts = showMessageBox.mock.calls[0]!.at(-1) as { detail: string }
      expect(opts.detail).toContain('(truncated, 500 characters in total)')
      expect(opts.detail).toMatch(/\(truncated, \d{4} characters in total\)/)
    })
    it('daemon 404 -> refused without any dialog or POST', async () => {
      const res = await call(IpcChannels.ApprovalDecide, 'ap_missing', { type: 'approve' })
      expect(res.ok).toBe(false)
      expect(res.data.error.code).toBe('approval_unavailable')
      expect(showMessageBox).not.toHaveBeenCalled()
      expect(seen).toHaveLength(0)
    })
    it('an already-resolved approval is refused without a dialog (409)', async () => {
      const res = await call(IpcChannels.ApprovalDecide, 'ap_done', { type: 'approve' })
      expect(res.status).toBe(409)
      expect(showMessageBox).not.toHaveBeenCalled()
    })
    it('rejects malformed ids/decisions before even asking', async () => {
      for (const [id, d] of [['../shutdown', { type: 'approve' }], ['ap_1', { type: 'nuke' }], ['ap_1', null], [7, { type: 'approve' }]]) {
        const res = await call(IpcChannels.ApprovalDecide, id, d)
        expect(res.ok).toBe(false)
        expect(res.data.error.code).toBe('invalid_request')
      }
      expect(showMessageBox).not.toHaveBeenCalled()
      expect(seen).toHaveLength(0)
    })
  })

  describe('T1-2: OsModuleUninstall / OsModuleInstallBundled (native confirm, then the CLI)', () => {
    beforeEach(() => {
      docStatus = { bundled: true, installed: true, externalDaemon: false }
      extraOsModules = [{ name: 'documents', enabled: true, version: '0.2.0' }]
      docUninstall.mockReset().mockResolvedValue({ ok: true })
      docInstall.mockReset().mockResolvedValue({ ok: true })
    })
    it('uninstall: confirmed → runs, ok', async () => {
      showMessageBox.mockResolvedValue({ response: 0 })
      const res = await call(IpcChannels.OsModuleUninstall)
      expect(res.ok).toBe(true)
      expect(docUninstall).toHaveBeenCalledOnce()
      const opts = showMessageBox.mock.calls[0]!.at(-1) as { message: string; detail: string }
      expect(opts.message).toContain('documents')
      expect(opts.detail).toContain('Imported documents stay')
    })
    it('uninstall: cancelled → nothing runs', async () => {
      showMessageBox.mockResolvedValue({ response: 1 })
      const res = await call(IpcChannels.OsModuleUninstall)
      expect(res.data.error.code).toBe('cancelled')
      expect(docUninstall).not.toHaveBeenCalled()
    })
    it('uninstall: not installed → refused without a dialog', async () => {
      docStatus = { ...docStatus, installed: false }
      const res = await call(IpcChannels.OsModuleUninstall)
      expect(res.data.error.code).toBe('not_installed')
      expect(showMessageBox).not.toHaveBeenCalled()
      expect(docUninstall).not.toHaveBeenCalled()
    })
    it("uninstall: the CLI's failure code and message are passed through", async () => {
      showMessageBox.mockResolvedValue({ response: 0 })
      docUninstall.mockResolvedValue({ ok: false, code: 'cli_failed', message: 'agent24 os uninstall documents (rc=2): store locked' })
      const res = await call(IpcChannels.OsModuleUninstall)
      expect(res.ok).toBe(false)
      expect(res.data.error.code).toBe('cli_failed')
      expect(res.data.error.message).toContain('(rc=2): store locked')
    })
    it('uninstall: turned off in the daemon → refused without a dialog (os.json would name a missing module)', async () => {
      extraOsModules = [{ name: 'documents', enabled: false, version: '0.2.0' }]
      const res = await call(IpcChannels.OsModuleUninstall)
      expect(res.data.error.code).toBe('module_disabled')
      expect(res.data.error.message).toContain('Turn it on first')
      expect(showMessageBox).not.toHaveBeenCalled()
      expect(docUninstall).not.toHaveBeenCalled()
    })
    it('status: os.json turned documents off → turnedOff (the page says "turned off", not the raw module_stopping)', async () => {
      extraOsModules = [{ name: 'documents', enabled: false, version: '0.2.0' }]
      expect(await call(IpcChannels.DocumentsModuleStatus)).toEqual({ ...docStatus, turnedOff: true })
      extraOsModules = [{ name: 'documents', enabled: true, version: '0.2.0' }]
      expect(await call(IpcChannels.DocumentsModuleStatus)).toEqual({ ...docStatus, turnedOff: false })
    })
    it('status: the daemon does not list documents → no turnedOff, only what is on disk', async () => {
      extraOsModules = []
      expect(await call(IpcChannels.DocumentsModuleStatus)).toEqual(docStatus)
    })
    it('uninstall: the daemon does not serve documents → refused without a dialog', async () => {
      extraOsModules = []
      const res = await call(IpcChannels.OsModuleUninstall)
      expect(res.data.error.code).toBe('module_unavailable')
      expect(showMessageBox).not.toHaveBeenCalled()
      expect(docUninstall).not.toHaveBeenCalled()
    })
    it('turning documents off after it was uninstalled is refused (os.json would name a missing module)', async () => {
      docStatus = { ...docStatus, installed: false }
      extraOsModules = [{ name: 'documents', enabled: true, version: '0.2.0' }]
      const res = await call(IpcChannels.OsModuleSetEnabled, 'documents', false)
      expect(res.data.error.code).toBe('not_installed')
      expect(showMessageBox).not.toHaveBeenCalled()
      expect(seen.some((r) => r.method === 'PATCH')).toBe(false)
      // Turning it on, or any other module off, is unaffected.
      showMessageBox.mockResolvedValue({ response: 0 })
      expect((await call(IpcChannels.OsModuleSetEnabled, 'sin90', false)).ok).toBe(true)
    })
    it('a CLI failure with an external daemon says how to stop it', async () => {
      docStatus = { ...docStatus, externalDaemon: true }
      showMessageBox.mockResolvedValue({ response: 0 })
      docUninstall.mockResolvedValue({ ok: false, code: 'cli_failed', message: 'busy: daemon.singleton.lock is held' })
      const res = await call(IpcChannels.OsModuleUninstall)
      expect(res.data.error.message).toContain('busy: daemon.singleton.lock is held')
      expect(res.data.error.message).toContain('agent24 daemon stop')
    })
    it('install: confirmed → runs, ok', async () => {
      docStatus = { ...docStatus, installed: false }
      showMessageBox.mockResolvedValue({ response: 0 })
      const res = await call(IpcChannels.OsModuleInstallBundled)
      expect(res.ok).toBe(true)
      expect(docInstall).toHaveBeenCalledOnce()
    })
    it('install: cancelled → nothing runs', async () => {
      docStatus = { ...docStatus, installed: false }
      showMessageBox.mockResolvedValue({ response: 1 })
      expect((await call(IpcChannels.OsModuleInstallBundled)).data.error.code).toBe('cancelled')
      expect(docInstall).not.toHaveBeenCalled()
    })
    it('install: already installed, or no bundle → refused without a dialog', async () => {
      expect((await call(IpcChannels.OsModuleInstallBundled)).data.error.code).toBe('already_installed')
      docStatus = { bundled: false, installed: false, externalDaemon: false }
      expect((await call(IpcChannels.OsModuleInstallBundled)).data.error.code).toBe('no_bundle')
      expect(showMessageBox).not.toHaveBeenCalled()
      expect(docInstall).not.toHaveBeenCalled()
    })
    it("install: a signature failure's code is passed through", async () => {
      docStatus = { ...docStatus, installed: false }
      showMessageBox.mockResolvedValue({ response: 0 })
      docInstall.mockResolvedValue({ ok: false, code: 'signature_invalid', message: 'a sealed resource is missing or invalid' })
      const res = await call(IpcChannels.OsModuleInstallBundled)
      expect(res.data.error.code).toBe('signature_invalid')
      expect(res.data.error.message).toContain('a sealed resource is missing or invalid')
    })
  })

  describe('OsModuleSetEnabled (dedicated IPC + native confirm)', () => {
    it('PATCHes /api/v1/os/{name} after confirmation', async () => {
      showMessageBox.mockResolvedValue({ response: 0 })
      const res = await call(IpcChannels.OsModuleSetEnabled, 'sin90', true)
      expect(res.ok).toBe(true)
      expect(seen[0]?.method).toBe('PATCH')
      expect(seen[0]?.url).toBe('/api/v1/os/sin90')
      expect(JSON.parse(seen[0]!.body)).toEqual({ enabled: true })
    })
    it('the dialog names the module and the state it becomes, from the daemon list', async () => {
      showMessageBox.mockResolvedValue({ response: 1 })
      await call(IpcChannels.OsModuleSetEnabled, 'sin90', true)
      const opts = showMessageBox.mock.calls[0]!.at(-1) as { message: string; detail: string }
      expect(opts.message).toContain('sin90')
      expect(opts.message).toContain('from disabled to enabled')
      expect(opts.detail).toContain('0.5.0')
      expect(opts.detail).toContain('Takes effect immediately')
      expect(opts.detail).not.toContain('takes effect after restart')
    })
    it('a module the daemon does not list is refused without a dialog', async () => {
      const res = await call(IpcChannels.OsModuleSetEnabled, 'ghost', true)
      expect(res.data.error.code).toBe('module_unavailable')
      expect(showMessageBox).not.toHaveBeenCalled()
      expect(seen).toHaveLength(0)
    })
    it('sends nothing when cancelled', async () => {
      showMessageBox.mockResolvedValue({ response: 1 })
      const res = await call(IpcChannels.OsModuleSetEnabled, 'sin90', false)
      expect(res.ok).toBe(false)
      expect(seen).toHaveLength(0)
    })
  })
})

describe('CommInstall elevated IPC', () => {
  it('requires native confirmation and cancellation sends no request', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 1 })
    const result = await call(IpcChannels.CommInstall)
    expect(result.ok).toBe(false)
    expect(seen).toHaveLength(0)
  })
  it('confirmation sends exactly one fixed install request', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    await call(IpcChannels.CommInstall)
    expect(showMessageBox).toHaveBeenCalledTimes(1)
    expect(seen).toHaveLength(1)
    expect(seen[0]).toMatchObject({ method: 'POST', url: '/api/v1/comm/install' })
  })
})

const NPUB = 'npub1' + 'q'.repeat(58)
// A real, checksum-valid npub (the NIP-49 vector's key).
const REAL_NPUB = 'npub140q9dxxcn4kan7s75j87vhrwn7mn7s7ncy5q2zutrcg6q2rqylmqxvda2x'

describe('CommIdentityExport elevated IPC', () => {
  const PW = 'backup-pass-1'
  const noDaemonCall = () => expect(seen.filter((r) => r.url?.includes('/identity/export'))).toHaveLength(0)

  it('the page API takes only a nickname: any extra argument (a password) is never used', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockResolvedValueOnce('main-collected-pass')
    showSaveDialog.mockResolvedValueOnce({ canceled: false, filePath: path.join(settingsDir, 'sig.ncryptsec') })
    await call(IpcChannels.CommIdentityExport, 'alice', 'renderer-supplied-pass')
    expect(JSON.parse(seen[0]!.body).backup_password).toBe('main-collected-pass')
    expect(seen[0]!.body).not.toContain('renderer-supplied-pass')
  })
  it('runs the steps in order: identity check, risk confirm, password window, save dialog, daemon', async () => {
    const order: string[] = []
    showMessageBox.mockImplementationOnce(async () => { order.push('confirm'); return { response: 0 } })
    promptBackupPassword.mockImplementationOnce(async () => { order.push('password'); return PW })
    showSaveDialog.mockImplementationOnce(async () => { order.push('save'); return { canceled: false, filePath: path.join(settingsDir, 'ord.ncryptsec') } })
    await call(IpcChannels.CommIdentityExport, 'alice')
    expect(order).toEqual(['confirm', 'password', 'save'])
    expect(seen.map((r) => r.url)).toEqual(['/api/v1/comm/identity/export'])
  })
  it('cancelling the risk confirm: no password window, no save dialog, no daemon call', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 1 })
    const result = await call(IpcChannels.CommIdentityExport, 'alice')
    expect(result.data.error.code).toBe('cancelled')
    expect(promptBackupPassword).not.toHaveBeenCalled()
    expect(showSaveDialog).not.toHaveBeenCalled()
    noDaemonCall()
  })
  it('cancelling the password window: no save dialog, no daemon call', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockResolvedValueOnce(null)
    const result = await call(IpcChannels.CommIdentityExport, 'alice')
    expect(result.data.error.code).toBe('cancelled')
    expect(showSaveDialog).not.toHaveBeenCalled()
    noDaemonCall()
  })
  it('cancelling the save dialog: no daemon call', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockResolvedValueOnce(PW)
    showSaveDialog.mockResolvedValueOnce({ canceled: true })
    const result = await call(IpcChannels.CommIdentityExport, 'alice')
    expect(result.data.error.code).toBe('cancelled')
    noDaemonCall()
  })
  it('sends one request: path from the dialog, the main-collected password in the body only, host credential', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockResolvedValueOnce(PW)
    showSaveDialog.mockResolvedValueOnce({ canceled: false, filePath: path.join(settingsDir, 'fresh-alice.ncryptsec') })
    await call(IpcChannels.CommIdentityExport, 'alice')
    expect(seen).toHaveLength(1)
    expect(seen[0]).toMatchObject({ method: 'POST', url: '/api/v1/comm/identity/export', auth: 'Bearer daemon-secret' })
    expect(JSON.parse(seen[0]!.body)).toEqual({
      nickname: 'alice', path: path.join(settingsDir, 'fresh-alice.ncryptsec'), backup_password: PW,
    })
    const opts = showMessageBox.mock.calls.at(-1)!.at(-1) as { detail: string }
    expect(opts.detail).toMatch(/impersonate/)
    expect(JSON.stringify(opts)).not.toContain(PW)
    expect(JSON.stringify(showSaveDialog.mock.calls.at(-1))).not.toContain(PW)
  })
  it('an unknown identity is refused before any dialog or window', async () => {
    showMessageBox.mockClear()
    const result = await call(IpcChannels.CommIdentityExport, 'ghost')
    expect(result.data.error.code).toBe('identity_not_found')
    expect(showMessageBox).not.toHaveBeenCalled()
    expect(promptBackupPassword).not.toHaveBeenCalled()
    expect(seen).toHaveLength(0)
  })
  it('the save dialog defaults to Downloads with a Unicode-safe name', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockResolvedValueOnce(PW)
    showSaveDialog.mockResolvedValueOnce({ canceled: true })
    await call(IpcChannels.CommIdentityExport, 'alice')
    const save = showSaveDialog.mock.calls.at(-1)!.at(-1) as { defaultPath: string }
    expect(save.defaultPath).toBe(path.join(settingsDir, 'alice.ncryptsec'))
  })
  it('an existing target is refused before any request (nothing overwritten)', async () => {
    const existing = path.join(settingsDir, 'taken.ncryptsec')
    fs.writeFileSync(existing, 'precious')
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockResolvedValueOnce(PW)
    showSaveDialog.mockResolvedValueOnce({ canceled: false, filePath: existing })
    const result = await call(IpcChannels.CommIdentityExport, 'alice')
    expect(result.data.error.code).toBe('target_exists')
    noDaemonCall()
    expect(fs.readFileSync(existing, 'utf8')).toBe('precious')
  })
  it('returns the daemon path to the renderer, and reveal accepts only that path', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockResolvedValueOnce(PW)
    const saved = path.join(settingsDir, 'shown-alice.ncryptsec')
    showSaveDialog.mockResolvedValueOnce({ canceled: false, filePath: saved })
    const result = await call(IpcChannels.CommIdentityExport, 'alice')
    expect(result.ok).toBe(true)
    expect(result.data.data).toMatchObject({ nickname: 'alice', npub: 'npub1alice', path: saved, format: 'ncryptsec' })
    const wrong = await call(IpcChannels.CommRevealExport, '/etc/passwd')
    expect(wrong.ok).toBe(false)
    expect(showItemInFolder).not.toHaveBeenCalled()
    const revealed = await call(IpcChannels.CommRevealExport, saved)
    expect(revealed.ok).toBe(true)
    expect(showItemInFolder).toHaveBeenCalledTimes(1)
    expect(showItemInFolder).toHaveBeenCalledWith(saved)
    const omitted = await call(IpcChannels.CommRevealExport)
    expect(omitted.ok).toBe(false)
    expect(showItemInFolder).toHaveBeenCalledTimes(1)
  })
  it('a daemon path that differs from the picked one is not recorded for reveal', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockResolvedValueOnce(PW)
    const picked = path.join(settingsDir, 'picked-alice.ncryptsec')
    const elsewhere = path.join(settingsDir, 'elsewhere-alice.ncryptsec')
    showSaveDialog.mockResolvedValueOnce({ canceled: false, filePath: picked })
    exportReportsPath = elsewhere
    try {
      const result = await call(IpcChannels.CommIdentityExport, 'alice')
      expect(result.ok).toBe(true)
    } finally {
      exportReportsPath = null
    }
    for (const target of [elsewhere, picked]) {
      const revealed = await call(IpcChannels.CommRevealExport, target)
      expect(revealed.ok).toBe(false)
    }
    expect(showItemInFolder).not.toHaveBeenCalled()
  })
  it('an unsafe backup password from the window is refused before the save dialog or daemon', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockResolvedValueOnce('short')
    showSaveDialog.mockClear()
    const result = await call(IpcChannels.CommIdentityExport, 'alice')
    expect(result.ok).toBe(false)
    expect(showSaveDialog).not.toHaveBeenCalled()
    noDaemonCall()
  })
  it('a cancelled save does not become a path the page can reveal', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockResolvedValueOnce(PW)
    showSaveDialog.mockResolvedValueOnce({ canceled: true })
    const result = await call(IpcChannels.CommIdentityExport, 'alice')
    expect(result.data.error.code).toBe('cancelled')
    const revealed = await call(IpcChannels.CommRevealExport, path.join(settingsDir, 'alice.ncryptsec'))
    expect(revealed.ok).toBe(false)
    expect(showItemInFolder).not.toHaveBeenCalled()
    noDaemonCall()
  })
  const until = async (cond: () => boolean): Promise<void> => {
    for (let i = 0; i < 500 && !cond(); i++) await new Promise((r) => setImmediate(r))
    expect(cond()).toBe(true)
  }

  it('the save dialog opens only after the password window is fully gone', async () => {
    let windowGone = false
    let openedWhileWindowUp = false
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockImplementationOnce(() => new Promise((resolve) => setTimeout(() => { windowGone = true; resolve(PW) }, 30)))
    showSaveDialog.mockImplementationOnce(async () => {
      openedWhileWindowUp = !windowGone
      return { canceled: true }
    })
    await call(IpcChannels.CommIdentityExport, 'alice')
    expect(showSaveDialog).toHaveBeenCalledTimes(1)
    expect(openedWhileWindowUp).toBe(false)
  })

  const aliveParent = () => trackableParent()

  it('a living parent is restored, shown, re-enabled and focused, then hosts the save sheet', async () => {
    const cases: { state: { minimized?: boolean; visible?: boolean; enabled?: boolean }; calls: string[] }[] = [
      { state: { minimized: true, visible: true, enabled: true }, calls: ['restore', 'app.focus', 'parent.focus', 'save', 'sheet'] },
      { state: { minimized: false, visible: false, enabled: true }, calls: ['show', 'app.focus', 'parent.focus', 'save', 'sheet'] },
      { state: { minimized: false, visible: true, enabled: false }, calls: ['setEnabled', 'app.focus', 'parent.focus', 'save', 'sheet'] },
      { state: { minimized: true, visible: false, enabled: false }, calls: ['restore', 'show', 'setEnabled', 'app.focus', 'parent.focus', 'save', 'sheet'] },
    ]
    for (const item of cases) {
      const parent = trackableParent(item.state)
      parentWindow = parent
      appFocus.mockReset()
      appFocus.mockImplementation(() => { parent.calls.push('app.focus') })
      showSaveDialog.mockReset()
      showSaveDialog.mockImplementationOnce(async (first: unknown) => {
        parent.calls.push('save')
        parent.calls.push(first === parent ? 'sheet' : 'app-modal')
        return { canceled: true }
      })
      showMessageBox.mockResolvedValueOnce({ response: 0 })
      promptBackupPassword.mockResolvedValueOnce(PW)
      const result = await call(IpcChannels.CommIdentityExport, 'alice')
      expect(result.data.error.code).toBe('cancelled')
      expect(parent.isMinimized()).toBe(false)
      expect(parent.isVisible()).toBe(true)
      expect(parent.isEnabled()).toBe(true)
      expect(showSaveDialog.mock.calls[0]?.[0]).toBe(parent)
      expect(appFocus).toHaveBeenCalledWith({ steal: true })
      expect(parent.calls).toEqual(item.calls)
      expect(parent.listenerCount('closed')).toBe(0)
    }
  })

  it('the parent window destroyed between the password step and the save step: cancelled, no dialog, no daemon call', async () => {
    let parentDestroyed = false
    parentWindow = { ...trackableParent(), isDestroyed: () => parentDestroyed }
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockImplementationOnce(async () => { parentDestroyed = true; return PW })
    const result = await call(IpcChannels.CommIdentityExport, 'alice')
    expect(result.data.error.code).toBe('cancelled')
    expect(showSaveDialog).not.toHaveBeenCalled()
    expect(seen.filter((r) => r.url === '/api/v1/comm/identity/export')).toHaveLength(0)
  })

  it('the parent destroyed or throwing while saveDialogParent brings it forward: cancelled, no dialog, no daemon call', async () => {
    const noExport = () => expect(seen.filter((r) => r.url === '/api/v1/comm/identity/export')).toHaveLength(0)

    // Still alive at the check before saveDialogParent. restore() destroys it
    // while the window is being brought forward, so the anchor comes back null.
    let destroyedOnRestore = false
    const minimized = trackableParent({ minimized: true })
    parentWindow = {
      ...minimized,
      isDestroyed: () => destroyedOnRestore,
      restore: () => {
        minimized.calls.push('restore')
        destroyedOnRestore = true
      },
    }
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockResolvedValueOnce(PW)
    const restored = await call(IpcChannels.CommIdentityExport, 'alice')
    expect(restored.data.error.code).toBe('cancelled')
    expect(showSaveDialog).not.toHaveBeenCalled()
    expect(minimized.calls).toContain('restore')
    noExport()

    resetLastCommExportForTests()
    showSaveDialog.mockClear()
    // focus() throws inside saveDialogParent. Same cancel, still no dialog.
    const throwing = trackableParent()
    parentWindow = {
      ...throwing,
      focus: () => { throw new Error('focus failed during restore') },
    }
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockResolvedValueOnce(PW)
    const focused = await call(IpcChannels.CommIdentityExport, 'alice')
    expect(focused.data.error.code).toBe('cancelled')
    expect(showSaveDialog).not.toHaveBeenCalled()
    noExport()
  })

  it('the save dialog is a sheet on the starting window while it is visible and enabled', async () => {
    parentWindow = aliveParent()
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockResolvedValueOnce(PW)
    showSaveDialog.mockResolvedValueOnce({ canceled: true })
    await call(IpcChannels.CommIdentityExport, 'alice')
    expect(showSaveDialog.mock.calls[0]![0]).toBe(parentWindow)
  })

  it('closing the parent or quitting while the save sheet is open cancels, releases the lock, and drops the listeners', async () => {
    const hangDialog = () => {
      let rejectDialog: (error: unknown) => void = () => {}
      let opened = false
      showSaveDialog.mockReset()
      showSaveDialog.mockImplementationOnce(() => {
        opened = true
        return new Promise((_resolve, reject) => { rejectDialog = reject })
      })
      return {
        opened: () => opened,
        reject: (error: unknown) => rejectDialog(error),
      }
    }
    const settle = async (pending: Promise<unknown>) => {
      const outcome = await Promise.race([
        pending.then((result) => result as { data: { error?: { code?: string } } }),
        new Promise<string>((resolve) => setTimeout(() => resolve('hung'), 500)),
      ])
      expect(outcome).not.toBe('hung')
      return outcome as { data: { error?: { code?: string } } }
    }

    parentWindow = trackableParent()
    const firstDialog = hangDialog()
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockResolvedValueOnce(PW)
    const first = call(IpcChannels.CommIdentityExport, 'alice')
    await until(() => firstDialog.opened())
    const listening = parentWindow.listenerCount('closed')
    parentWindow.emit('closed')
    const closedResult = await settle(first)
    expect(closedResult.data.error?.code).toBe('cancelled')
    expect(listening).toBe(1)
    expect(parentWindow.listenerCount('closed')).toBe(0)
    expect(appListeners.get('before-quit')?.size).toBe(1)
    firstDialog.reject(new Error('sheet died with its parent'))
    await new Promise((r) => setImmediate(r))
    showMessageBox.mockResolvedValueOnce({ response: 1 })
    const afterClose = await call(IpcChannels.CommIdentityExport, 'alice')
    expect(afterClose.data.error.code).not.toBe('busy')

    resetLastCommExportForTests()
    parentWindow = trackableParent()
    const quitDialog = hangDialog()
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockResolvedValueOnce(PW)
    const second = call(IpcChannels.CommIdentityExport, 'alice')
    await until(() => quitDialog.opened())
    expect(appListeners.get('before-quit')?.size).toBe(2)
    emitApp('before-quit')
    const quitResult = await settle(second)
    expect(quitResult.data.error?.code).toBe('cancelled')
    expect(parentWindow.listenerCount('closed')).toBe(0)
    expect(appListeners.get('before-quit')?.size).toBe(1)
    quitDialog.reject(new Error('sheet died because the app quit'))
    await new Promise((r) => setImmediate(r))
    showMessageBox.mockResolvedValueOnce({ response: 1 })
    const afterQuit = await call(IpcChannels.CommIdentityExport, 'alice')
    expect(afterQuit.data.error.code).not.toBe('busy')
    noDaemonCall()
  })

  it('quitting the app during the password step stops the flow before the save dialog', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockImplementationOnce(async () => { emitApp('before-quit'); return PW })
    const result = await call(IpcChannels.CommIdentityExport, 'alice')
    expect(result.data.error.code).toBe('cancelled')
    expect(showSaveDialog).not.toHaveBeenCalled()
    noDaemonCall()
  })

  it('human steps have no deadline: a 120 s password step and a 120 s save step still complete', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    try {
      const target = path.join(settingsDir, 'slow-human.ncryptsec')
      let givePassword: (pw: string) => void = () => {}
      let pickFile: (r: unknown) => void = () => {}
      showMessageBox.mockResolvedValueOnce({ response: 0 })
      promptBackupPassword.mockReturnValueOnce(new Promise<string>((r) => { givePassword = r }))
      showSaveDialog.mockReturnValueOnce(new Promise((r) => { pickFile = r }))
      const pending = call(IpcChannels.CommIdentityExport, 'alice')
      await until(() => promptBackupPassword.mock.calls.length > 0)
      await vi.advanceTimersByTimeAsync(120_000)
      givePassword(PW)
      await until(() => showSaveDialog.mock.calls.length > 0)
      await vi.advanceTimersByTimeAsync(120_000)
      pickFile({ canceled: false, filePath: target })
      const result = await pending
      expect(result.ok).toBe(true)
      expect(result.data.data.path).toBe(target)
    } finally {
      vi.useRealTimers()
    }
  })

  it('the daemon step is the one deadline: it answers timeout, and the slot stays held until the call really ends', async () => {
    exportTimeoutMs.current = 150
    hangExport = true
    const target = path.join(settingsDir, 'hung.ncryptsec')
    showMessageBox.mockResolvedValue({ response: 0 })
    promptBackupPassword.mockResolvedValue(PW)
    showSaveDialog.mockResolvedValue({ canceled: false, filePath: target })
    const first = await call(IpcChannels.CommIdentityExport, 'alice')
    expect(first.data.error.code).toBe('timeout')
    // The daemon request is still outstanding: nothing else may start.
    const second = await call(IpcChannels.CommIdentityExport, 'alice')
    expect(second.data.error.code).toBe('busy')
    const other = await call(IpcChannels.CommContactAdd, 'bob', NPUB, '')
    expect(other.data.error.code).toBe('busy')
    // The call ends: the slot frees.
    hangExport = false
    for (const r of hungExports.splice(0)) r.end(JSON.stringify({ ok: true, data: {} }))
    await until(() => !hungExports.length)
    let third = await call(IpcChannels.CommIdentityExport, 'alice')
    for (let i = 0; i < 50 && third.data.error?.code === 'busy'; i++) {
      await new Promise((r) => setTimeout(r, 10))
      third = await call(IpcChannels.CommIdentityExport, 'alice')
    }
    expect(third.data?.error?.code).not.toBe('busy')
    showMessageBox.mockReset()
    promptBackupPassword.mockReset()
    showSaveDialog.mockReset()
  })

  it('Stop / Start are refused while a comm write is in flight, and allowed otherwise', async () => {
    let release: (v: unknown) => void = () => {}
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockReturnValueOnce(new Promise((r) => { release = r }))
    const exporting = call(IpcChannels.CommIdentityExport, 'alice')
    await until(() => promptBackupPassword.mock.calls.length > 0)
    for (const route of ['stop', 'start']) {
      const refused = await call(IpcChannels.BackendProxy, { method: 'POST', path: `/api/v1/comm/daemon/${route}` })
      expect(refused.status).toBe(429)
      expect(refused.data.error.code).toBe('busy')
    }
    // Reads are not blocked.
    const read = await call(IpcChannels.BackendProxy, { method: 'GET', path: '/api/v1/comm/daemon' })
    expect(read.status).not.toBe(429)
    release(null)
    await exporting
    seen = []
    const stop = await call(IpcChannels.BackendProxy, { method: 'POST', path: '/api/v1/comm/daemon/stop' })
    expect(stop.status).not.toBe(429)
    expect(seen.map((r) => r.url)).toEqual(['/api/v1/comm/daemon/stop'])
  })

  it('the reveal target is cleared when the next export starts, and on failure', async () => {
    showMessageBox.mockResolvedValue({ response: 0 })
    promptBackupPassword.mockResolvedValue(PW)
    const saved = path.join(settingsDir, 'first-reveal.ncryptsec')
    showSaveDialog.mockResolvedValueOnce({ canceled: false, filePath: saved })
    await call(IpcChannels.CommIdentityExport, 'alice')
    // A second export begins and is declined: the first path is no longer revealable.
    showSaveDialog.mockResolvedValueOnce({ canceled: true })
    await call(IpcChannels.CommIdentityExport, 'alice')
    const revealed = await call(IpcChannels.CommRevealExport, saved)
    expect(revealed.ok).toBe(false)
    expect(showItemInFolder).not.toHaveBeenCalled()
    showMessageBox.mockReset()
    promptBackupPassword.mockReset()
  })

  it('the reveal target expires after ten minutes', async () => {
    vi.useFakeTimers({ toFake: ['Date'] })
    try {
      showMessageBox.mockResolvedValueOnce({ response: 0 })
      promptBackupPassword.mockResolvedValueOnce(PW)
      const saved = path.join(settingsDir, 'ttl-reveal.ncryptsec')
      showSaveDialog.mockResolvedValueOnce({ canceled: false, filePath: saved })
      await call(IpcChannels.CommIdentityExport, 'alice')
      vi.setSystemTime(Date.now() + 9 * 60_000)
      expect((await call(IpcChannels.CommRevealExport, saved)).ok).toBe(true)
      vi.setSystemTime(Date.now() + 2 * 60_000)
      const late = await call(IpcChannels.CommRevealExport, saved)
      expect(late.ok).toBe(false)
      expect(late.data.error.message).not.toBe('only the file just exported can be shown')
    } finally {
      vi.useRealTimers()
    }
  })

  it('a password the window returns but main finds unusable is refused before the save dialog', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    promptBackupPassword.mockResolvedValueOnce('short')
    const result = await call(IpcChannels.CommIdentityExport, 'alice')
    expect(result.data.error.code).toBe('invalid_request')
    expect(showSaveDialog).not.toHaveBeenCalled()
    noDaemonCall()
  })
  it.each([
    ['control chars in the name', ['al\nice']],
    ['object as the name', [{ nickname: 'alice' }]],
    ['no name', []],
  ])('refuses %s before any dialog', async (_name, args) => {
    showMessageBox.mockClear()
    const result = await call(IpcChannels.CommIdentityExport, ...(args as unknown[]))
    expect(result.data.error.code).toBe('invalid_request')
    expect(showMessageBox).not.toHaveBeenCalled()
    expect(seen).toHaveLength(0)
  })
})

describe('Restore from backup (CommImportPick / CommIdentityImport)', () => {
  const NCS = `ncryptsec1${'q'.repeat(150)}`
  const FILE = path.join(settingsDir, 'alice-backup.ncryptsec')
  const SECOND = path.join(settingsDir, 'second.ncryptsec')
  beforeEach(() => {
    fs.rmSync(FILE, { force: true }) // a previous test may have left a symlink here
    fs.writeFileSync(FILE, `${NCS}\n`)
    fs.writeFileSync(SECOND, `ncryptsec1${'p'.repeat(150)}\n`)
  })
  /** The real window runs `work` with what the user typed; this stands in for it. */
  const typing = (...passwords: string[]) => promptRestorePassword.mockImplementation(async (_win: unknown, work: (p: string) => Promise<{ done: boolean; value?: unknown; error?: string }>) => {
    for (const pw of passwords) {
      const outcome = await work(pw)
      if (outcome.done) return outcome.value
    }
    return null // user gave up
  })
  const pick = async () => {
    showOpenDialog.mockResolvedValueOnce({ canceled: false, filePaths: [FILE] })
    return call(IpcChannels.CommImportPick)
  }

  it('the open dialog hands the page a file NAME and a ticket, never the path', async () => {
    const result = await pick()
    expect(result.ok).toBe(true)
    expect(result.fileName).toBe('alice-backup.ncryptsec')
    expect(typeof result.ticket).toBe('string')
    expect(JSON.stringify(result)).not.toContain(settingsDir)
    expect(JSON.stringify(result)).not.toContain(NCS)
    const opts = showOpenDialog.mock.calls.at(-1)!.at(-1) as { properties: string[] }
    expect(opts.properties).toContain('openFile')
  })
  it('L2: the file is read when it is picked; swapping it afterwards changes nothing', async () => {
    const { ticket } = await pick()
    fs.writeFileSync(FILE, `ncryptsec1${'z'.repeat(150)}\n`) // swapped after the pick
    fs.rmSync(FILE)
    fs.symlinkSync(SECOND, FILE) // and replaced by a link
    typing('restore-pass-1')
    await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)
    expect(JSON.parse(seen[0]!.body).ncryptsec).toBe(NCS)
    expect(JSON.parse(seen[0]!.body)).not.toHaveProperty('path')
  })
  it.each([
    ['text that is not a backup', () => fs.writeFileSync(FILE, 'hello\n')],
    ['two lines', () => fs.writeFileSync(FILE, `${NCS}\nsecond\n`)],
    ['a file over 4 KiB', () => fs.writeFileSync(FILE, `ncryptsec1${'q'.repeat(5000)}`)],
    ['an empty file', () => fs.writeFileSync(FILE, '')],
    ['a symlink', () => { fs.rmSync(FILE); fs.symlinkSync(SECOND, FILE) }],
  ])('L2: %s gets no ticket', async (_name, arrange) => {
    arrange()
    const result = await pick()
    expect(result.ok).toBe(false)
    expect(result.cancelled).toBe(false)
    expect(result.ticket).toBeUndefined()
    expect(typeof result.message).toBe('string')
    typing('restore-pass-1')
    expect((await call(IpcChannels.CommIdentityImport, 'f'.repeat(32), 'alice', false)).data.error.code).toBe('ticket_expired')
    expect(seen).toHaveLength(0)
  })
  it('L-5: an expired ticket is dropped for good, even if the clock is later wound back', async () => {
    const t0 = 1_700_000_000_000
    const now = vi.spyOn(Date, 'now').mockReturnValue(t0)
    try {
      const { ticket } = await pick()
      now.mockReturnValue(t0 + 5 * 60_000 + 1)
      expect((await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)).data.error.code).toBe('ticket_expired')
      now.mockReturnValue(t0 + 1000) // inside the original window again
      expect((await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)).data.error.code).toBe('ticket_expired')
      expect(promptRestorePassword).not.toHaveBeenCalled()
    } finally {
      now.mockRestore()
    }
  })
  it.each([
    ['is cancelled', () => showOpenDialog.mockResolvedValueOnce({ canceled: true, filePaths: [] })],
    ['picks a file that is not a backup', () => { fs.writeFileSync(SECOND, 'hello\n'); showOpenDialog.mockResolvedValueOnce({ canceled: false, filePaths: [SECOND] }) }],
    ['throws', () => showOpenDialog.mockRejectedValueOnce(new Error('boom'))],
  ])('L-5: starting a new pick invalidates the old ticket even if the new pick %s', async (_name, arrange) => {
    const { ticket } = await pick()
    arrange()
    await call(IpcChannels.CommImportPick)
    typing('restore-pass-1')
    expect((await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)).data.error.code).toBe('ticket_expired')
    expect(seen).toHaveLength(0)
  })
  it('a window that waits for a password cannot submit the old backup after a newer pick was started and cancelled', async () => {
    const first = await pick()
    let submit!: (pw: string) => Promise<{ done: boolean; value?: any }> // eslint-disable-line @typescript-eslint/no-explicit-any
    let windowOpen!: () => void
    let closeWindow!: (v: null) => void
    const opened = new Promise<void>((r) => { windowOpen = r })
    promptRestorePassword.mockImplementationOnce(async (_w: unknown, work: typeof submit) => {
      submit = work
      windowOpen()
      return new Promise((resolve) => { closeWindow = resolve }) // the window just sits there
    })
    const running = call(IpcChannels.CommIdentityImport, first.ticket, 'alice', false)
    await opened
    showOpenDialog.mockResolvedValueOnce({ canceled: true, filePaths: [] })
    expect(await call(IpcChannels.CommImportPick)).toEqual({ ok: false, cancelled: true })
    // the user now types the password into the OLD window, first try and retry
    for (let i = 0; i < 2; i += 1) {
      const outcome = await submit('restore-pass-1')
      expect(outcome.done).toBe(true)
      expect(outcome.value.data.error.code).toBe('superseded')
    }
    expect(seen).toHaveLength(0) // no daemon request was made
    closeWindow(null)
    await running
  })
  it('L-5: a flow that ends while a new pick dialog is open cannot put its old ticket back', async () => {
    const first = await pick()
    let release!: () => void
    promptRestorePassword.mockImplementationOnce(() => new Promise((resolve) => { release = () => resolve(null) }))
    const running = call(IpcChannels.CommIdentityImport, first.ticket, 'alice', false)
    await new Promise((r) => setTimeout(r, 20))
    let finishPick!: (v: unknown) => void
    showOpenDialog.mockImplementationOnce(() => new Promise((resolve) => { finishPick = resolve }))
    const picking = call(IpcChannels.CommImportPick)
    await new Promise((r) => setTimeout(r, 20))
    release() // the old flow is cancelled while the new dialog is still open
    expect((await running).data.error.code).toBe('cancelled')
    finishPick({ canceled: true, filePaths: [] })
    await picking
    expect((await call(IpcChannels.CommIdentityImport, first.ticket, 'alice', false)).data.error.code).toBe('ticket_expired')
  })
  it('cancelling the open dialog yields no ticket and nothing else happens', async () => {
    showOpenDialog.mockResolvedValueOnce({ canceled: true, filePaths: [] })
    expect(await call(IpcChannels.CommImportPick)).toEqual({ ok: false, cancelled: true })
    expect(promptRestorePassword).not.toHaveBeenCalled()
    expect(seen).toHaveLength(0)
  })
  it('restores: the backup text was read by main at pick time, the password from main\'s window, the nickname and default from the page', async () => {
    const { ticket } = await pick()
    typing('restore-pass-1')
    const result = await call(IpcChannels.CommIdentityImport, ticket, 'alice', true)
    expect(result.ok).toBe(true)
    expect(seen).toHaveLength(1)
    expect(seen[0]).toMatchObject({ method: 'POST', url: '/api/v1/comm/identity/import', auth: 'Bearer daemon-secret' })
    expect(JSON.parse(seen[0]!.body)).toEqual({ nickname: 'alice', ncryptsec: NCS, backup_password: 'restore-pass-1', default: true })
    expect(promptRestorePassword).toHaveBeenCalledTimes(1)
  })
  it('a wrong password is retried inside the window; the page only sees the final answer', async () => {
    const { ticket } = await pick()
    typing('wrong-pass-x', 'right-pass-1')
    const result = await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)
    expect(result.ok).toBe(true)
    expect(seen.map((r) => JSON.parse(r.body).backup_password)).toEqual(['wrong-pass-x', 'right-pass-1'])
  })
  it('cancelling the password window makes no daemon call and keeps the selection', async () => {
    const { ticket } = await pick()
    typing() // closes without typing anything
    const cancelled = await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)
    expect(cancelled.data.error.code).toBe('cancelled')
    expect(seen).toHaveLength(0)
    typing('restore-pass-1') // the same ticket still works
    const again = await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)
    expect(again.ok).toBe(true)
  })
  it('a daemon refusal (nickname_exists) keeps the selection so the nickname can be changed', async () => {
    const { ticket } = await pick()
    typing('dupe-nick-pass')
    const refused = await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)
    expect(refused.ok).toBe(false)
    expect(refused.data.error).toBe('nickname_exists')
    typing('restore-pass-1')
    expect((await call(IpcChannels.CommIdentityImport, ticket, 'alice2', false)).ok).toBe(true)
  })
  it('shows "will become the default" in the window only when the restore asks for the default', async () => {
    const { ticket } = await pick()
    typing('restore-pass-1')
    await call(IpcChannels.CommIdentityImport, ticket, 'alice', true)
    expect(promptRestorePassword.mock.calls[0]![2]).toEqual({ willBeDefault: true, mayBeDefault: false })
    const second = await pick()
    typing('restore-pass-1')
    await call(IpcChannels.CommIdentityImport, second.ticket, 'bob', false)
    expect(promptRestorePassword.mock.calls[1]![2]).toEqual({ willBeDefault: false, mayBeDefault: false })
  })
  it('input the daemon calls invalid is fixed inside the window, not by closing it', async () => {
    const { ticket } = await pick()
    typing('bad-input-pass', 'restore-pass-1')
    const result = await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)
    expect(result.ok).toBe(true)
    expect(seen.map((r) => JSON.parse(r.body).backup_password)).toEqual(['bad-input-pass', 'restore-pass-1'])
  })
  it('an invalid-input answer (empty nickname/password) does not use up an attempt', async () => {
    const { ticket } = await pick()
    typing('bad-input-pass', 'bad-input-pass', 'bad-input-pass', 'bad-input-pass', 'restore-pass-1')
    const result = await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)
    expect(result.ok).toBe(true)
    expect(seen).toHaveLength(5)
  })
  it('a selection survives at most three attempts', async () => {
    const { ticket } = await pick()
    typing() // the user closes the window each time
    for (let attempt = 1; attempt <= 3; attempt += 1) {
      const result = await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)
      expect(result.data.error.code, `attempt ${attempt}`).toBe('cancelled')
    }
    const fourth = await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)
    expect(fourth.data.error.code).toBe('ticket_expired')
    expect(seen).toHaveLength(0)
  })
  it('re-arming never extends the original expiry', async () => {
    const t0 = 1_800_000_000_000
    const now = vi.spyOn(Date, 'now').mockReturnValue(t0)
    try {
      const { ticket } = await pick()
      typing()
      now.mockReturnValue(t0 + 4 * 60_000) // inside the 5 minute window
      expect((await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)).data.error.code).toBe('cancelled')
      now.mockReturnValue(t0 + 5 * 60_000 + 1) // re-armed 61 s ago, but the original 5 minutes are up
      expect((await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)).data.error.code).toBe('ticket_expired')
    } finally {
      now.mockRestore()
    }
  })
  it('a newer pick invalidates the older ticket, and an older flow cannot put its ticket back', async () => {
    const first = await pick()
    let release!: () => void
    promptRestorePassword.mockImplementationOnce(() => new Promise((resolve) => { release = () => resolve(null) }))
    const running = call(IpcChannels.CommIdentityImport, first.ticket, 'alice', false)
    await new Promise((r) => setTimeout(r, 20)) // the window for the FIRST selection is open
    showOpenDialog.mockResolvedValueOnce({ canceled: false, filePaths: [SECOND] })
    const second = await call(IpcChannels.CommImportPick)
    release() // the first flow ends in a cancel
    expect((await running).data.error.code).toBe('cancelled')
    // the first ticket is dead; the second still works
    expect((await call(IpcChannels.CommIdentityImport, first.ticket, 'alice', false)).data.error.code).toBe('ticket_expired')
    typing('restore-pass-1')
    expect((await call(IpcChannels.CommIdentityImport, second.ticket, 'bob', false)).ok).toBe(true)
    expect(JSON.parse(seen[0]!.body).ncryptsec).toBe(`ncryptsec1${'p'.repeat(150)}`)
  })
  it('main decides the "will become the default" notice: when asked, OR when there is no default identity yet', async () => {
    identityListHasDefault = false
    const { ticket } = await pick()
    typing('restore-pass-1')
    await call(IpcChannels.CommIdentityImport, ticket, 'alice', false) // the page did NOT ask for the default
    expect(promptRestorePassword.mock.calls[0]![2]).toEqual({ willBeDefault: true, mayBeDefault: false })
    identityListHasDefault = true
    const second = await pick()
    typing('restore-pass-1')
    await call(IpcChannels.CommIdentityImport, second.ticket, 'bob', false)
    expect(promptRestorePassword.mock.calls[1]![2]).toEqual({ willBeDefault: false, mayBeDefault: false })
  })
  it('if the identity list cannot be read, the window says the identity MAY become the default (never silently "no")', async () => {
    identityListFails = true
    const { ticket } = await pick()
    typing('restore-pass-1')
    await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)
    expect(promptRestorePassword.mock.calls[0]![2]).toEqual({ willBeDefault: false, mayBeDefault: true })
    // when the page asked for the default, that stays a plain "will"
    const second = await pick()
    typing('restore-pass-1')
    await call(IpcChannels.CommIdentityImport, second.ticket, 'bob', true)
    expect(promptRestorePassword.mock.calls[1]![2]).toEqual({ willBeDefault: true, mayBeDefault: false })
  })
  it('retries inside the open window count against the attempt limit', async () => {
    const { ticket } = await pick()
    typing('wrong-pass-a', 'wrong-pass-b', 'wrong-pass-c', 'wrong-pass-d')
    const result = await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)
    // 1 free submit + 2 retries (3 attempts in all), then the flow is closed with a clear message
    expect(seen).toHaveLength(3)
    expect(result.ok).toBe(false)
    expect(result.data.error.code).toBe('too_many_attempts')
    expect((await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)).data.error.code).toBe('ticket_expired')
  })
  it('a retry inside the open window cannot outlive the ticket expiry', async () => {
    const t0 = 1_900_000_000_000
    const now = vi.spyOn(Date, 'now').mockReturnValue(t0)
    try {
      const { ticket } = await pick()
      promptRestorePassword.mockImplementation(async (_w: unknown, work: (p: string) => Promise<{ done: boolean; value?: unknown }>) => {
        const first = await work('wrong-pass-a')
        if (first.done) return first.value
        now.mockReturnValue(t0 + 5 * 60_000 + 1) // the user dawdles past the expiry
        const second = await work('right-pass-1')
        return second.done ? second.value : null
      })
      const result = await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)
      expect(result.data.error.code).toBe('ticket_expired')
      expect(seen).toHaveLength(1) // only the first submit reached the daemon
    } finally {
      now.mockRestore()
    }
  })
  it('a ticket is single use after success and unknown tickets are refused without any window', async () => {
    const { ticket } = await pick()
    typing('restore-pass-1')
    await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)
    promptRestorePassword.mockClear()
    const reused = await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)
    expect(reused.data.error.code).toBe('ticket_expired')
    const forged = await call(IpcChannels.CommIdentityImport, 'f'.repeat(32), 'alice', false)
    expect(forged.data.error.code).toBe('ticket_expired')
    expect(promptRestorePassword).not.toHaveBeenCalled()
  })
  it.each([
    ['control chars in the nickname', ['al\nice', false]],
    ['object as the nickname', [{}, false]],
    ['non-boolean default', ['alice', 'yes']],
  ])('refuses %s before any window', async (_name, rest) => {
    const { ticket } = await pick()
    const result = await call(IpcChannels.CommIdentityImport, ticket, ...(rest as unknown[]))
    expect(result.data.error.code).toBe('invalid_request')
    expect(promptRestorePassword).not.toHaveBeenCalled()
    expect(seen).toHaveLength(0)
  })
  it('a non-string ticket is refused', async () => {
    const result = await call(IpcChannels.CommIdentityImport, { path: '/etc/passwd' }, 'alice', false)
    expect(result.data.error.code).toBe('invalid_request')
    expect(seen).toHaveLength(0)
  })
})

describe('L4: whole-home import, relay list and unlock (dedicated IPC)', () => {
  const HOME = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-home-'))
  const typingHome = (...passwords: string[]) => promptHomeImportPassword.mockImplementation(async (_win: unknown, work: (p: string) => Promise<{ done: boolean; value?: unknown; error?: string }>) => {
    for (const pw of passwords) {
      const outcome = await work(pw)
      if (outcome.done) return outcome.value
    }
    return null
  })
  const typingUnlock = (...passwords: string[]) => promptUnlockPassword.mockImplementation(async (_win: unknown, work: (p: string) => Promise<{ done: boolean; value?: unknown; error?: string }>) => {
    for (const pw of passwords) {
      const outcome = await work(pw)
      if (outcome.done) return outcome.value
    }
    return null
  })

  it('home import: main picks the folder, confirms natively showing it, then asks for the password itself', async () => {
    showOpenDialog.mockResolvedValueOnce({ canceled: false, filePaths: [HOME] })
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    typingHome('home-pass-1')
    const result = await call(IpcChannels.CommHomeImport, '/etc', 'ignored-page-args')
    expect(result.ok).toBe(true)
    const dialogOpts = showOpenDialog.mock.calls.at(-1)!.at(-1) as { properties: string[] }
    expect(dialogOpts.properties).toEqual(['openDirectory'])
    expect((showMessageBox.mock.calls.at(-1)!.at(-1) as { message: string }).message).toContain(HOME)
    expect(seen).toHaveLength(1)
    expect(seen[0]).toMatchObject({ method: 'POST', url: '/api/v1/comm/import', auth: 'Bearer daemon-secret' })
    expect(JSON.parse(seen[0]!.body)).toEqual({ from: HOME, password: 'home-pass-1', confirm: true })
  })
  it('home import: declining the folder picker, the confirm or the password window sends nothing', async () => {
    showOpenDialog.mockResolvedValueOnce({ canceled: true, filePaths: [] })
    expect((await call(IpcChannels.CommHomeImport)).data.error.code).toBe('cancelled')
    showOpenDialog.mockResolvedValueOnce({ canceled: false, filePaths: [HOME] })
    showMessageBox.mockResolvedValueOnce({ response: 1 })
    expect((await call(IpcChannels.CommHomeImport)).data.error.code).toBe('cancelled')
    expect(promptHomeImportPassword).not.toHaveBeenCalled()
    showOpenDialog.mockResolvedValueOnce({ canceled: false, filePaths: [HOME] })
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    typingHome()
    expect((await call(IpcChannels.CommHomeImport)).data.error.code).toBe('cancelled')
    expect(seen).toHaveLength(0)
  })
  it('home import: a path that is not a folder is refused before the confirm', async () => {
    showOpenDialog.mockResolvedValueOnce({ canceled: false, filePaths: [path.join(HOME, 'missing')] })
    showMessageBox.mockClear()
    expect((await call(IpcChannels.CommHomeImport)).data.error.code).toBe('invalid_request')
    expect(showMessageBox).not.toHaveBeenCalled()
    expect(seen).toHaveLength(0)
  })
  it('home import: a wrong keystore password is retried inside the window', async () => {
    showOpenDialog.mockResolvedValueOnce({ canceled: false, filePaths: [HOME] })
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    typingHome('wrong-home', 'home-pass-1')
    expect((await call(IpcChannels.CommHomeImport)).ok).toBe(true)
    expect(seen.map((r) => JSON.parse(r.body).password)).toEqual(['wrong-home', 'home-pass-1'])
  })

  it('skills: the confirmation is built in main from the daemon record, then one POST with the daemon generation', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    const result = await call(IpcChannels.SkillsActivate, {
      skillId: 'demo', bodyRevisionId: 'b2', evalsRevisionId: 'e1', summary: 'FORGED', expectedGeneration: 99,
    })
    expect(result.ok).toBe(true)
    const opts = showMessageBox.mock.calls.at(-1)!.at(-1) as { message: string; detail: string }
    expect(opts.message).toContain('"demo"')
    expect(opts.detail).toContain('2.0.0')
    expect(opts.detail).toContain('cafef00d')
    expect(`${opts.message}${opts.detail}`).not.toContain('FORGED')
    const posts = seen.filter((r) => r.method === 'POST')
    expect(posts).toHaveLength(1)
    expect(posts[0]!.url).toBe('/api/v1/skills/demo/activate')
    expect(JSON.parse(posts[0]!.body)).toEqual({ expected_generation: 2, body_revision_id: 'b2', evals_revision_id: 'e1', accepted: [] })
  })
  it('skills: declining sends no POST, and the page cannot reach the route through the proxy', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 1 })
    expect((await call(IpcChannels.SkillsActivate, { skillId: 'demo', bodyRevisionId: 'b2', evalsRevisionId: 'e1' })).data.error.code).toBe('cancelled')
    expect(seen.filter((r) => r.method === 'POST')).toHaveLength(0)
    const proxied = await call(IpcChannels.BackendProxy, { method: 'POST', path: '/api/v1/skills/demo/activate', body: {} })
    expect(proxied.ok).toBe(false)
    expect(seen.filter((r) => r.method === 'POST')).toHaveLength(0)
  })

  it('relay: a changed list is confirmed natively, then PUT exactly as validated', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    const list = ['wss://a.example', 'ws://localhost:7777']
    const result = await call(IpcChannels.CommRelaySet, list)
    expect(result.ok).toBe(true)
    const opts = showMessageBox.mock.calls.at(-1)!.at(-1) as { message: string; detail: string }
    expect(opts.detail).toContain('wss://a.example')
    expect(seen).toHaveLength(1)
    expect(seen[0]).toMatchObject({ method: 'PUT', url: '/api/v1/comm/relay' })
    expect(JSON.parse(seen[0]!.body)).toEqual({ relays: list })
  })
  it('relay: declining the confirm sends nothing; an unchanged list needs no confirm', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 1 })
    expect((await call(IpcChannels.CommRelaySet, ['wss://a.example'])).data.error.code).toBe('cancelled')
    expect(seen).toHaveLength(0)
    showMessageBox.mockClear()
    const same = await call(IpcChannels.CommRelaySet, ['wss://old.example'])
    expect(same.ok).toBe(true)
    // L-8: no dialog and NO daemon write; the answer is the current configuration
    expect(showMessageBox).not.toHaveBeenCalled()
    expect(seen).toHaveLength(0)
    expect(same.data).toEqual({ ok: true, data: { relays: ['wss://old.example'], source: 'config', configured: true } })
    // the stored list is compared in normalized form
    currentRelays = { relays: ['wss://Old.Example/'], configured: true }
    expect((await call(IpcChannels.CommRelaySet, ['wss://old.example'])).ok).toBe(true)
    expect(seen).toHaveLength(0)
    // an unconfigured daemon always needs the write
    currentRelays = { relays: ['wss://old.example'], configured: false }
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    await call(IpcChannels.CommRelaySet, ['wss://old.example'])
    expect(seen).toHaveLength(1)
  })
  it('an @ in the PATH is fine; only userinfo is refused', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    expect((await call(IpcChannels.CommRelaySet, ['wss://x.example/path@tag'])).ok).toBe(true)
    expect(JSON.parse(seen[0]!.body).relays).toEqual(['wss://x.example/path@tag'])
  })
  it('L-9: ws:// is accepted for loopback hosts only', async () => {
    showMessageBox.mockResolvedValue({ response: 0 })
    const list = ['ws://localhost', 'ws://127.0.0.1:9', 'ws://127.8.8.8', 'ws://[::1]:7777']
    expect((await call(IpcChannels.CommRelaySet, list)).ok).toBe(true)
    expect(JSON.parse(seen[0]!.body).relays).toEqual(list)
    showMessageBox.mockReset()
  })
  it('M-1: the dialog shows every relay in full, one per line, even for 8 maximum-length URLs', async () => {
    const urls = Array.from({ length: 8 }, (_, i) => `wss://r.example/${String(i).repeat(3)}${'p'.repeat(256 - 'wss://r.example/'.length - 3)}`)
    expect(urls.every((u) => u.length === 256)).toBe(true)
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    expect((await call(IpcChannels.CommRelaySet, urls)).ok).toBe(true)
    const detail = (showMessageBox.mock.calls.at(-1)!.at(-1) as { detail: string }).detail
    const lines = detail.split('\n').slice(0, 8)
    expect(lines).toEqual(urls) // nothing clipped, nothing hidden, same order
    expect(detail).not.toContain('truncated')
    expect(JSON.parse(seen[0]!.body).relays).toEqual(urls)
  })
  it('M-2: the dialog and the daemon get the normalized form', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    await call(IpcChannels.CommRelaySet, ['WSS://Relay.Example:443/Path'])
    expect((showMessageBox.mock.calls.at(-1)!.at(-1) as { detail: string }).detail.startsWith('wss://relay.example/Path\n')).toBe(true)
    expect(JSON.parse(seen[0]!.body)).toEqual({ relays: ['wss://relay.example/Path'] })
  })
  it.each([
    ['not an array', 'wss://a.example'],
    ['empty', []],
    ['nine entries', Array.from({ length: 9 }, (_, i) => `wss://r${i}.example`)],
    ['https scheme', ['https://a.example']],
    ['ws:// to a non-loopback host', ['ws://relay.example']],
    ['ws:// to a lookalike of localhost', ['ws://localhost.evil.example']],
    ['Cyrillic homoglyph host', ['wss://\u0440elay.example']],
    ['RLO in the path', ['wss://relay.example/\u202Egpj']],
    ['zero-width space', ['wss://relay\u200B.example']],
    ['fragment', ['wss://relay.example/#frag']],
    ['query string', ['wss://relay.example/?q=1']],
    ['userinfo', ['user@relay.example', 'wss://user@relay.example']],
    ['malformed authority [::1]evil', ['ws://[::1]evil']],
    ['non-numeric port', ['ws://localhost:evil']],
    ['257-character url', [`wss://r.example/${'p'.repeat(257 - 'wss://r.example/'.length)}`]],
    ['eight valid and one hidden over-long url', [...Array.from({ length: 7 }, (_, i) => `wss://r${i}.example`), `wss://r.example/${'p'.repeat(300)}`]],
    ['no scheme', ['a.example']],
    ['credentials in the url', ['wss://user:pw@a.example']],
    ['whitespace', ['wss://a.example /x']],
    ['newline', ['wss://a.example\nwss://b.example']],
    ['duplicate', ['wss://a.example', 'wss://a.example']],
    ['non-string entry', [{ url: 'wss://a.example' }]],
    ['no host', ['wss://']],
    ['overlong', [`wss://${'a'.repeat(600)}.example`]],
  ])('relay: refuses %s before any dialog or request', async (_name, list) => {
    showMessageBox.mockClear()
    const result = await call(IpcChannels.CommRelaySet, list)
    expect(result.data.error.code).toBe('invalid_request')
    expect(showMessageBox).not.toHaveBeenCalled()
    expect(seen).toHaveLength(0)
  })

  it('unlock: the password comes from main\'s window; the page only passes the remember flag', async () => {
    typingUnlock('right-unlock')
    const result = await call(IpcChannels.CommUnlock, true)
    expect(result.ok).toBe(true)
    expect(JSON.parse(seen[0]!.body)).toEqual({ password: 'right-unlock', remember: true })
    expect(promptUnlockPassword.mock.calls[0]![2]).toEqual({ remember: true })
  })
  it('unlock: a wrong password is retried inside the window; closing it is a cancel', async () => {
    typingUnlock('wrong-unlock', 'right-unlock')
    expect((await call(IpcChannels.CommUnlock, false)).ok).toBe(true)
    expect(seen.map((r) => JSON.parse(r.body).password)).toEqual(['wrong-unlock', 'right-unlock'])
    seen = []
    typingUnlock()
    expect((await call(IpcChannels.CommUnlock, false)).data.error.code).toBe('cancelled')
    expect(seen).toHaveLength(0)
  })
  it.each([['a string', 'true'], ['an object', { remember: true }], ['a number', 1]])('unlock: refuses %s as the remember flag before any window', async (_name, flag) => {
    const result = await call(IpcChannels.CommUnlock, flag)
    expect(result.data.error.code).toBe('invalid_request')
    expect(promptUnlockPassword).not.toHaveBeenCalled()
  })
})

describe('CommContactAdd / CommSend dedicated IPC', () => {
  it('contact add shows a native confirm with the nickname and the FULL npub; cancelling sends nothing', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 1 })
    const result = await call(IpcChannels.CommContactAdd, 'bob', REAL_NPUB, 'friend')
    expect(result.data.error.code).toBe('cancelled')
    expect(seen).toHaveLength(0)
    const opts = showMessageBox.mock.calls.at(-1)!.at(-1) as { message: string }
    expect(opts.message).toContain('bob')
    expect(opts.message).toContain(REAL_NPUB)
  })
  it('adds a contact with a fixed route and only validated fields', async () => {
    showMessageBox.mockResolvedValueOnce({ response: 0 })
    await call(IpcChannels.CommContactAdd, 'bob', REAL_NPUB, 'friend')
    expect(seen).toHaveLength(1)
    expect(seen[0]).toMatchObject({ method: 'POST', url: '/api/v1/comm/contact', auth: 'Bearer daemon-secret' })
    expect(JSON.parse(seen[0]!.body)).toEqual({ nickname: 'bob', npub: REAL_NPUB, role: 'friend' })
  })
  it.each([
    ['npub with a bad checksum', ['bob', NPUB, '']],
    ['npub of the wrong prefix', ['bob', 'nsec1' + 'q'.repeat(58), '']],
    ['newline in the nickname', ['bo\nb', REAL_NPUB, '']],
    ['newline in the role', ['bob', REAL_NPUB, 'x\ny']],
    ['non-string nickname', [{}, REAL_NPUB, '']],
  ])('contact add refuses %s (before the confirm dialog)', async (_name, args) => {
    showMessageBox.mockClear()
    const result = await call(IpcChannels.CommContactAdd, ...(args as unknown[]))
    expect(result.data.error.code).toBe('invalid_request')
    expect(seen).toHaveLength(0)
    expect(showMessageBox).not.toHaveBeenCalled()
  })
  it('sends encrypted to a contact name or a valid npub, with bounded content', async () => {
    await call(IpcChannels.CommSend, 'bob', 'hello')
    await call(IpcChannels.CommSend, REAL_NPUB, 'hello')
    expect(seen.map((s) => [s.method, s.url])).toEqual([['POST', '/api/v1/comm/send'], ['POST', '/api/v1/comm/send']])
    expect(JSON.parse(seen[1]!.body)).toEqual({ to: REAL_NPUB, content: 'hello', encrypt: true })
  })
  it.each([
    ['an invalid npub as recipient', [NPUB, 'hi']],
    ['a recipient with a newline', ['bo\nb', 'hi']],
    ['empty content', ['bob', '   ']],
    ['oversized content', ['bob', 'x'.repeat(16 * 1024 + 1)]],
    ['NUL in content', ['bob', 'a\0b']],
    ['non-string content', ['bob', { a: 1 }]],
  ])('send refuses %s', async (_name, args) => {
    const result = await call(IpcChannels.CommSend, ...(args as unknown[]))
    expect(result.data.error.code).toBe('invalid_request')
    expect(seen).toHaveLength(0)
  })
})

describe('A-3 AgentEar disable / unpair (dedicated IPC + native confirm)', () => {
  it('setEnabled(false) PATCHes /api/v1/attached/agentear after confirmation, with the host credential', async () => {
    showMessageBox.mockResolvedValue({ response: 0 })
    const res = await call(IpcChannels.AgentEarSetEnabled, false)
    expect(res.ok).toBe(true)
    expect(showMessageBox).toHaveBeenCalledTimes(1)
    expect(seen).toHaveLength(1)
    expect(seen[0]?.method).toBe('PATCH')
    expect(seen[0]?.url).toBe('/api/v1/attached/agentear')
    expect(seen[0]?.auth).toBe('Bearer daemon-secret')
    expect(JSON.parse(seen[0]!.body)).toEqual({ enabled: false })
  })
  it('setEnabled(true) sends enabled: true', async () => {
    attachedModules = [{ name: 'agentear', attach_status: 'disabled' }]
    showMessageBox.mockResolvedValue({ response: 0 })
    const res = await call(IpcChannels.AgentEarSetEnabled, true)
    expect(res.ok).toBe(true)
    expect(seen.map((s) => [s.method, s.url, s.body])).toEqual([['PATCH', '/api/v1/attached/agentear', '{"enabled":true}']])
  })
  it('unpair DELETEs /api/v1/attached/agentear after confirmation', async () => {
    showMessageBox.mockResolvedValue({ response: 0 })
    const res = await call(IpcChannels.AgentEarUnpair)
    expect(res.ok).toBe(true)
    expect(res.status).toBe(204)
    expect(seen.map((s) => [s.method, s.url])).toEqual([['DELETE', '/api/v1/attached/agentear']])
  })
  it('only ever touches agentear: a page-supplied name is ignored or refused', async () => {
    showMessageBox.mockResolvedValue({ response: 0 })
    // A name in the first slot is not a boolean -> refused before any dialog.
    const named = await call(IpcChannels.AgentEarSetEnabled, 'other', false)
    expect(named.data.error.code).toBe('invalid_request')
    expect(showMessageBox).not.toHaveBeenCalled()
    expect(seen).toHaveLength(0)
    // Extra arguments cannot redirect the request.
    await call(IpcChannels.AgentEarSetEnabled, false, 'other')
    await call(IpcChannels.AgentEarUnpair, 'other')
    await call(IpcChannels.AgentEarUnpair, '../../shutdown')
    expect(seen.map((s) => s.url)).toEqual(['/api/v1/attached/agentear', '/api/v1/attached/agentear', '/api/v1/attached/agentear'])
  })
  it('rejects a non-boolean enabled before asking', async () => {
    for (const v of [undefined, null, 'false', 0, 1, {}]) {
      const res = await call(IpcChannels.AgentEarSetEnabled, v)
      expect(res.ok).toBe(false)
      expect(res.data.error.code).toBe('invalid_request')
    }
    expect(showMessageBox).not.toHaveBeenCalled()
    expect(seen).toHaveLength(0)
  })
  it('sends nothing when the user cancels (both channels)', async () => {
    showMessageBox.mockResolvedValue({ response: 1 })
    const a = await call(IpcChannels.AgentEarSetEnabled, false)
    const b = await call(IpcChannels.AgentEarUnpair)
    expect(a.data.error.code).toBe('cancelled')
    expect(b.data.error.code).toBe('cancelled')
    expect(showMessageBox).toHaveBeenCalledTimes(2)
    expect(seen).toHaveLength(0)
  })
  it('a dialog that fails to open counts as a cancel', async () => {
    showMessageBox.mockRejectedValue(new Error('no display'))
    const res = await call(IpcChannels.AgentEarUnpair)
    expect(res.data.error.code).toBe('cancelled')
    expect(seen).toHaveLength(0)
  })
  it('not paired -> not_paired without any dialog or write', async () => {
    attachedModules = [{ name: 'other', attach_status: 'attached' }]
    const a = await call(IpcChannels.AgentEarSetEnabled, false)
    const b = await call(IpcChannels.AgentEarUnpair)
    for (const res of [a, b]) {
      expect(res.ok).toBe(false)
      expect(res.status).toBe(404)
      expect(res.data.error.code).toBe('not_paired')
    }
    expect(showMessageBox).not.toHaveBeenCalled()
    expect(seen).toHaveLength(0)
  })
  it('the unpair dialog says AgentEar must be paired again; the disable dialog says pairing is kept', async () => {
    showMessageBox.mockResolvedValue({ response: 1 })
    await call(IpcChannels.AgentEarUnpair)
    await call(IpcChannels.AgentEarSetEnabled, false)
    await call(IpcChannels.AgentEarSetEnabled, true)
    const [unpair, disable, enable] = showMessageBox.mock.calls.map((c) => c.at(-1) as { message: string; detail: string; buttons: string[]; cancelId: number; defaultId: number })
    expect(unpair!.message).toContain('Unpair AgentEar')
    expect(unpair!.detail).toContain('pair AgentEar again')
    expect(disable!.message).toContain('Disable AgentEar')
    expect(disable!.detail).toContain('pairing is kept')
    expect(enable!.message).toContain('Enable AgentEar')
    // Cancel is the default and the Escape answer.
    for (const opts of [unpair!, disable!, enable!]) {
      expect(opts.defaultId).toBe(1)
      expect(opts.cancelId).toBe(1)
    }
  })
  it('the dialogs are translated (zh, th)', async () => {
    showMessageBox.mockResolvedValue({ response: 1 })
    await call(IpcChannels.AppSetLocale, 'zh')
    await call(IpcChannels.AgentEarUnpair)
    await call(IpcChannels.AppSetLocale, 'th')
    await call(IpcChannels.AgentEarUnpair)
    await call(IpcChannels.AppSetLocale, 'en')
    const [zh, th] = showMessageBox.mock.calls.map((c) => c.at(-1) as { message: string; detail: string })
    expect(zh!.message).toContain('解除')
    expect(zh!.detail).toContain('重新配对')
    expect(th!.message).toContain('ยกเลิกการจับคู่')
    expect(th!.detail).toContain('จับคู่ใหม่')
  })
  it.each([
    [404, 'not_found', 404, 'not_paired'],
    [403, 'presence_required', 403, 'forbidden'],
    [401, 'unauthorized', 403, 'forbidden'],
    [500, 'internal', 502, 'daemon_error'],
    [400, 'invalid_request', 502, 'daemon_error'],
  ])('daemon %i %s -> stable error code', async (status, code, wantStatus, wantCode) => {
    attachedFailure = { status, code }
    showMessageBox.mockResolvedValue({ response: 0 })
    for (const res of [await call(IpcChannels.AgentEarSetEnabled, false), await call(IpcChannels.AgentEarUnpair)]) {
      expect(res.ok).toBe(false)
      expect(res.status).toBe(wantStatus)
      expect(res.data.error.code).toBe(wantCode)
      expect(typeof res.data.error.message).toBe('string')
    }
  })
  it('daemon unreachable -> backend error code passes through, never a raw throw', async () => {
    const saved = endpoint.port
    endpoint.port = 1
    try {
      const res = await call(IpcChannels.AgentEarUnpair)
      expect(res.ok).toBe(false)
      expect(['proxy_failed', 'backend_not_ready', 'timeout']).toContain(res.data.error.code)
      expect(showMessageBox).not.toHaveBeenCalled()
    } finally {
      endpoint.port = saved
    }
  })
  it('concurrent calls: one dialog, the rest are busy at once and send nothing', async () => {
    let release!: (v: { response: number }) => void
    showMessageBox.mockReturnValue(new Promise((r) => { release = r }))
    const first = call(IpcChannels.AgentEarUnpair)
    const second = call(IpcChannels.AgentEarUnpair)
    const third = call(IpcChannels.AgentEarSetEnabled, false)
    const fourth = call(IpcChannels.OsModuleSetEnabled, 'sin90', true)
    for (const p of [second, third, fourth]) expect((await p).data.error.code).toBe('busy')
    await vi.waitFor(() => expect(showMessageBox).toHaveBeenCalledTimes(1))
    release({ response: 0 })
    expect((await first).ok).toBe(true)
    expect(showMessageBox).toHaveBeenCalledTimes(1)
    expect(seen.map((s) => [s.method, s.url])).toEqual([['DELETE', '/api/v1/attached/agentear']])
    // Released afterwards.
    showMessageBox.mockResolvedValue({ response: 1 })
    expect((await call(IpcChannels.AgentEarSetEnabled, true)).data.error.code).toBe('cancelled')
  })
})

describe('sensitive IPCs share one in-flight slot (no dialog stacking)', () => {
  it('two concurrent calls open one dialog; the second is busy immediately', async () => {
    let release!: (v: { response: number }) => void
    showMessageBox.mockReturnValue(new Promise((r) => { release = r }))
    const first = call(IpcChannels.ApprovalDecide, 'ap_1', { type: 'approve' })
    const second = call(IpcChannels.OsModuleSetEnabled, 'sin90', true)
    const third = call(IpcChannels.ApprovalDecide, 'ap_1', { type: 'approve' })
    expect((await second).data.error.code).toBe('busy')
    expect((await third).data.error.code).toBe('busy')
    // Let the first reach its dialog, then confirm it.
    await vi.waitFor(() => expect(showMessageBox).toHaveBeenCalledTimes(1))
    release({ response: 1 })
    expect((await first).data.error.code).toBe('cancelled')
    expect(showMessageBox).toHaveBeenCalledTimes(1)
  })
  it('the slot is released afterwards (also after a refusal)', async () => {
    await call(IpcChannels.ApprovalDecide, 'ap_missing', { type: 'approve' })
    showMessageBox.mockResolvedValue({ response: 1 })
    const res = await call(IpcChannels.ApprovalDecide, 'ap_1', { type: 'approve' })
    expect(res.data.error.code).toBe('cancelled')
  })
})

describe('A3 sender guard on every handler', () => {
  it('rejects a call whose senderFrame is not the main frame', async () => {
    const h = handlers.get(IpcChannels.OmlxStop)!
    await expect(Promise.resolve().then(() => h({ senderFrame: { url: APP_URL } }))).rejects.toThrow(/untrusted/)
  })
  it('rejects a call from a foreign URL even on the main frame', async () => {
    const h = handlers.get(IpcChannels.BackendProxy)!
    const evil = { url: 'https://evil.example/' }
    const win = mainWindow.webContents
    const saved = win.mainFrame
    win.mainFrame = evil
    try {
      await expect(Promise.resolve().then(() => h({ senderFrame: evil }, { method: 'GET', path: '/api/v1/runs' }))).rejects.toThrow(/untrusted/)
    } finally {
      win.mainFrame = saved
    }
    expect(seen).toHaveLength(0)
  })
  it('rejects a null senderFrame', async () => {
    const h = handlers.get(IpcChannels.AppPing)!
    await expect(Promise.resolve().then(() => h({ senderFrame: null }))).rejects.toThrow(/untrusted/)
  })
  it('every registered channel is guarded', async () => {
    for (const [channel, h] of handlers) {
      await expect(Promise.resolve().then(() => h({ senderFrame: null }))).rejects.toThrow(/untrusted/)
      expect(channel).toBeTruthy()
    }
  })
})

describe('A4 oMLX IPC hardening', () => {
  const badUrls = [
    'http://evil.example.com:8088',
    'http://127.0.0.1.evil.com:8088',
    'http://user:pw@127.0.0.1:8088',
    'ftp://127.0.0.1:8088',
    'file:///etc/passwd',
    'not a url',
    '',
    42,
  ]
  it.each(badUrls)('OmlxModels rejects %j and sends nothing', async (url) => {
    const res = await call(IpcChannels.OmlxModels, url, 'key')
    expect(res.ok).toBe(false)
    expect(res.error).toMatch(/invalid url/)
  })
  it.each(badUrls)('OmlxWarmup rejects %j', async (url) => {
    const res = await call(IpcChannels.OmlxWarmup, url, 'key', 'model')
    expect(res.ok).toBe(false)
    expect(res.error).toMatch(/invalid url/)
  })
  it('OmlxModels with a CRLF api key resolves forbidden instead of throwing', async () => {
    const res = await call(IpcChannels.OmlxModels, `http://127.0.0.1:${endpoint.port}`, 'k\r\nX-Evil: 1')
    expect(res.ok).toBe(false)
    expect(res.error).toMatch(/forbidden/)
    expect(seen).toHaveLength(0)
  })
  it('OmlxModels accepts loopback and still reaches it with the key', async () => {
    const res = await call(IpcChannels.OmlxModels, `http://127.0.0.1:${endpoint.port}`, 'k')
    expect(res.ok).toBe(true)
    expect(res.models).toEqual(['m1'])
    expect(seen[0]?.auth).toBe('Bearer k')
  })
  it.each([0, -1, 65536, 1.5, '8080; rm -rf', NaN, null])('OmlxStart rejects port %j without spawning', async (port) => {
    const res = await call(IpcChannels.OmlxStart, port === null ? 'x' : port, 'secret-key')
    expect(res.ok).toBe(false)
    expect(execFileMock).not.toHaveBeenCalled()
  })
  it('OmlxStart passes the api key via env, never argv', async () => {
    const res = await call(IpcChannels.OmlxStart, 8123, 'secret-key-123')
    expect(res.ok).toBe(true)
    expect(execFileMock).toHaveBeenCalledTimes(1)
    const [cmd, args, opts] = execFileMock.mock.calls[0] as [string, string[], { env: Record<string, string> }]
    expect(cmd).toBe('omlx')
    expect(args.join(' ')).not.toContain('secret-key-123')
    expect(args).not.toContain('--api-key')
    expect(args).toEqual(expect.arrayContaining(['serve', '--port', '8123']))
    expect(opts.env['OMLX_API_KEY']).toBe('secret-key-123')
  })
  it('the legacy hardcoded default key is gone from the source tree', async () => {
    const { execFileSync } = await vi.importActual<typeof import('node:child_process')>('node:child_process')
    const needle = 'xiao' + 'bao8088'
    let out = ''
    try {
      out = execFileSync('grep', ['-rn', needle, 'src'], { cwd: process.cwd(), encoding: 'utf8' })
    } catch { /* grep exit 1 = no match */ }
    // This test file builds the needle by concatenation, so it never matches itself.
    expect(out).toBe('')
  })
})

describe('FU-114 OmlxStop only kills the process this app started', () => {
  function alive(pid: number): boolean {
    try {
      process.kill(pid, 0)
      return true
    } catch {
      return false
    }
  }

  /** Fields ChildProcess marks readonly; the fakes assign them. */
  type MutableChild = EventEmitter & {
    pid: number
    exitCode: number | null
    signalCode: NodeJS.Signals | null
    kill: ChildProcess['kill']
    signals: Array<NodeJS.Signals | number | undefined>
  }

  function asChild(child: MutableChild): ChildProcess {
    return child as unknown as ChildProcess
  }

  /** Exits on the first signal, so stop resolves without waiting out the grace. */
  function dyingChild(): ChildProcess {
    const child: MutableChild = Object.assign(new EventEmitter(), {
      pid: 42,
      exitCode: null as number | null,
      signalCode: null as NodeJS.Signals | null,
      signals: [] as Array<NodeJS.Signals | number | undefined>,
      kill: (() => true) as ChildProcess['kill'],
    })
    child.kill = (() => {
      setImmediate(() => child.emit('exit', null, 'SIGTERM'))
      return true
    }) as ChildProcess['kill']
    return asChild(child)
  }

  /** Records signals and stays up until the test emits `exit`. */
  function heldChild(pid: number): MutableChild {
    const child: MutableChild = Object.assign(new EventEmitter(), {
      pid,
      exitCode: null as number | null,
      signalCode: null as NodeJS.Signals | null,
      signals: [] as Array<NodeJS.Signals | number | undefined>,
      kill: (() => true) as ChildProcess['kill'],
    })
    child.kill = ((signal?: NodeJS.Signals | number) => {
      child.signals.push(signal)
      return true
    }) as ChildProcess['kill']
    return child
  }

  function spawnNode(script: string, extraArgs: string[] = []): ChildProcess {
    // stdout stays paused so `untilReady` still sees the buffered "ready" line.
    return spawn(process.execPath, ['-e', script, ...extraArgs], { stdio: ['ignore', 'pipe', 'ignore'] })
  }

  async function untilReady(child: ChildProcess): Promise<void> {
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('child did not become ready')), 2_000)
      const onData = (buf: Buffer): void => {
        if (!buf.toString().includes('ready')) return
        clearTimeout(timer)
        child.stdout?.off('data', onData)
        resolve()
      }
      child.stdout?.on('data', onData)
      child.once('exit', () => {
        clearTimeout(timer)
        reject(new Error('child exited before ready'))
      })
    })
  }

  function killQuiet(pid: number | undefined): void {
    if (pid == null) return
    try { process.kill(pid, 'SIGKILL') } catch { /* already gone */ }
  }

  it('OmlxStop never invokes pkill', async () => {
    expect(await call(IpcChannels.OmlxStop)).toEqual({ ok: true, stopped: false })
    omlxExecFile.current = () => dyingChild()
    await call(IpcChannels.OmlxStart, 8123, 'k')
    expect(await call(IpcChannels.OmlxStop)).toEqual({ ok: true, stopped: true })
    expect(execFileMock.mock.calls.map((c) => c[0])).not.toContain('pkill')
  })

  it('does not call pkill when the slot is empty', async () => {
    // pkill is mocked and never runs (see the execFile mock). This locks
    // "OmlxStop does not invoke pkill", not that a real pkill would spare a
    // process. The child below only shows this handler sent it no signal.
    // Its argv contains "omlx serve", which `pkill -f 'omlx serve'` would match.
    const external = spawnNode(
      'process.stdout.write("ready\\n"); setInterval(() => {}, 1000)',
      ['omlx', 'serve'],
    )
    try {
      await untilReady(external)
      expect(external.pid).toEqual(expect.any(Number))
      expect(alive(external.pid!)).toBe(true)
      expect(await call(IpcChannels.OmlxStop)).toEqual({ ok: true, stopped: false })
      expect(alive(external.pid!)).toBe(true)
      expect(execFileMock.mock.calls.map((c) => c[0])).not.toContain('pkill')
    } finally {
      killQuiet(external.pid)
    }
  })

  it('stops the process this app spawned', async () => {
    const owned: { current: ChildProcess | null } = { current: null }
    omlxExecFile.current = () => {
      owned.current = spawnNode('process.stdout.write("ready\\n"); setInterval(() => {}, 1000)')
      return owned.current
    }
    try {
      await call(IpcChannels.OmlxStart, 8123, 'k')
      const child = owned.current
      if (!child?.pid) throw new Error('owned child missing')
      await untilReady(child)
      const started = Date.now()
      expect(await call(IpcChannels.OmlxStop)).toEqual({ ok: true, stopped: true })
      expect(alive(child.pid)).toBe(false)
      // Died on SIGTERM, inside the grace window.
      expect(Date.now() - started).toBeLessThan(5_000)
    } finally {
      killQuiet(owned.current?.pid)
    }
  })

  it('escalates to SIGKILL when SIGTERM is ignored', async () => {
    const graceMs = 400
    setOmlxStopGraceMsForTests(graceMs)
    const owned: { current: ChildProcess | null } = { current: null }
    omlxExecFile.current = () => {
      owned.current = spawnNode('process.on("SIGTERM", () => {}); process.stdout.write("ready\\n"); setInterval(() => {}, 1000)')
      return owned.current
    }
    try {
      await call(IpcChannels.OmlxStart, 8123, 'k')
      const child = owned.current
      if (!child?.pid) throw new Error('owned child missing')
      await untilReady(child)
      const started = Date.now()
      expect(await call(IpcChannels.OmlxStop)).toEqual({ ok: true, stopped: true })
      const elapsed = Date.now() - started
      expect(alive(child.pid)).toBe(false)
      expect(elapsed).toBeGreaterThanOrEqual(graceMs - 50)
    } finally {
      killQuiet(owned.current?.pid)
    }
  })

  it('resolves when exit never follows SIGKILL', async () => {
    setOmlxStopGraceMsForTests(20)
    setOmlxKillFallbackMsForTests(30)
    const children: MutableChild[] = []
    omlxExecFile.current = () => {
      const child = heldChild(7101)
      children.push(child)
      return asChild(child)
    }
    await call(IpcChannels.OmlxStart, 8123, 'k')
    const started = Date.now()
    expect(await call(IpcChannels.OmlxStop)).toEqual({ ok: true, stopped: true })
    expect(children[0]!.signals).toEqual(['SIGTERM', 'SIGKILL'])
    expect(Date.now() - started).toBeLessThan(1_000)
  })

  it('treats kill() returning false as the pid already being gone', async () => {
    const children: MutableChild[] = []
    omlxExecFile.current = () => {
      const child = heldChild(7102)
      child.kill = (() => false) as ChildProcess['kill']
      children.push(child)
      return asChild(child)
    }
    await call(IpcChannels.OmlxStart, 8123, 'k')
    const started = Date.now()
    expect(await call(IpcChannels.OmlxStop)).toEqual({ ok: true, stopped: true })
    expect(Date.now() - started).toBeLessThan(500)
    expect(children[0]!.signals).toEqual([])
  })

  it('start during stop spawns only after the old process exits', async () => {
    setOmlxStopGraceMsForTests(400)
    const children: MutableChild[] = []
    omlxExecFile.current = () => {
      const child = heldChild(7_000 + children.length)
      children.push(child)
      return asChild(child)
    }
    await call(IpcChannels.OmlxStart, 8123, 'k')
    const stopping = call(IpcChannels.OmlxStop)
    await vi.waitFor(() => expect(children[0]!.signals).toContain('SIGTERM'))
    const starting = call(IpcChannels.OmlxStart, 8124, 'k')
    await new Promise((r) => setTimeout(r, 40))
    expect(children).toHaveLength(1)
    children[0]!.emit('exit', null, 'SIGTERM')
    expect(await stopping).toEqual({ ok: true, stopped: true })
    expect(await starting).toMatchObject({ ok: true })
    expect(children).toHaveLength(2)
    expect(children[1]!.signals).toEqual([])
    const stoppingNext = call(IpcChannels.OmlxStop)
    await vi.waitFor(() => expect(children[1]!.signals).toContain('SIGTERM'))
    children[1]!.emit('exit', null, 'SIGTERM')
    expect(await stoppingNext).toEqual({ ok: true, stopped: true })
  })

  it('before-quit sends SIGTERM before the caller awaits, and bounds the wait', async () => {
    expect(OMLX_QUIT_STOP_BUDGET_MS).toBe(5_000 + 2_000 + 1_000)
    expect(OMLX_QUIT_STOP_BUDGET_MS).toBeLessThan(22_000)
    setOmlxStopGraceMsForTests(20)
    setOmlxKillFallbackMsForTests(30)
    const children: MutableChild[] = []
    omlxExecFile.current = () => {
      const child = heldChild(7103)
      children.push(child)
      return asChild(child)
    }
    await call(IpcChannels.OmlxStart, 8123, 'k')
    const pending = stopOwnedOmlx()
    expect(children[0]!.signals[0]).toBe('SIGTERM')
    children[0]!.emit('exit', null, 'SIGTERM')
    expect(await pending).toBe(true)
    omlxExecFile.current = () => {
      const child = heldChild(7104)
      children.push(child)
      return asChild(child)
    }
    await call(IpcChannels.OmlxStart, 8124, 'k')
    emitApp('before-quit')
    expect(children[1]!.signals[0]).toBe('SIGTERM')
    const started = Date.now()
    const quit = ensureOwnedOmlxQuit()
    children[1]!.emit('exit', null, 'SIGTERM')
    await quit
    expect(Date.now() - started).toBeLessThan(1_000)
  })

  // PR#1 F1 (ruling 2026-11-13-pr1-omlx-quit-resident, A): with the resident
  // service installed the daemon keeps running after the app quits and uses
  // this oMLX, so quitting leaves it running; the user's own stop still stops it.
  it('leaves the oMLX this app started running on quit when the resident service is installed', async () => {
    keepOmlx.current = true
    const children: MutableChild[] = []
    omlxExecFile.current = () => {
      const child = heldChild(7105)
      children.push(child)
      return asChild(child)
    }
    await call(IpcChannels.OmlxStart, 8123, 'k')
    emitApp('before-quit')
    expect(await ensureOwnedOmlxQuit()).toBe(false)
    expect(await ensureOwnedOmlxQuit()).toBe(false)
    expect(children[0]!.signals).toEqual([])
    // Recorded once, with the port it was started on (ruling 乙).
    expect(keptOmlx).toEqual([{ pid: 7105, port: 8123 }])
    // A quit that keeps stops nothing kept earlier either (ruling A).
    expect(keptStop.calls).toBe(0)
    const stopping = call(IpcChannels.OmlxStop)
    await vi.waitFor(() => expect(children[0]!.signals).toContain('SIGTERM'))
    children[0]!.emit('exit', null, 'SIGTERM')
    expect(await stopping).toEqual({ ok: true, stopped: true })
  })

  it('reads whether to keep it at the quit, not when the handlers were registered', async () => {
    keepOmlx.current = true
    const children: MutableChild[] = []
    omlxExecFile.current = () => {
      const child = heldChild(7106)
      children.push(child)
      return asChild(child)
    }
    await call(IpcChannels.OmlxStart, 8123, 'k')
    // 「完全退出」 removed the LaunchAgent before it quits.
    keepOmlx.current = false
    emitApp('before-quit')
    expect(children[0]!.signals[0]).toBe('SIGTERM')
    const quit = ensureOwnedOmlxQuit()
    children[0]!.emit('exit', null, 'SIGTERM')
    expect(await quit).toBe(true)
    // Stopped, so nothing is left to record.
    expect(keptOmlx).toEqual([])
  })

  // Ruling 2026-11-13-pr1-omlx-kept-deactivate (A): a quit with the plist gone
  // stops what earlier quits left running, once, inside the same budget.
  it('a quit that stops also stops what earlier quits left running, once for all its entries', async () => {
    let release = () => {}
    keptStop.impl = () => new Promise<void>((r) => { release = r })
    emitApp('before-quit')
    const quits = [ensureOwnedOmlxQuit(), ensureOwnedOmlxQuit()]
    expect(keptStop.calls).toBe(1)
    release()
    expect(await Promise.all(quits)).toEqual([true, true])
  })

  it('the quit waits for the stop of what earlier quits left running, but no longer than its budget', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    try {
      keptStop.impl = () => new Promise<void>(() => {})
      let settled: boolean | null = null
      void ensureOwnedOmlxQuit().then((v) => { settled = v })
      await vi.advanceTimersByTimeAsync(OMLX_QUIT_STOP_BUDGET_MS - 1)
      expect(settled).toBeNull()
      await vi.advanceTimersByTimeAsync(1)
      expect(settled).toBe(false)
      expect(keptStop.calls).toBe(1)
    } finally {
      vi.useRealTimers()
    }
  })

  it('a quit that stops goes on when stopping what earlier quits left running throws or fails', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    keptStop.impl = () => { throw new Error('ps failed') }
    expect(await ensureOwnedOmlxQuit()).toBe(true)
    resetOmlxProcessForTests()
    keptStop.impl = () => Promise.reject(new Error('lsof failed'))
    expect(await ensureOwnedOmlxQuit()).toBe(true)
    expect(keptStop.calls).toBe(2)
  })

  it('a quit that keeps goes on when recording it fails', async () => {
    keepOmlx.current = true
    rememberThrows.current = true
    omlxExecFile.current = () => asChild(heldChild(7109))
    await call(IpcChannels.OmlxStart, 8123, 'k')
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    expect(() => emitApp('before-quit')).not.toThrow()
    expect(await ensureOwnedOmlxQuit()).toBe(false)
  })

  it('an older child exiting late does not clear the port of the one started after it', async () => {
    setOmlxStopGraceMsForTests(10)
    setOmlxKillFallbackMsForTests(10)
    const children: MutableChild[] = []
    omlxExecFile.current = () => {
      const child = heldChild(7110 + children.length)
      children.push(child)
      return asChild(child)
    }
    await call(IpcChannels.OmlxStart, 8123, 'k')
    // It ignores both signals: the stop gives up after the kill fallback.
    expect(await call(IpcChannels.OmlxStop)).toEqual({ ok: true, stopped: true })
    await call(IpcChannels.OmlxStart, 8124, 'k')
    expect(children).toHaveLength(2)
    children[0]!.emit('exit', null, 'SIGKILL')
    keepOmlx.current = true
    expect(await ensureOwnedOmlxQuit()).toBe(false)
    expect(keptOmlx).toEqual([{ pid: 7111, port: 8124 }])
  })

  it('records nothing on a quit that keeps when this app started no oMLX, or it already exited', async () => {
    keepOmlx.current = true
    emitApp('before-quit')
    expect(await ensureOwnedOmlxQuit()).toBe(false)
    expect(keptOmlx).toEqual([])
    const children: MutableChild[] = []
    omlxExecFile.current = () => {
      const child = heldChild(7108)
      children.push(child)
      return asChild(child)
    }
    await call(IpcChannels.OmlxStart, 8123, 'k')
    children[0]!.emit('exit', 0, null)
    expect(await ensureOwnedOmlxQuit()).toBe(false)
    expect(keptOmlx).toEqual([])
  })

  it('a stop the quit already started is waited for, whatever is read after it', async () => {
    const children: MutableChild[] = []
    omlxExecFile.current = () => {
      const child = heldChild(7107)
      children.push(child)
      return asChild(child)
    }
    await call(IpcChannels.OmlxStart, 8123, 'k')
    emitApp('before-quit')
    expect(children[0]!.signals[0]).toBe('SIGTERM')
    keepOmlx.current = true
    let settled = false
    const quit = ensureOwnedOmlxQuit().then((v) => { settled = true; return v })
    await new Promise((r) => setTimeout(r, 10))
    expect(settled).toBe(false)
    children[0]!.emit('exit', null, 'SIGTERM')
    expect(await quit).toBe(true)
  })
})

// The node-daemon-era `modules:*` IPC (A5 on main) was deleted outright by
// REV-01 on ab/core: its tests went with the handlers.

describe('UI-02 chat history IPC', () => {
  it('saves, lists, reads, deletes and clears through the guarded handlers', async () => {
    fs.rmSync(path.join(settingsDir, 'chat-history'), { recursive: true, force: true })
    const record = {
      format: 'agent24.desktop.chat-history', version: 3, id: 'chat_ipc_1', title: '你好',
      title_source: 'prompt', group_id: null, archived_at: null, runs: [], pending: [], initiator: 'user', unseen: [],
      created_at: '2026-11-08T00:00:00.000Z', updated_at: '2026-11-08T00:00:01.000Z',
      messages: [{ role: 'user', content: '你好', at: '2026-11-08T00:00:00.000Z', delivered: true }],
    }
    expect(await call(IpcChannels.ChatHistoryList)).toEqual([])
    const summary = { id: 'chat_ipc_1', title: '你好', title_source: 'prompt', group_id: null, archived_at: null, created_at: '2026-11-08T00:00:00.000Z', updated_at: '2026-11-08T00:00:01.000Z', first_prompt: '你好', tasks: [], pending: [], initiator: 'user', unseen: [] }
    expect(await call(IpcChannels.ChatHistorySave, record)).toEqual(summary)
    expect(await call(IpcChannels.ChatHistoryList)).toEqual([summary])
    expect(await call(IpcChannels.ChatHistoryGet, 'chat_ipc_1')).toEqual(record)
    expect(await call(IpcChannels.ChatHistorySetTitle, 'chat_ipc_1', '问候', 'user')).toEqual({ ...summary, title: '问候', title_source: 'user' })
    expect(await call(IpcChannels.ChatHistorySetTitle, 'chat_ipc_1', '模型', 'model')).toEqual({ ...summary, title: '问候', title_source: 'user' })
    await expect(call(IpcChannels.ChatHistorySetTitle, 'chat_ipc_1', '名', 'prompt')).rejects.toThrow('Invalid chat history title source')
    expect(await call(IpcChannels.ChatHistorySetTitle, 'chat_ipc_none', '名', 'user')).toBeNull()
    // M6 C1: marking a conversation seen goes through the same guarded handlers.
    expect(await call(IpcChannels.ChatHistoryMarkSeen, 'chat_ipc_1')).toEqual({ ...summary, title: '问候', title_source: 'user' })
    expect(await call(IpcChannels.ChatHistoryMarkSeen, 'chat_ipc_none')).toBeNull()
    await expect(call(IpcChannels.ChatHistoryMarkSeen, '../x')).rejects.toThrow('Invalid chat history id')
    // UI-06: groups and archiving go through the same guarded handlers.
    const { groups: [group] } = await call(IpcChannels.ChatGroupsCreate, '工作') as { groups: Array<{ id: string; name: string }> }
    expect(group).toMatchObject({ name: '工作', collapsed: false })
    expect(await call(IpcChannels.ChatHistoryOrganize, 'chat_ipc_1', { group_id: group.id, archived: true }))
      .toMatchObject({ group_id: group.id, archived_at: expect.stringMatching(/Z$/) })
    await expect(call(IpcChannels.ChatHistoryOrganize, 'chat_ipc_1', { group_id: 'grp_none' })).rejects.toThrow('Unknown chat group')
    expect(await call(IpcChannels.ChatGroupsRename, group.id, '私事')).toMatchObject({ groups: [{ name: '私事' }] })
    expect(await call(IpcChannels.ChatGroupsSetCollapsed, 'assistant', true)).toMatchObject({ assistant_collapsed: true })
    expect(await call(IpcChannels.ChatGroupsDelete, group.id)).toMatchObject({ groups: [] })
    expect(await call(IpcChannels.ChatGroupsGet)).toMatchObject({ groups: [], assistant_collapsed: true, archive_collapsed: true })
    await expect(call(IpcChannels.ChatHistorySave, { ...record, version: 9 })).rejects.toThrow('Invalid chat history record')
    await expect(call(IpcChannels.ChatHistoryGet, '../ui-prefs')).rejects.toThrow('Invalid chat history id')
    await call(IpcChannels.ChatHistoryDelete, 'chat_ipc_1')
    expect(await call(IpcChannels.ChatHistoryGet, 'chat_ipc_1')).toBeNull()
    await call(IpcChannels.ChatHistorySave, record)
    await call(IpcChannels.ChatHistoryClear)
    expect(await call(IpcChannels.ChatHistoryList)).toEqual([])
    fs.rmSync(path.join(settingsDir, 'chat-history'), { recursive: true, force: true })
  })
})

describe('D1 navigation preferences IPC', () => {
  it('reads defaults, persists valid preferences and recovers from a damaged file', async () => {
    expect(await call(IpcChannels.NavPrefsGet)).toEqual({ pinned: ['documents', 'design', 'create'], slotCount: 3, showOverview: true })
    const prefs = { pinned: ['voice'], slotCount: 2, showOverview: false }
    await call(IpcChannels.NavPrefsSet, prefs)
    expect(await call(IpcChannels.NavPrefsGet)).toEqual(prefs)
    await expect(call(IpcChannels.NavPrefsSet, { pinned: [2] })).rejects.toThrow('Invalid navigation preferences')
    expect(await call(IpcChannels.NavPrefsGet)).toEqual(prefs)
    fs.writeFileSync(path.join(settingsDir, 'ui-prefs.json'), 'broken')
    expect((await call(IpcChannels.NavPrefsGet)).slotCount).toBe(3)
    expect(seen).toHaveLength(0)
  })

  it('allows only read-only manifest discovery through the proxy', async () => {
    expect((await call(IpcChannels.BackendProxy, { method: 'GET', path: '/api/modules' })).ok).toBe(true)
    expect(seen).toMatchObject([{ method: 'GET', url: '/api/modules' }])
  })

  it('refuses preference changes from an untrusted frame', async () => {
    const write = handlers.get(IpcChannels.NavPrefsSet)
    expect(write).toBeDefined()
    await expect(Promise.resolve().then(() => write?.({ senderFrame: { url: 'https://evil.example' } }, { pinned: [], slotCount: 1, showOverview: false }))).rejects.toThrow()
    expect(fs.existsSync(path.join(settingsDir, 'ui-prefs.json'))).toBe(false)
  })
})

describe('backend:endpoint hands the renderer the port only (KB-HOST-01 PR-A1)', () => {
  it('returns exactly {port}, with no credential, while main holds one', async () => {
    const result = await call(IpcChannels.BackendEndpoint)
    expect(result).toEqual({ port: endpoint.port })
    expect(JSON.stringify(result)).not.toContain(endpoint.token)
  })
})

describe('the generic backend proxy never reaches the presence channel (CORE-2 AgentEar S3)', () => {
  it.each([
    ['GET', '/api/v1/presence/challenges/next?wait=25'],
    ['POST', '/api/v1/presence/challenges/abc123'],
    ['GET', '/api/v1/presence'],
    ['GET', '/API/v1/Presence/challenges/next'],
    ['GET', '/api/v1/./presence/challenges/next'],
    ['GET', '/api/v1/models/../presence/challenges/next'],
    ['GET', '/api/v1//presence/challenges/next'],
    ['GET', '/api/v1/%70resence/challenges/next'],
    ['POST', '/api/v1/presence%2Fchallenges%2Fabc'],
    ['POST', '/api/v1\\presence/challenges/abc'],
    ['POST', '/presence/challenges/abc'],
  ] as const)('refuses %s %s without sending it', async (method, path) => {
    const res = await call(IpcChannels.BackendProxy, { method, path })
    expect(res.status).toBe(403)
    expect(JSON.stringify(res.data)).toContain('presence_channel_private')
    expect(seen).toHaveLength(0)
  })

  it.each(['/api/v1/presences', '/api/v1/sessions?next=/api/v1/presence/x', '/api/v1/models'])('does not flag %s', (path) => {
    expect(reachesPresenceChannel(path)).toBe(false)
  })
})

describe('the generic backend proxy never reaches the host interface (KB-HOST-01 PR-B1a)', () => {
  it.each([
    '/api/v1/host/knowledge/bindings/b1?seq=1',
    '/api/v1/host',
    '/API/v1/Host/knowledge/bindings/b1',
    '/api/v1/./host/knowledge/bindings/b1',
    '/api/v1/models/../host/knowledge/bindings/b1',
    '/api/v1//host/knowledge/bindings/b1',
    '/api/v1/%68ost/knowledge/bindings/b1',
    '/api/v1/host%2Fknowledge',
    '/api/v1\\host/knowledge/bindings/b1',
    // Encoded `/` and `..` inside the id: the daemon routes the raw path.
    '/api/v1/host/knowledge/bindings/..%2F..%2F..%2Fx?seq=1',
  ])('refuses %s without sending it', async (path) => {
    const res = await call(IpcChannels.BackendProxy, { method: 'PUT', path })
    expect(res.status).toBe(403)
    expect(JSON.stringify(res.data)).toContain('host_interface_private')
    expect(seen).toHaveLength(0)
  })

  it.each(['/api/v1/hosts', '/api/v1/sessions?next=/api/v1/host/x', '/api/v1/tool-overrides/foo%'])('still proxies %s', async (path) => {
    expect(reachesHostInterface(path)).toBe(false)
  })
})


describe('oMLX 401 and the key from ~/.omlx/settings.json', () => {
  const SECRET = 'sk-omlx-from-settings-9f3a'
  const url = () => `http://127.0.0.1:${endpoint.port}`
  const writeSettings = (obj: unknown, mode = 0o600) => {
    fs.writeFileSync(settingsFile, JSON.stringify(obj), { mode })
    fs.chmodSync(settingsFile, mode)
  }
  const goodSettings = () => ({ auth: { api_key: SECRET }, server: { host: '127.0.0.1', port: endpoint.port } })

  it('OmlxModels on 401 reports authRequired (running, needs a key), not a generic failure', async () => {
    modelsRequireKey = 'right-key'
    const res = await call(IpcChannels.OmlxModels, url(), '')
    expect(res).toMatchObject({ ok: false, authRequired: true })
    const wrong = await call(IpcChannels.OmlxModels, url(), 'wrong-key')
    expect(wrong).toMatchObject({ ok: false, authRequired: true })
    const right = await call(IpcChannels.OmlxModels, url(), 'right-key')
    expect(right).toMatchObject({ ok: true, models: ['m1'] })
    expect(right.authRequired).toBeUndefined()
  })

  it('a server that is simply down is not reported as authRequired', async () => {
    const res = await call(IpcChannels.OmlxModels, 'http://127.0.0.1:1', '')
    expect(res).toMatchObject({ ok: false })
    expect(res.authRequired).toBeUndefined()
  })

  it('detect: keyed server and no settings file -> authRequired with the url', async () => {
    modelsRequireKey = SECRET
    const res = await call(IpcChannels.OmlxDetect)
    expect(res).toEqual({ authRequired: true, url: url() })
    expect(seen.length).toBeGreaterThan(0)
    expect(seen.every((r) => r.url === '/v1/models' && r.auth === undefined)).toBe(true)
  })

  it('detect: uses the settings.json key, connects, and the key is NOT in the IPC result or any log', async () => {
    modelsRequireKey = SECRET
    writeSettings(goodSettings())
    const spies = (['log', 'info', 'warn', 'error', 'debug'] as const).map((m) => vi.spyOn(console, m).mockImplementation(() => {}))
    try {
      const res = await call(IpcChannels.OmlxDetect)
      expect(res).toMatchObject({ url: url(), models: ['m1'], apiKey: '' })
      expect(JSON.stringify(res)).not.toContain(SECRET)
      // the key really was sent to the verified origin (and only as a header)
      expect(seen.some((r) => r.auth === `Bearer ${SECRET}`)).toBe(true)
      expect(seen.every((r) => !(r.url ?? '').includes(SECRET) && !r.body.includes(SECRET))).toBe(true)

      // follow-up calls the renderer makes with an empty key still work, still return no key
      const models = await call(IpcChannels.OmlxModels, url(), '')
      expect(models).toMatchObject({ ok: true, models: ['m1'] })
      expect(JSON.stringify(models)).not.toContain(SECRET)
      const warm = await call(IpcChannels.OmlxWarmup, url(), '', 'm1')
      expect(JSON.stringify(warm)).not.toContain(SECRET)
      const llm = await call(IpcChannels.LlmStatus)
      expect(JSON.stringify(llm)).not.toContain(SECRET)

      for (const spy of spies) {
        expect(JSON.stringify(spy.mock.calls)).not.toContain(SECRET)
      }
    } finally {
      spies.forEach((spy) => spy.mockRestore())
    }
  })

  it('the key goes only to the exact address settings.json names (not localhost, not another port)', async () => {
    modelsRequireKey = SECRET
    writeSettings(goodSettings())
    const res = await call(IpcChannels.OmlxModels, `http://localhost:${endpoint.port}`, '')
    expect(res).toMatchObject({ ok: false, authRequired: true })
    expect(seen.length).toBeGreaterThan(0)
    expect(seen.every((r) => r.auth === undefined)).toBe(true)
  })

  it('a rotated key is used at once; a removed settings file stops it being sent', async () => {
    modelsRequireKey = 'k2'
    writeSettings({ auth: { api_key: 'k1' }, server: { port: endpoint.port } })
    expect(await call(IpcChannels.OmlxModels, url(), '')).toMatchObject({ ok: false, authRequired: true })
    writeSettings({ auth: { api_key: 'k2' }, server: { port: endpoint.port } })
    expect(await call(IpcChannels.OmlxModels, url(), '')).toMatchObject({ ok: true })
    fs.rmSync(settingsFile)
    seen = []
    expect(await call(IpcChannels.OmlxModels, url(), '')).toMatchObject({ ok: false, authRequired: true })
    expect(seen.length).toBeGreaterThan(0)
    expect(seen.every((r) => r.auth === undefined)).toBe(true)
  })

  it('a server that echoes the key back as a model id cannot hand it to the renderer', async () => {
    reflectAuthAsModel = true
    writeSettings(goodSettings())
    const models = await call(IpcChannels.OmlxModels, url(), '')
    expect(models).toMatchObject({ ok: true, models: ['m1'] })
    const detect = await call(IpcChannels.OmlxDetect)
    expect(JSON.stringify([models, detect])).not.toContain(SECRET)
  })

  it.each(['base64', 'base64url', 'hex'] as const)('拦截模型 id 中的 %s 编码 key', async (encoding) => {
    const key = 'sk-omlx-~?'
    reflectAuthAsModel = true
    reflectedKeyEncoding = encoding
    writeSettings({ ...goodSettings(), auth: { api_key: key } })
    const models = await call(IpcChannels.OmlxModels, url(), '')
    expect(seen.some((r) => r.auth === `Bearer ${key}`)).toBe(true)
    expect(models).toMatchObject({ ok: true, models: ['m1'] })
    const detect = await call(IpcChannels.OmlxDetect)
    expect(detect).toMatchObject({ models: ['m1'] })
    expect(JSON.stringify([models, detect])).not.toContain(Buffer.from(key).toString(encoding))
  })

  it.each([
    ['group/world writable', () => writeSettings(goodSettings(), 0o666)],
    ['no api_key', () => writeSettings({ auth: {}, server: { port: endpoint.port } })],
    ['empty api_key', () => writeSettings({ auth: { api_key: '  ' }, server: { port: endpoint.port } })],
    ['control chars in key', () => writeSettings({ auth: { api_key: 'a\r\nX-Evil: 1' }, server: { port: endpoint.port } })],
    ['localhost by name', () => writeSettings({ auth: { api_key: SECRET }, server: { host: 'localhost', port: endpoint.port } })],
    ['non-loopback host', () => writeSettings({ auth: { api_key: SECRET }, server: { host: '192.168.2.32', port: endpoint.port } })],
    ['malformed JSON', () => fs.writeFileSync(settingsFile, '{nope', { mode: 0o600 })],
  ])('settings.json ignored (%s): key never sent, result is authRequired', async (_name, setup) => {
    modelsRequireKey = SECRET
    setup()
    const res = await call(IpcChannels.OmlxDetect)
    expect(res).toMatchObject({ authRequired: true, url: url() })
    expect(seen.length).toBeGreaterThan(0)
    expect(seen.every((r) => r.auth === undefined)).toBe(true)
  })

  it('settings.json that is a symlink is ignored', async () => {
    modelsRequireKey = SECRET
    const real = path.join(settingsDir, 'real.json')
    fs.writeFileSync(real, JSON.stringify(goodSettings()), { mode: 0o600 })
    fs.symlinkSync(real, settingsFile)
    const res = await call(IpcChannels.OmlxDetect)
    expect(res).toMatchObject({ authRequired: true, url: url() })
    expect(seen.length).toBeGreaterThan(0)
    expect(seen.every((r) => r.auth === undefined)).toBe(true)
    fs.rmSync(real)
  })
})


describe('app:set-locale', () => {
  it('switches the main-process language for dialogs, ignoring junk', async () => {
    const { getMainLocale, resetMainLocaleForTest } = await import('../main-i18n')
    resetMainLocaleForTest()
    expect(getMainLocale()).toBe('en')
    await call(IpcChannels.AppSetLocale, 'zh')
    expect(getMainLocale()).toBe('zh')
    await call(IpcChannels.AppSetLocale, { evil: true })
    await call(IpcChannels.AppSetLocale, 'fr')
    expect(getMainLocale()).toBe('zh')
    resetMainLocaleForTest()
  })
})
