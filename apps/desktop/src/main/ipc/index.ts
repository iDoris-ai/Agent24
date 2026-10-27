// IPC handler registry. M0/M1 starter handlers; capability modules will
// register their own handlers here via the module loader (later M1 task).

import http from 'node:http'
import https from 'node:https'
import { execFile, type ChildProcess } from 'node:child_process'
import { app, BrowserWindow, dialog, ipcMain, shell, type IpcMainInvokeEvent } from 'electron'
import { getBackendEndpoint } from '../backend-manager'
import { findAllowedRoute, proxyError, proxyRequest, timeoutFor } from '../backend-proxy'
import { omlxEndpoint, parseLoopbackUrl, parsePort } from '../omlx-validate'
import type { IpcGuard } from './guard'
import { documentsCall, httpSender } from '../documents'
import { importFile } from '../documents-import'
import { APPROVAL_DECISION_TYPES, IpcChannels } from '../../shared/ipc-types'
import type {
  BackendEndpointResult,
  BackendProxyRequest,
  BackendProxyResponse,
  LlmStatusResult,
  IdorisStatusResult,
  OmlxDetectResult,
  OmlxModelsResult,
  OmlxStartResult,
  OmlxStopResult,
  OmlxWarmupResult,
} from '../../shared/ipc-types'

const BACKEND_HOST = '127.0.0.1'

// oMLX server management
let omlxProcess: ChildProcess | null = null

/** oMLX API key the user configured for THIS app (env `OMLX_API_KEY`, the same
 *  variable oMLX itself reads). Empty = none: probes that need a key are
 *  skipped rather than guessed with a built-in default. */
function configuredOmlxKey(): string {
  return (process.env['OMLX_API_KEY'] ?? '').trim()
}

function omlxProbeCandidates(): { url: string; apiKey: string }[] {
  const key = configuredOmlxKey()
  return [
    // Keyed probe only when a key is configured.
    ...(key ? [{ url: 'http://127.0.0.1:8088', apiKey: key }] : []),
    // Keyless probes: a keyed server just answers 401, which reads as "no models".
    { url: 'http://127.0.0.1:8088', apiKey: '' },
    { url: 'http://127.0.0.1:8000', apiKey: '' },
    { url: 'http://127.0.0.1:8001', apiKey: '' },
  ]
}

/** Loopback-only request to an oMLX/Ollama-style server. `base` must already
 *  have passed parseLoopbackUrl. */
function loopbackRequest(
  base: URL,
  suffix: string,
  init: { method: string; headers: Record<string, string>; timeout: number; body?: string },
  onResponse: (res: http.IncomingMessage) => void,
  onFail: (err: Error) => void,
): void {
  const target = omlxEndpoint(base, suffix)
  const mod = target.protocol === 'https:' ? https : http
  const req = mod.request(
    {
      hostname: target.hostname.replace(/^\[|\]$/g, ''),
      port: target.port || (target.protocol === 'https:' ? 443 : 80),
      path: target.pathname,
      method: init.method,
      headers: init.headers,
      timeout: init.timeout,
    },
    onResponse,
  )
  req.on('error', onFail)
  req.on('timeout', () => { req.destroy(); onFail(new Error('timeout')) })
  if (init.body !== undefined) req.write(init.body)
  req.end()
}

function fetchOmlxModels(url: unknown, apiKey: string): Promise<string[]> {
  return new Promise((resolve) => {
    const base = parseLoopbackUrl(url)
    if (!base) { resolve([]); return }
    const headers: Record<string, string> = { 'Accept': 'application/json' }
    if (apiKey) headers['Authorization'] = `Bearer ${apiKey}`
    loopbackRequest(
      base,
      '/v1/models',
      { method: 'GET', headers, timeout: 2000 },
      (res) => {
        const chunks: Buffer[] = []
        res.on('data', (c: Buffer) => chunks.push(c))
        res.on('end', () => {
          try {
            const body = JSON.parse(Buffer.concat(chunks).toString()) as { data?: { id: string }[] }
            resolve((body.data ?? []).map((m) => m.id))
          } catch { resolve([]) }
        })
      },
      () => resolve([]),
    )
  })
}

// Allowlist of URL prefixes permitted for shell.openExternal. Anything
// not matching is rejected silently — prevents IPC injection opening
// file:// or other dangerous schemes.
const EXTERNAL_URL_ALLOWLIST = ['https://', 'http://'] as const

/** Ready-to-use daemon access for main-process code (not allowlisted: callers
 *  here build their own fixed paths). */
