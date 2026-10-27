// PR-A (pre-release desktop hardening) — handler-level tests for A1/A3/A4/A5.
// Electron is mocked so registerIpcHandlers() can run under plain Node; the
// daemon is a real loopback http server so "no request reached it" is a
// factual assertion, not a mock-call count.

import http from 'node:http'
import type { AddressInfo } from 'node:net'
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'

const handlers = new Map<string, (event: unknown, ...args: unknown[]) => unknown>()
const showMessageBox = vi.fn()
const execFileMock = vi.fn()

vi.mock('electron', () => ({
  app: { getVersion: () => '0.0.0-test' },
  BrowserWindow: { fromWebContents: () => null },
  dialog: { showMessageBox: (...a: unknown[]) => showMessageBox(...a), showOpenDialog: vi.fn() },
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
}))

import { IpcChannels } from '../../shared/ipc-types'
import { createIpcGuard } from './guard'
import { reachesHostInterface, registerIpcHandlers } from './index'

const APP_URL = 'file:///app/renderer/index.html'
const mainFrame = { url: APP_URL }
const mainWindow = { isDestroyed: () => false, webContents: { mainFrame } }
const trustedEvent = { senderFrame: mainFrame, sender: { isDestroyed: () => false, send: vi.fn() } }

let daemon: http.Server
let seen: { method?: string; url?: string; auth?: string; body: string }[] = []

beforeAll(async () => {
  daemon = http.createServer((req, res) => {
    const chunks: Buffer[] = []
    req.on('data', (c: Buffer) => chunks.push(c))
    req.on('end', () => {
      if (!(req.method === 'GET' && req.url?.startsWith('/api/v1/approvals/')) && !(req.method === 'GET' && req.url === '/api/v1/os')) seen.push({ method: req.method, url: req.url, auth: req.headers.authorization, body: Buffer.concat(chunks).toString() })
      res.setHeader('Content-Type', 'application/json')
      if (req.url === '/v1/models') res.end(JSON.stringify({ data: [{ id: 'm1' }] }))
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
      } else if (req.method === 'GET' && req.url === '/api/v1/os') {
        res.end(JSON.stringify({ modules: [{ name: 'sin90', enabled: false, version: '0.5.0' }] }))
      } else res.end(JSON.stringify({ ok: true, echoed: req.url }))
    })
  })
  await new Promise<void>((r) => daemon.listen(0, '127.0.0.1', r))
  endpoint.port = (daemon.address() as AddressInfo).port
  registerIpcHandlers({
    guard: createIpcGuard({
      getMainWindow: () => mainWindow as never,
      isTrustedUrl: (u) => u === APP_URL,
    }),
  })
})

afterAll(() => new Promise<void>((r) => daemon.close(() => r())))

beforeEach(() => {
  seen = []
  showMessageBox.mockReset()
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
    ['GET', '/api/modules'],
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
      expect(opts.detail).toContain('页面备注（仅供参考）：harmless readme tweak')
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
      expect(opts.detail).toContain('（已截断，共 500 字符）')
      expect(opts.detail).toMatch(/（已截断，共 \d{4} 字符）/)
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
      expect(opts.message).toContain('停用 改为 启用')
      expect(opts.detail).toContain('0.5.0')
      expect(opts.detail).toContain('立即生效')
      expect(opts.detail).not.toContain('重启后生效')
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

describe('ModulesEnable/Disable id validation', () => {
  it.each(['..', 'a/../b', '', 'x y', '.'])('%j is refused without touching the daemon', async (id) => {
    for (const ch of [IpcChannels.ModulesEnable, IpcChannels.ModulesDisable]) {
      expect(await call(ch, id)).toEqual({ ok: false })
    }
    expect(seen).toHaveLength(0)
  })
  it('an ordinary id is forwarded', async () => {
    await call(IpcChannels.ModulesEnable, 'hello-1.2')
    expect(seen[0]?.url).toBe('/api/modules/hello-1.2/enable')
  })
})

describe('A5 node-daemon module IPC is retired', () => {
  const notSupported = { error: { code: 'not_supported', message: 'node-daemon only' } }
  it.each([
    [IpcChannels.ModulesList, []],
    [IpcChannels.ModulesDiscover, [{}]],
    [IpcChannels.ModulesInstall, ['@x/y']],
    [IpcChannels.ModulesUninstall, ['@x/y', 'y']],
  ])('%s returns not_supported without touching the daemon', async (channel, args) => {
    const res = await call(channel, ...(args as unknown[]))
    expect(res).toEqual(notSupported)
    expect(seen).toHaveLength(0)
  })
})

describe('backend:endpoint hands the renderer the port only (KB-HOST-01 PR-A1)', () => {
  it('returns exactly {port}, with no credential, while main holds one', async () => {
    const result = await call(IpcChannels.BackendEndpoint)
    expect(result).toEqual({ port: endpoint.port })
    expect(JSON.stringify(result)).not.toContain(endpoint.token)
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
