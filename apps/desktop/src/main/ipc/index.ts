// IPC handler registry. M0/M1 starter handlers; capability modules will
// register their own handlers here via the module loader (later M1 task).

import http from 'node:http'
import https from 'node:https'
import { execFile, type ChildProcess } from 'node:child_process'
import { app, BrowserWindow, dialog, ipcMain, shell, type IpcMainInvokeEvent } from 'electron'
import { daemonCliEndpoint, daemonConnection, getBackendEndpoint } from '../backend-manager'
import { findAllowedRoute, proxyError, proxyRequest, timeoutFor } from '../backend-proxy'
import { omlxEndpoint, parseLoopbackUrl, parsePort } from '../omlx-validate'
import { readOmlxSettingsKey } from '../omlx-settings-key'
import type { IpcGuard } from './guard'
import { documentsCall } from '../documents'
import { importFile } from '../documents-import'
import { IpcChannels } from '../../shared/ipc-types'
import type {
  BackendEndpointResult,
  BackendProxyRequest,
  BackendProxyResponse,
  LlmStatusResult,
  IdorisStatusResult,
  OmlxAuthRequiredResult,
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

/** A key read from the user's own `~/.omlx/settings.json`. It lives only here,
 *  in main-process memory: it is attached to requests for exactly the origin it
 *  was verified against and is never returned over IPC or logged. */
let discoveredOmlxKey: { origin: string; apiKey: string } | null = null

/** The key to send for `base`: what the caller supplied, or — when it sent none
 *  — the discovered key, but only for the very origin it was verified on. */
function effectiveOmlxKey(base: URL, supplied: string): string {
  if (supplied) return supplied
  return discoveredOmlxKey && discoveredOmlxKey.origin === base.origin ? discoveredOmlxKey.apiKey : ''
}

interface OmlxProbeCandidate { url: string; apiKey: string; fromSettings?: boolean }

function omlxProbeCandidates(settingsPath?: string): OmlxProbeCandidate[] {
  const key = configuredOmlxKey()
  const fromFile = readOmlxSettingsKey(settingsPath)
  return [
    // Keyed probe only when a key is configured.
    ...(key ? [{ url: 'http://127.0.0.1:8088', apiKey: key }] : []),
    // The key from the user's own oMLX config, for the address that same
    // config says oMLX listens on.
    ...(fromFile ? [{ url: fromFile.url, apiKey: fromFile.apiKey, fromSettings: true }] : []),
    // Keyless probes: a keyed server answers 401 (reported as "auth required").
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

interface OmlxProbe { models: string[]; authRequired: boolean }

function probeOmlx(url: unknown, apiKey: string): Promise<OmlxProbe> {
  return new Promise((resolve) => {
    const base = parseLoopbackUrl(url)
    if (!base) { resolve({ models: [], authRequired: false }); return }
    const headers: Record<string, string> = { 'Accept': 'application/json' }
    const key = effectiveOmlxKey(base, apiKey)
    if (key) headers['Authorization'] = `Bearer ${key}`
    try {
    loopbackRequest(
      base,
      '/v1/models',
      { method: 'GET', headers, timeout: 2000 },
      (res) => {
        // 401/403: something is listening and wants a (different) key.
        if (res.statusCode === 401 || res.statusCode === 403) {
          res.resume()
          resolve({ models: [], authRequired: true })
          return
        }
        const chunks: Buffer[] = []
        res.on('data', (c: Buffer) => chunks.push(c))
        res.on('end', () => {
          try {
            const body = JSON.parse(Buffer.concat(chunks).toString()) as { data?: { id: string }[] }
            resolve({ models: (body.data ?? []).map((m) => m.id), authRequired: false })
          } catch { resolve({ models: [], authRequired: false }) }
        })
      },
      () => resolve({ models: [], authRequired: false }),
    )
    } catch { resolve({ models: [], authRequired: false }) }
  })
}

function fetchOmlxModels(url: unknown, apiKey: string): Promise<string[]> {
  return probeOmlx(url, apiKey).then((probe) => probe.models)
}

// Allowlist of URL prefixes permitted for shell.openExternal. Anything
// not matching is rejected silently — prevents IPC injection opening
// file:// or other dangerous schemes.
const EXTERNAL_URL_ALLOWLIST = ['https://', 'http://'] as const

// The daemon's private host interface (`/api/v1/host/…`, KB-HOST-01) belongs
// to main alone: the renderer's generic proxy never reaches it, or the page
// would act with the product host's authority (UI-BRIDGE U1.5). The daemon
// routes on the raw path, so that is what decides; a decoded and normalized
// form that lands there is refused as well, and one that does not decode
// falls back to the raw verdict.
const HOST_INTERFACE = /^\/api\/v1\/host(\/|$)/i

export function reachesHostInterface(requestPath: string): boolean {
  const raw = requestPath.split(/[?#]/, 1)[0] ?? ''
  const hits = (path: string) => HOST_INTERFACE.test(path.replace(/\/{2,}/g, '/'))
  if (hits(raw)) return true
  try {
    return hits(new URL(decodeURIComponent(raw), 'http://daemon').pathname)
  } catch {
    return false
  }
}

/** Ready-to-use daemon access for main-process code (not allowlisted: callers
 *  here build their own fixed paths). */
function proxyToBackend(req: BackendProxyRequest, timeoutMs?: number): Promise<BackendProxyResponse> {
  // Port and credential come from the backend's ready line (dynamic port +
  // token for agent24d; fixed 8765 and no credential for the node mock).
  // Without a usable credential nothing is sent.
  const conn = daemonConnection()
  if (!conn) {
    return Promise.resolve(proxyError('backend_not_ready', 'Backend daemon is not ready (no announced port or credential)'))
  }
  return proxyRequest(conn, req, { host: BACKEND_HOST, ...(timeoutMs === undefined ? {} : { timeoutMs }) })
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

/** Shortens text for the native dialog, and says so (with the full length) when it did. */
const clip = (text: string, max: number): string =>
  text.length > max ? `${text.slice(0, max)}…（已截断，共 ${text.length} 字符）` : text

// eslint-disable-next-line no-control-regex
const hasControlChars = (text: string): boolean => /[\u0000-\u001f\u007f]/.test(text)
const SAFE_MODULE_ID = /^[A-Za-z0-9._-]+$/
const DECISION_SHAPE = /^[a-z_]{1,64}$/

// Sensitive IPCs share one in-flight slot: a script that fires them in a loop
// must not be able to stack native dialogs. Busy => immediate refusal, no queue.
let sensitiveBusy = false
async function exclusive(fn: () => Promise<BackendProxyResponse>): Promise<BackendProxyResponse> {
  if (sensitiveBusy) {
    return { ok: false, status: 429, data: { error: { code: 'busy', message: '另一项需要确认的操作正在进行' } } }
  }
  sensitiveBusy = true
  try {
    return await fn()
  } finally {
    sensitiveBusy = false
  }
}

const cancelled = (): BackendProxyResponse =>
  ({ ok: false, status: 499, data: { error: { code: 'cancelled', message: '操作已取消' } } })
const invalidRequest = (message: string): BackendProxyResponse =>
  ({ ok: false, status: 400, data: { error: { code: 'invalid_request', message } } })

export function registerIpcHandlers(options: {
  idorisStatus?: () => Promise<IdorisStatusResult>
  /** Test seam: where to look for the user's `~/.omlx/settings.json`. */
  omlxSettingsPath?: string
  /** A3: every handler below is registered through this (sender validation). */
  guard: IpcGuard
}): void {
  const handle: typeof ipcMain.handle = (channel, listener) => ipcMain.handle(channel, options.guard(listener))
  // oMLX: auto-detect running server
  handle(IpcChannels.OmlxDetect, async (): Promise<OmlxDetectResult | OmlxAuthRequiredResult | null> => {
    let authRequiredAt: string | null = null
    for (const candidate of omlxProbeCandidates(options.omlxSettingsPath)) {
      const probe = await probeOmlx(candidate.url, candidate.apiKey)
      if (probe.models.length > 0) {
        if (candidate.fromSettings) {
          // Verified: remember it for this origin in main only. The renderer
          // is handed an empty key, never the one from the user's config file.
          discoveredOmlxKey = { origin: new URL(candidate.url).origin, apiKey: candidate.apiKey }
          return { url: candidate.url, apiKey: '', models: probe.models }
        }
        return { url: candidate.url, apiKey: candidate.apiKey, models: probe.models }
      }
      if (probe.authRequired && authRequiredAt === null) authRequiredAt = candidate.url
    }
    return authRequiredAt ? { authRequired: true, url: authRequiredAt } : null
  })

  // oMLX: list models from given url+key (url must be loopback http(s))
  handle(IpcChannels.OmlxModels, async (_event, url: unknown, apiKey: unknown): Promise<OmlxModelsResult> => {
    if (!parseLoopbackUrl(url)) return { ok: false, models: [], error: 'invalid url (loopback http/https only)' }
    if (typeof apiKey === 'string' && hasControlChars(apiKey)) return { ok: false, models: [], error: 'forbidden: invalid api key' }
    const probe = await probeOmlx(url, typeof apiKey === 'string' ? apiKey : '')
    if (probe.models.length > 0) return { ok: true, models: probe.models }
    if (probe.authRequired) return { ok: false, models: [], authRequired: true, error: 'authentication required' }
    return { ok: false, models: [], error: 'no response or no models' }
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
    if (hasControlChars(key)) return { model: modelId, ok: false, error: 'forbidden: invalid api key' }
    return new Promise((resolve) => {
      const body = JSON.stringify({ model: modelId, messages: [{ role: 'user', content: 'hi' }], max_tokens: 1 })
      const headers: Record<string, string> = { 'Content-Type': 'application/json', 'Content-Length': String(Buffer.byteLength(body)) }
      const sendKey = effectiveOmlxKey(base, key)
      if (sendKey) headers['Authorization'] = `Bearer ${sendKey}`
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
    if (typeof id !== 'string' || !SAFE_MODULE_ID.test(id) || id === '.' || id.includes('..')) return { ok: false }
    const res = await proxyToBackend({ method: 'POST', path: `/api/modules/${encodeURIComponent(id)}/enable` })
    return { ok: res.ok }
  })

  handle(IpcChannels.ModulesDisable, async (_event, id: unknown): Promise<{ ok: boolean }> => {
    if (typeof id !== 'string' || !SAFE_MODULE_ID.test(id) || id === '.' || id.includes('..')) return { ok: false }
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
  handle(IpcChannels.DocumentsRequest, (_event, req: unknown) =>
    documentsCall(req, daemonCliEndpoint),
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
    return importFile(path, daemonCliEndpoint, (p) => {
      if (!event.sender.isDestroyed()) event.sender.send(IpcChannels.DocumentsImportProgress, p)
    })
  })
  handle(IpcChannels.BackendProxy, (_event, req: unknown) => {
    if (!isBackendProxyRequest(req)) {
      return proxyError('forbidden', 'valid method and absolute path required')
    }
    if (reachesHostInterface(req.path)) {
      return {
        ok: false,
        status: 403,
        data: { error: { code: 'host_interface_private', message: 'The host interface is not reachable from the renderer' } },
      }
    }
    // A1: only allowlisted (method, path-template) pairs are forwarded.
    const route = findAllowedRoute(req.method, req.path)
    if (!route) {
      return proxyError('forbidden', `${req.method} ${req.path.split('?')[0]} is not available through the renderer proxy`)
    }
    return proxyToBackend(req, timeoutFor(route))
  })

  // A1: POST /api/v1/approvals/{id} — an elevated operation, so it is not on
  // the generic proxy. The native dialog shows the daemon's OWN record of the
  // approval (read here, by id), never text supplied by the page; the page's
  // reason is shown only as a note. Unreadable record => refuse, no dialog.
  handle(IpcChannels.ApprovalDecide, (event, id: unknown, decision: unknown): Promise<BackendProxyResponse> =>
    exclusive(async () => {
      const d = decision as { type?: unknown; reason?: unknown } | null
      if (typeof id !== 'string' || !SAFE_ID.test(id) || id === '.' || id === '..') return invalidRequest('invalid approval id')
      // Shape only: which decisions are valid is the daemon's call (it offers
      // approve_for_target etc. per approval); checked against the record below.
      if (typeof d !== 'object' || d === null || typeof d.type !== 'string' || !DECISION_SHAPE.test(d.type)) {
        return invalidRequest('invalid decision')
      }
      if (d.reason !== undefined && typeof d.reason !== 'string') return invalidRequest('invalid reason')

      const rec = await proxyToBackend({ method: 'GET', path: `/api/v1/approvals/${id}` })
      if (!rec.ok || typeof rec.data !== 'object' || rec.data === null) {
        return { ok: false, status: rec.status === 404 ? 404 : 502, data: { error: { code: 'approval_unavailable', message: '读不到该审批的真实记录，已拒绝操作' } } }
      }
      const a = rec.data as {
        kind?: unknown; summary?: unknown; payload?: unknown; run_id?: unknown; status?: unknown; available_decisions?: unknown
      }
      if (a.status !== undefined && a.status !== 'pending') {
        return { ok: false, status: 409, data: { error: { code: 'not_pending', message: `审批已处于 ${String(a.status)} 状态` } } }
      }
      // The record's own decision list wins when it has one.
      if (Array.isArray(a.available_decisions) && !a.available_decisions.includes(d.type)) {
        return invalidRequest('decision not offered for this approval')
      }
      const label: Record<string, string> = { approve: '批准', approve_for_session: '本会话内一律批准', deny: '拒绝', abort: '中止' }
      const payload = clip(JSON.stringify(a.payload ?? {}), 600)
      const detail = [
        `类型：${clip(String(a.kind ?? '?'), 40)}`,
        `摘要：${clip(String(a.summary ?? ''), 300)}`,
        `运行：${clip(String(a.run_id ?? '?'), 80)}`,
        `详情：${payload}`,
        typeof d.reason === 'string' && d.reason ? `页面备注（仅供参考）：${clip(d.reason, 200)}` : '',
        '此决定会立即生效，且可能无法撤销。',
      ].filter(Boolean).join('\n')
      const ok = await confirmNative(event, `确认对审批 ${id} 做出「${label[d.type as string] ?? d.type}」决定？`, detail)
      if (!ok) return cancelled()
      return proxyToBackend({
        method: 'POST',
        path: `/api/v1/approvals/${id}`,
        body: { type: d.type, ...(typeof d.reason === 'string' ? { reason: d.reason } : {}) },
      })
    }),
  )

  // A1: PATCH /api/v1/os/{name} — enables/disables a domain OS module. The
  // dialog names the module as the daemon lists it, and the state it becomes.
  handle(IpcChannels.OsModuleSetEnabled, (event, name: unknown, enabled: unknown): Promise<BackendProxyResponse> =>
    exclusive(async () => {
      if (typeof name !== 'string' || !SAFE_ID.test(name) || name === '.' || name === '..') return invalidRequest('invalid module name')
      if (typeof enabled !== 'boolean') return invalidRequest('enabled must be a boolean')
      const list = await proxyToBackend({ method: 'GET', path: '/api/v1/os' })
      const mods = (list.data as { modules?: { name?: unknown; enabled?: unknown; version?: unknown }[] } | null)?.modules
      const mod = list.ok && Array.isArray(mods) ? mods.find((m) => m.name === name) : undefined
      if (!mod) {
        return { ok: false, status: 404, data: { error: { code: 'module_unavailable', message: '守护进程没有该领域模块，已拒绝操作' } } }
      }
      const ok = await confirmNative(
        event,
        `确认将领域模块「${name}」${mod.enabled === true ? '由 启用 改为' : mod.enabled === false ? '由 停用 改为' : '改为'} ${enabled ? '启用' : '停用'}？`,
        `版本：${clip(String(mod.version ?? '?'), 40)}\n立即生效：停用会立刻撤销该模块的 consent 并停止它。`,
      )
      if (!ok) return cancelled()
      return proxyToBackend({ method: 'PATCH', path: `/api/v1/os/${encodeURIComponent(name)}`, body: { enabled } })
    }),
  )
}