function proxyToBackend(req: BackendProxyRequest, timeoutMs?: number): Promise<BackendProxyResponse> {
  // Endpoint comes from the backend's ready line (dynamic port + token for
  // agent24d; fixed 8765 + empty token for the node mock)
  const endpoint = getBackendEndpoint()
  if (!endpoint) {
    return Promise.resolve(proxyError('backend_not_ready', 'Backend daemon has not announced readiness yet'))
  }
  return proxyRequest(endpoint, req, { host: BACKEND_HOST, ...(timeoutMs === undefined ? {} : { timeoutMs }) })
}

function isBackendProxyRequest(req: unknown): req is BackendProxyRequest {
  if (typeof req !== 'object' || req === null) return false

  const candidate = req as Record<string, unknown>
  const method = candidate['method']
  const requestPath = candidate['path']
  const allowedMethods = new Set(['GET', 'POST', 'PUT', 'PATCH', 'DELETE'])

  return (
    typeof method === 'string' &&
    allowedMethods.has(method) &&
    typeof requestPath === 'string' &&
    requestPath.startsWith('/') &&
    !requestPath.startsWith('//')
  )
}

const SAFE_ID = /^[A-Za-z0-9_.:-]{1,128}$/

/** Native yes/no for an elevated operation. The text is built here, in main,
 *  so a compromised renderer cannot dress the prompt up. */
async function confirmNative(event: IpcMainInvokeEvent, message: string, detail: string): Promise<boolean> {
  const win = BrowserWindow.fromWebContents(event.sender)
  const opts: Electron.MessageBoxOptions = {
    type: 'warning',
    buttons: ['确认', '取消'],
    defaultId: 1,
    cancelId: 1,
    message,
    detail,
    noLink: true,
  }
  try {
    const result = win ? await dialog.showMessageBox(win, opts) : await dialog.showMessageBox(opts)
    return result.response === 0
  } catch {
    return false
  }
}

const cancelled = (): BackendProxyResponse =>
  ({ ok: false, status: 499, data: { error: { code: 'cancelled', message: '操作已取消' } } })
const invalidRequest = (message: string): BackendProxyResponse =>
  ({ ok: false, status: 400, data: { error: { code: 'invalid_request', message } } })

