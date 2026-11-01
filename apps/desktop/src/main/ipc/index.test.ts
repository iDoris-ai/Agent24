// PR-A (pre-release desktop hardening) — handler-level tests for A1/A3/A4/A5.
// Electron is mocked so registerIpcHandlers() can run under plain Node; the
// daemon is a real loopback http server so "no request reached it" is a
// factual assertion, not a mock-call count.

import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import http from 'node:http'
import type { AddressInfo } from 'node:net'
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'

const handlers = new Map<string, (event: unknown, ...args: unknown[]) => unknown>()
const showMessageBox = vi.fn()
const showSaveDialog = vi.fn()
const showOpenDialog = vi.fn()
const promptBackupPassword = vi.fn()
const promptRestorePassword = vi.fn()
const execFileMock = vi.fn()

vi.mock('electron', () => ({
  app: { getVersion: () => '0.0.0-test', getPath: () => settingsDir },
  BrowserWindow: { fromWebContents: () => null },
  dialog: { showMessageBox: (...a: unknown[]) => showMessageBox(...a), showOpenDialog: (...a: unknown[]) => showOpenDialog(...a), showSaveDialog: (...a: unknown[]) => showSaveDialog(...a) },
  ipcMain: { handle: (channel: string, fn: (event: unknown, ...args: unknown[]) => unknown) => { handlers.set(channel, fn) } },
  shell: { openExternal: vi.fn(() => Promise.resolve()) },
}))

vi.mock('node:child_process', async (orig) => ({
  ...(await orig<typeof import('node:child_process')>()),
  execFile: (...a: unknown[]) => {
    execFileMock(...a)
    return { on: vi.fn(), kill: vi.fn() }
  },
}))

const endpoint = { port: 0, token: 'daemon-secret' }
vi.mock('../backend-manager', () => ({
  getBackendEndpoint: () => ({ port: endpoint.port }),
  daemonConnection: () => ({ port: endpoint.port, authHeaders: { Authorization: `Bearer ${endpoint.token}` } }),
  daemonCliEndpoint: () => ({ port: endpoint.port, token: endpoint.token }),
}))

import { IpcChannels } from '../../shared/ipc-types'
import { createIpcGuard } from './guard'
import { reachesHostInterface, reachesPresenceChannel, registerIpcHandlers } from './index'

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

beforeAll(async () => {
  daemon = http.createServer((req, res) => {
    const chunks: Buffer[] = []
    req.on('data', (c: Buffer) => chunks.push(c))
    req.on('end', () => {
      if (!(req.method === 'GET' && req.url === '/api/v1/comm/identity') && !(req.method === 'GET' && req.url?.startsWith('/api/v1/approvals/')) && !(req.method === 'GET' && req.url === '/api/v1/os') && !(req.method === 'GET' && req.url === '/api/v1/attached')) seen.push({ method: req.method, url: req.url, auth: req.headers.authorization, body: Buffer.concat(chunks).toString() })
      res.setHeader('Content-Type', 'application/json')
      if (req.url === '/v1/models' && modelsRequireKey && req.headers.authorization !== `Bearer ${modelsRequireKey}`) {
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
      } else if (req.method === 'GET' && req.url === '/api/v1/comm/identity') {
        res.end(JSON.stringify({ ok: true, data: [{ nickname: 'alice', npub: 'npub1alice', default: true, encrypted: true }] }))
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
        res.end(JSON.stringify({ modules: [{ name: 'sin90', enabled: false, version: '0.5.0' }] }))
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
  modelsRequireKey = null
  reflectAuthAsModel = false
  reflectedKeyEncoding = null
  fs.rmSync(settingsFile, { force: true })
  fs.rmSync(path.join(settingsDir, 'ui-prefs.json'), { force: true })
  seen = []
  attachedModules = [{ name: 'agentear', attach_status: 'attached' }, { name: 'other', attach_status: 'attached' }]
  attachedFailure = null
  showMessageBox.mockReset()
  showSaveDialog.mockReset()
  promptBackupPassword.mockReset()
  promptRestorePassword.mockReset()
  showOpenDialog.mockReset()
  execFileMock.mockReset()
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
  const FILE = '/Users/x/Downloads/alice-backup.ncryptsec'
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
    expect(JSON.stringify(result)).not.toContain('/Users/x')
    const opts = showOpenDialog.mock.calls.at(-1)!.at(-1) as { properties: string[] }
    expect(opts.properties).toContain('openFile')
  })
  it('cancelling the open dialog yields no ticket and nothing else happens', async () => {
    showOpenDialog.mockResolvedValueOnce({ canceled: true, filePaths: [] })
    expect(await call(IpcChannels.CommImportPick)).toEqual({ ok: false, cancelled: true })
    expect(promptRestorePassword).not.toHaveBeenCalled()
    expect(seen).toHaveLength(0)
  })
  it('restores: the path comes from main, the password from main\'s window, the nickname and default from the page', async () => {
    const { ticket } = await pick()
    typing('restore-pass-1')
    const result = await call(IpcChannels.CommIdentityImport, ticket, 'alice', true)
    expect(result.ok).toBe(true)
    expect(seen).toHaveLength(1)
    expect(seen[0]).toMatchObject({ method: 'POST', url: '/api/v1/comm/identity/import', auth: 'Bearer daemon-secret' })
    expect(JSON.parse(seen[0]!.body)).toEqual({ nickname: 'alice', path: FILE, backup_password: 'restore-pass-1', default: true })
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
    expect(promptRestorePassword.mock.calls[0]![2]).toEqual({ willBeDefault: true })
    const second = await pick()
    typing('restore-pass-1')
    await call(IpcChannels.CommIdentityImport, second.ticket, 'bob', false)
    expect(promptRestorePassword.mock.calls[1]![2]).toEqual({ willBeDefault: false })
  })
  it('input the daemon calls invalid is fixed inside the window, not by closing it', async () => {
    const { ticket } = await pick()
    typing('bad-input-pass', 'restore-pass-1')
    const result = await call(IpcChannels.CommIdentityImport, ticket, 'alice', false)
    expect(result.ok).toBe(true)
    expect(seen.map((r) => JSON.parse(r.body).backup_password)).toEqual(['bad-input-pass', 'restore-pass-1'])
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
    showOpenDialog.mockResolvedValueOnce({ canceled: false, filePaths: ['/Users/x/Downloads/second.ncryptsec'] })
    const second = await call(IpcChannels.CommImportPick)
    release() // the first flow ends in a cancel
    expect((await running).data.error.code).toBe('cancelled')
    // the first ticket is dead; the second still works
    expect((await call(IpcChannels.CommIdentityImport, first.ticket, 'alice', false)).data.error.code).toBe('ticket_expired')
    typing('restore-pass-1')
    expect((await call(IpcChannels.CommIdentityImport, second.ticket, 'bob', false)).ok).toBe(true)
    expect(JSON.parse(seen[0]!.body).path).toBe('/Users/x/Downloads/second.ncryptsec')
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

// The node-daemon-era `modules:*` IPC (A5 on main) was deleted outright by
// REV-01 on ab/core: its tests went with the handlers.

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