export function registerIpcHandlers(options: {
  idorisStatus?: () => Promise<IdorisStatusResult>
  /** A3: every handler below is registered through this (sender validation). */
  guard: IpcGuard
}): void {
  const handle: typeof ipcMain.handle = (channel, listener) => ipcMain.handle(channel, options.guard(listener))
  // oMLX: auto-detect running server
  handle(IpcChannels.OmlxDetect, async (): Promise<OmlxDetectResult | null> => {
    for (const candidate of omlxProbeCandidates()) {
      const models = await fetchOmlxModels(candidate.url, candidate.apiKey)
      if (models.length > 0) return { ...candidate, models }
    }
    return null
  })

  // oMLX: list models from given url+key (url must be loopback http(s))
  handle(IpcChannels.OmlxModels, async (_event, url: unknown, apiKey: unknown): Promise<OmlxModelsResult> => {
    if (!parseLoopbackUrl(url)) return { ok: false, models: [], error: 'invalid url (loopback http/https only)' }
    const models = await fetchOmlxModels(url, typeof apiKey === 'string' ? apiKey : '')
    return models.length > 0 ? { ok: true, models } : { ok: false, models: [], error: 'no response or no models' }
  })

  // oMLX: start server via `omlx serve`
  handle(IpcChannels.OmlxStart, (_event, port: unknown, apiKey: unknown): OmlxStartResult => {
    const p = port === undefined ? 8000 : parsePort(port)
    if (p === null) return { ok: false, url: '', error: 'invalid port (integer 1-65535)' }
    if (omlxProcess) return { ok: true, url: `http://127.0.0.1:${p}` }
    // The key goes to the child through its environment, never argv (argv is
    // world-readable via `ps`). oMLX reads OMLX_API_KEY — verified in
    // oMLX.app/Contents/Resources/omlx/settings.py (`os.getenv("OMLX_API_KEY")`
    // -> auth.api_key), so no `--api-key` flag is needed.
    const env: NodeJS.ProcessEnv = { ...process.env }
    if (typeof apiKey === 'string' && apiKey) env['OMLX_API_KEY'] = apiKey
    omlxProcess = execFile('omlx', ['serve', '--port', String(p)], { env })
    omlxProcess.on('exit', () => { omlxProcess = null })
    return { ok: true, url: `http://127.0.0.1:${p}` }
  })

  // oMLX: warmup a model by sending a minimal chat request (triggers LLM load into memory)
  handle(IpcChannels.OmlxWarmup, async (_event, url: unknown, apiKey: unknown, modelId: unknown): Promise<OmlxWarmupResult> => {
    if (typeof modelId !== 'string') return { model: String(modelId), ok: false, error: 'invalid args' }
    const base = parseLoopbackUrl(url)
    if (!base) return { model: modelId, ok: false, error: 'invalid url (loopback http/https only)' }
    const key = typeof apiKey === 'string' ? apiKey : ''
    return new Promise((resolve) => {
      const body = JSON.stringify({ model: modelId, messages: [{ role: 'user', content: 'hi' }], max_tokens: 1 })
      const headers: Record<string, string> = { 'Content-Type': 'application/json', 'Content-Length': String(Buffer.byteLength(body)) }
      if (key) headers['Authorization'] = `Bearer ${key}`
      loopbackRequest(
        base,
        '/v1/chat/completions',
        { method: 'POST', headers, timeout: 60000, body },
        (res) => {
          res.resume()
          resolve({ model: modelId, ok: (res.statusCode ?? 500) < 400 })
        },
        (e) => resolve({ model: modelId, ok: false, error: e.message }),
      )
    })
  })

  // oMLX: stop server — kills both app-spawned and externally-started processes
  handle(IpcChannels.OmlxStop, (): OmlxStopResult => {
    if (omlxProcess) { omlxProcess.kill('SIGTERM'); omlxProcess = null }
    // pkill is macOS/Linux only; on Windows this is a no-op (omlx is not supported there yet)
    if (process.platform !== 'win32') {
      execFile('pkill', ['-f', 'omlx serve'], () => { /* ignore exit code */ })
    }
    return { ok: true }
  })
  // modules:* — node-daemon era (npm community modules). The Rust daemon has
  // no `/api/modules*`; list/discover/install/uninstall are retired (A5) and
  // answer with a fixed envelope. The pages stay until REV-01 deletes them.
  const modulesNotSupported = (): { error: { code: string; message: string } } =>
    ({ error: { code: 'not_supported', message: 'node-daemon only' } })
  handle(IpcChannels.ModulesList, modulesNotSupported)
  handle(IpcChannels.ModulesDiscover, modulesNotSupported)
  handle(IpcChannels.ModulesInstall, modulesNotSupported)
  handle(IpcChannels.ModulesUninstall, modulesNotSupported)

  // modules:enable / modules:disable — toggle module state via backend
  handle(IpcChannels.ModulesEnable, async (_event, id: unknown): Promise<{ ok: boolean }> => {
    if (typeof id !== 'string') return { ok: false }
    const res = await proxyToBackend({ method: 'POST', path: `/api/modules/${encodeURIComponent(id)}/enable` })
    return { ok: res.ok }
  })

  handle(IpcChannels.ModulesDisable, async (_event, id: unknown): Promise<{ ok: boolean }> => {
    if (typeof id !== 'string') return { ok: false }
    const res = await proxyToBackend({ method: 'POST', path: `/api/modules/${encodeURIComponent(id)}/disable` })
    return { ok: res.ok }
  })

  // llm:status — current active LLM provider + model
  handle(IpcChannels.LlmStatus, async (): Promise<LlmStatusResult> => {
    // Try oMLX first
    const omlxModels = await fetchOmlxModels('http://127.0.0.1:8088', configuredOmlxKey())
    if (omlxModels.length > 0) {
      return { provider: 'omlx', url: 'http://127.0.0.1:8088', model: omlxModels[0] }
    }
    // Try Ollama
    const ollamaModels = await fetchOmlxModels('http://127.0.0.1:11434', '')
    if (ollamaModels.length > 0) {
      return { provider: 'ollama', url: 'http://127.0.0.1:11434', model: ollamaModels[0] }
    }
    return { provider: 'none', url: '', model: '' }
  })

  handle(IpcChannels.IdorisStatus, (): Promise<IdorisStatusResult> =>
    options.idorisStatus?.() ?? Promise.resolve({ state: 'unavailable', reason: 'handoff-missing' }),
  )

  handle(IpcChannels.AppPing, () => 'pong')
  handle(IpcChannels.AppVersion, () => app.getVersion())
  handle(IpcChannels.ShellOpenExternal, (_event, url: unknown) => {
    if (typeof url !== 'string') return
    const allowed = EXTERNAL_URL_ALLOWLIST.some((prefix) => url.startsWith(prefix))
    if (allowed) {
      shell.openExternal(url).catch((err) => console.error('openExternal failed', err))
    }
  })
  // backend:endpoint — the daemon's actual port (FU-93: the sidebar used to
  // show a hardcoded `:8765`, but agent24d is spawned with `--port 0` and
  // gets whatever the OS hands it).
  handle(IpcChannels.BackendEndpoint, (): BackendEndpointResult | null => {
    const endpoint = getBackendEndpoint()
    return endpoint ? { port: endpoint.port } : null
  })
  const sendToDocuments = httpSender(getBackendEndpoint, BACKEND_HOST)
  handle(IpcChannels.DocumentsRequest, (_event, req: unknown) =>
    documentsCall(req, sendToDocuments),
  )
  handle(IpcChannels.DocumentsImportFile, async (event) => {
    const window = BrowserWindow.fromWebContents(event.sender)
    const options: Electron.OpenDialogOptions = {
      properties: ['openFile'],
      filters: [{ name: 'PDF, JPEG, PNG', extensions: ['pdf', 'jpg', 'jpeg', 'png'] }],
    }
    let picked: Electron.OpenDialogReturnValue
    try {
      picked = window ? await dialog.showOpenDialog(window, options) : await dialog.showOpenDialog(options)
    } catch {
      return { ok: false, status: 500, error: { code: 'dialog_failed', message: 'the file dialog could not open' } }
    }
    const path = picked.filePaths[0]
    if (picked.canceled || !path) return null
    return importFile(path, sendToDocuments, (p) => {
      if (!event.sender.isDestroyed()) event.sender.send(IpcChannels.DocumentsImportProgress, p)
    })
  })
  handle(IpcChannels.BackendProxy, (_event, req: unknown) => {
    if (!isBackendProxyRequest(req)) {
      return proxyError('forbidden', 'valid method and absolute path required')
    }
    // A1: only allowlisted (method, path-template) pairs are forwarded.
    const route = findAllowedRoute(req.method, req.path)
    if (!route) {
      return proxyError('forbidden', `${req.method} ${req.path.split('?')[0]} is not available through the renderer proxy`)
    }
    return proxyToBackend(req, timeoutFor(route))
  })

  // A1: POST /api/v1/approvals/{id} — an elevated operation, so it is not on
  // the generic proxy. The user confirms in a native dialog built here.
  handle(IpcChannels.ApprovalDecide, async (event, id: unknown, decision: unknown): Promise<BackendProxyResponse> => {
    const d = decision as { type?: unknown; reason?: unknown } | null
    if (typeof id !== 'string' || !SAFE_ID.test(id) || id === '.' || id === '..') return invalidRequest('invalid approval id')
    if (typeof d !== 'object' || d === null || !(APPROVAL_DECISION_TYPES as readonly unknown[]).includes(d.type)) {
      return invalidRequest('invalid decision')
    }
    if (d.reason !== undefined && typeof d.reason !== 'string') return invalidRequest('invalid reason')
    const label: Record<string, string> = { approve: '批准', approve_for_session: '本会话内一律批准', deny: '拒绝', abort: '中止' }
    const ok = await confirmNative(
      event,
      `确认对审批 ${id} 做出「${label[d.type as string]}」决定？`,
      typeof d.reason === 'string' && d.reason ? `原因：${d.reason}` : '此决定会立即生效，且可能无法撤销。',
    )
    if (!ok) return cancelled()
    return proxyToBackend({
      method: 'POST',
      path: `/api/v1/approvals/${id}`,
      body: { type: d.type, ...(typeof d.reason === 'string' ? { reason: d.reason } : {}) },
    })
  })

  // A1: PATCH /api/v1/os/{name} — enables/disables a domain OS module.
  handle(IpcChannels.OsModuleSetEnabled, async (event, name: unknown, enabled: unknown): Promise<BackendProxyResponse> => {
    if (typeof name !== 'string' || !SAFE_ID.test(name) || name === '.' || name === '..') return invalidRequest('invalid module name')
    if (typeof enabled !== 'boolean') return invalidRequest('enabled must be a boolean')
    const ok = await confirmNative(
      event,
      `确认${enabled ? '启用' : '停用'}领域模块「${name}」？`,
      '变更写入配置，守护进程重启后生效。',
    )
    if (!ok) return cancelled()
    return proxyToBackend({ method: 'PATCH', path: `/api/v1/os/${encodeURIComponent(name)}`, body: { enabled } })
  })
}
