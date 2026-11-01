// IPC handler registry. M0/M1 starter handlers; capability modules will
// register their own handlers here via the module loader (later M1 task).

import fs from 'node:fs'
import path from 'node:path'
import { randomBytes } from 'node:crypto'
import http from 'node:http'
import https from 'node:https'
import { execFile, type ChildProcess } from 'node:child_process'
import { app, BrowserWindow, dialog, ipcMain, shell, type IpcMainInvokeEvent } from 'electron'
import { createNavPrefsStore } from '../nav-prefs'
import { daemonCliEndpoint, daemonConnection, getBackendEndpoint } from '../backend-manager'
import { findAllowedRoute, proxyError, proxyRequest, timeoutFor, withKnowledgeBinding } from '../backend-proxy'
import type { KnowledgeController } from '../knowledge-settings'
import { omlxEndpoint, parseLoopbackUrl, parsePort } from '../omlx-validate'
import { backupFileStem, isSafeBackupPassword, isSafeMessage, isSafeName, isValidNpub } from '../comm-validate'
import {
  promptBackupPassword as openBackupPasswordWindow,
  promptRestorePassword as openRestorePasswordWindow,
} from '../backup-password-window'
import { mt, setMainLocale, isMainLocale } from '../main-i18n'
import { readOmlxSettingsKey } from '../omlx-settings-key'
import type { IpcGuard } from './guard'
import { documentsCall } from '../documents'
import { importFile } from '../documents-import'
import { IpcChannels } from '../../shared/ipc-types'
import type {
  CommImportPickResult,
  KnowledgeSettingsResult,
  KnowledgeStatus,
  BackendEndpointResult,
  BackendProxyRequest,
  BackendProxyResponse,
  LlmStatusResult,
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

/** Where to look for the user's own `~/.omlx/settings.json` (test seam). */
let omlxSettingsPath: string | undefined

/** The key to send for `base`: what the caller supplied or — when it sent none —
 *  the one in the user's own oMLX config, but only for the exact address that
 *  config says oMLX listens on. It is re-read every time (so a rotated or
 *  removed key is honoured at once), lives only in main-process memory, and is
 *  never returned over IPC or logged. */
function effectiveOmlxKey(base: URL, supplied: string): string {
  if (supplied) return supplied
  const fromFile = readOmlxSettingsKey(omlxSettingsPath)
  return fromFile && new URL(fromFile.url).origin === base.origin ? fromFile.apiKey : ''
}

interface OmlxProbeCandidate { url: string; apiKey: string; fromSettings?: boolean }

function omlxProbeCandidates(ports: readonly number[] = [8088, 8000, 8001]): OmlxProbeCandidate[] {
  const key = configuredOmlxKey()
  const fromFile = readOmlxSettingsKey(omlxSettingsPath)
  return [
    // Keyed probe only when a key is configured.
    ...(key && ports.includes(8088) ? [{ url: 'http://127.0.0.1:8088', apiKey: key }] : []),
    // The user's own oMLX config, for the address that same config says oMLX
    // listens on. No key is copied here: it is resolved from the file right
    // before the request (effectiveOmlxKey), so it can never be stale.
    ...(fromFile ? [{ url: fromFile.url, apiKey: '', fromSettings: true }] : []),
    // Keyless probes: a keyed server answers 401 (reported as "auth required").
    ...ports.map((port) => ({ url: `http://127.0.0.1:${port}`, apiKey: '' })),
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
    const keyEncodings = key ? [
      key,
      Buffer.from(key).toString('base64').replace(/=+$/, ''),
      Buffer.from(key).toString('base64url'),
      Buffer.from(key).toString('hex'),
      Buffer.from(key).toString('hex').toUpperCase(),
    ] : []
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
            // Only plain string ids, and never one that echoes the key back
            // (a reflecting server must not turn into a way to hand it on).
            const models = (body.data ?? []).map((m) => m.id)
              .filter((id): id is string => typeof id === 'string' && !keyEncodings.some((encoded) => id.includes(encoded)))
            resolve({ models, authRequired: false })
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
  return reachesPrefix(HOST_INTERFACE, requestPath)
}

// CORE-2 AgentEar S3: the daemon's desktop confirmation channel
// (`/api/v1/presence/challenges/…`) belongs to main alone. If the renderer
// could read or answer a challenge it could approve its own elevation, so the
// proxy refuses the whole `/presence` subtree on top of the allowlist, by the
// same raw / decoded / normalized rule as the host interface.
const PRESENCE_CHANNEL = /^(\/api\/v1)?\/presence(\/|$)/i

export function reachesPresenceChannel(requestPath: string): boolean {
  return reachesPrefix(PRESENCE_CHANNEL, requestPath)
}

function reachesPrefix(prefix: RegExp, requestPath: string): boolean {
  const raw = requestPath.split(/[?#]/, 1)[0] ?? ''
  const hits = (path: string) => prefix.test(path.replace(/\/{2,}/g, '/'))
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

/** The daemon's whole export budget is 30 s for Hyphae's run plus one in-daemon scrypt check; this stays well above it. */
const EXPORT_TIMEOUT_MS = 90_000

/**
 * Restore: the one pending backup selection, an opaque ticket standing for a
 * file the user picked. It is consumed when used. After a result that is not a
 * success it is re-armed (same ticket, SAME expiry: the TTL is never extended)
 * at most IMPORT_MAX_ATTEMPTS times in total, and only if the user has not
 * picked a different file in the meantime (a newer pick replaces it, so an older
 * flow can never put its own ticket back).
 */
interface PendingBackup { id: string; path: string; expires: number; attemptsLeft: number }
let pendingBackup: PendingBackup | null = null
const IMPORT_TICKET_TTL_MS = 5 * 60_000
const IMPORT_MAX_ATTEMPTS = 3

/** True if anything (file, dir, even a dangling symlink) is at `p`. */
function pathExists(p: string): boolean {
  try { fs.lstatSync(p); return true } catch { return false }
}

const SAFE_ID = /^[A-Za-z0-9_.:-]{1,128}$/

/** Native yes/no for an elevated operation. The text is built here, in main,
 *  so a compromised renderer cannot dress the prompt up. */
async function confirmNative(event: IpcMainInvokeEvent, message: string, detail: string): Promise<boolean> {
  const win = BrowserWindow.fromWebContents(event.sender)
  const opts: Electron.MessageBoxOptions = {
    type: 'warning',
    buttons: [mt('dialog.confirm'), mt('dialog.cancel')],
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
  text.length > max ? `${text.slice(0, max)}${mt('clip.truncated', { length: text.length })}` : text

// eslint-disable-next-line no-control-regex
const hasControlChars = (text: string): boolean => /[\u0000-\u001f\u007f]/.test(text)
const DECISION_SHAPE = /^[a-z_]{1,64}$/

// Sensitive IPCs share one in-flight slot: a script that fires them in a loop
// must not be able to stack native dialogs. Busy => immediate refusal, no queue.
let sensitiveBusy = false
async function exclusive(fn: () => Promise<BackendProxyResponse>): Promise<BackendProxyResponse> {
  if (sensitiveBusy) {
    return { ok: false, status: 429, data: { error: { code: 'busy', message: mt('err.busy') } } }
  }
  sensitiveBusy = true
  try {
    return await fn()
  } finally {
    sensitiveBusy = false
  }
}

// A-3: the one attached module the desktop may disable or unpair. Fixed here,
// never taken from the renderer.
const AGENTEAR = 'agentear'
const AGENTEAR_PATH = `/api/v1/attached/${AGENTEAR}`

/** Error codes the AgentEar IPCs promise the renderer. Anything else the
 *  daemon or transport says is folded into one of these. */
export type AgentEarErrorCode =
  | 'invalid_request' | 'busy' | 'cancelled' | 'not_paired' | 'forbidden' | 'daemon_error'
  | 'backend_not_ready' | 'timeout' | 'proxy_failed'

const agentearError = (status: number, code: AgentEarErrorCode, message: string): BackendProxyResponse =>
  ({ ok: false, status, data: { error: { code, message } } })

/** Maps a failed daemon/proxy response onto the stable AgentEar codes. */
function agentearFailure(res: BackendProxyResponse): BackendProxyResponse {
  const code = (res.data as { error?: { code?: unknown } } | null)?.error?.code
  // Transport failures produced by backend-proxy keep their own code.
  if (code === 'backend_not_ready' || code === 'timeout' || code === 'proxy_failed') return res
  if (res.status === 404) return agentearError(404, 'not_paired', mt('err.agentearNotPaired'))
  if (res.status === 401 || res.status === 403) return agentearError(403, 'forbidden', mt('err.agentearForbidden'))
  return agentearError(502, 'daemon_error', mt('err.agentearDaemon'))
}

/** Confirms from the daemon's own list (never from the page) that AgentEar
 *  is paired. Null = go ahead; otherwise the failure to return as is. */
async function agentearPairedOrFailure(): Promise<BackendProxyResponse | null> {
  const list = await proxyToBackend({ method: 'GET', path: '/api/v1/attached' })
  if (!list.ok) return agentearFailure(list)
  const mods = (list.data as { modules?: unknown } | null)?.modules
  const paired = Array.isArray(mods) && mods.some((m) => (m as { name?: unknown } | null)?.name === AGENTEAR)
  return paired ? null : agentearError(404, 'not_paired', mt('err.agentearNotPaired'))
}

const cancelled = (): BackendProxyResponse =>
  ({ ok: false, status: 499, data: { error: { code: 'cancelled', message: mt('err.cancelled') } } })
const invalidRequest = (message: string): BackendProxyResponse =>
  ({ ok: false, status: 400, data: { error: { code: 'invalid_request', message } } })

export function registerIpcHandlers(options: {
  /** Test seam: where to look for the user's `~/.omlx/settings.json`. */
  omlxSettingsPath?: string
  /** Test seam: replace discovery ports to isolate probes from local services. */
  omlxCandidatePorts?: readonly number[]
  /** A3: every handler below is registered through this (sender validation). */
  guard: IpcGuard
  /** The Knowledge settings, once main has built them. */
  knowledge?: () => KnowledgeController | null
  /** Test seam: the main-owned backup-password window. */
  promptBackupPassword?: (parent: BrowserWindow | null) => Promise<string | null>
  /** Test seam: the main-owned restore-password window. */
  promptRestorePassword?: typeof openRestorePasswordWindow
}): void {
  const promptBackupPassword = options.promptBackupPassword ?? openBackupPasswordWindow
  const promptRestorePassword = options.promptRestorePassword ?? openRestorePasswordWindow
  const knowledge = options.knowledge ?? (() => null)
  omlxSettingsPath = options.omlxSettingsPath
  const handle: typeof ipcMain.handle = (channel, listener) => ipcMain.handle(channel, options.guard(listener))
  // oMLX: auto-detect running server
  handle(IpcChannels.OmlxDetect, async (): Promise<OmlxDetectResult | OmlxAuthRequiredResult | null> => {
    let authRequiredAt: string | null = null
    for (const candidate of omlxProbeCandidates(options.omlxCandidatePorts)) {
      const probe = await probeOmlx(candidate.url, candidate.apiKey)
      if (probe.models.length > 0) {
        if (candidate.fromSettings) {
          // The renderer is handed an empty key, never the one from the user's
          // config file; main re-reads it for later requests to this address.
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


  // The renderer reports its UI language so the tray and native dialogs follow it.
  handle(IpcChannels.AppSetLocale, (_event, locale: unknown): void => {
    if (isMainLocale(locale)) setMainLocale(locale)
  })

  const navPrefs = () => createNavPrefsStore(app.getPath('userData'))
  handle(IpcChannels.NavPrefsGet, () => navPrefs().get())
  handle(IpcChannels.NavPrefsSet, (_event, value: unknown) => navPrefs().set(value))

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
    if (reachesPresenceChannel(req.path)) {
      return {
        ok: false,
        status: 403,
        data: { error: { code: 'presence_channel_private', message: 'The presence channel is not reachable from the renderer' } },
      }
    }
    // A1: only allowlisted (method, path-template) pairs are forwarded.
    const route = findAllowedRoute(req.method, req.path)
    if (!route) {
      return proxyError('forbidden', `${req.method} ${req.path.split('?')[0]} is not available through the renderer proxy`)
    }
    const forwarded = withKnowledgeBinding(route, req, knowledge()?.bindingForRun() ?? null)
    if (!forwarded) return proxyError('invalid_request', 'A run is created from a JSON object')
    return proxyToBackend(forwarded, timeoutFor(route))
  })

  handle(IpcChannels.KnowledgeStatus, (): KnowledgeStatus | null => knowledge()?.status() ?? null)
  handle(IpcChannels.KnowledgeConfigure, (_event, input: unknown): Promise<KnowledgeSettingsResult> => {
    const controller = knowledge()
    if (!controller || typeof input !== 'object' || input === null) {
      return Promise.resolve({ ok: false, reason: controller ? 'invalid' : 'unavailable' })
    }
    const { endpoint, knowledgeBaseIds, apiKey } = input as Record<string, unknown>
    return controller.configure({ endpoint, knowledgeBaseIds, apiKey })
  })
  handle(IpcChannels.KnowledgeSetEnabled, (_event, enabled: unknown): Promise<KnowledgeSettingsResult> => {
    const controller = knowledge()
    if (!controller || typeof enabled !== 'boolean') {
      return Promise.resolve({ ok: false, reason: controller ? 'invalid' : 'unavailable' })
    }
    return controller.setEnabled(enabled)
  })

  // Executable installation is elevated and must never use the generic proxy.
  handle(IpcChannels.CommInstall, (event): Promise<BackendProxyResponse> =>
    exclusive(async () => {
      const ok = await confirmNative(event, mt('comm.install'), mt('comm.installDetail'))
      if (!ok) return cancelled()
      return proxyToBackend({ method: 'POST', path: '/api/v1/comm/install' }, 360_000)
    }),
  )

  // Private-key backup. Not on the generic proxy: the destination is chosen
  // here with a native save dialog (never by the page) and the daemon creates
  // the file itself (0600, exclusive create, never overwriting). What is
  // written is Hyphae's password-encrypted NIP-49 backup, so no plaintext key
  // exists in this process or the renderer; the page only learns
  // {nickname, npub, path}.
  handle(IpcChannels.CommIdentityExport, (event, nickname: unknown): Promise<BackendProxyResponse> =>
    exclusive(async () => {
      // The page asks only for "export this identity". The backup password is
      // collected below by a window main owns, never by or through the page.
      if (!isSafeName(nickname, 128)) return invalidRequest('invalid nickname')
      // 1. The identity must exist before the user is asked anything.
      const listed = await proxyToBackend({ method: 'GET', path: '/api/v1/comm/identity' })
      const identities = (listed.data as { data?: unknown } | null)?.data
      if (!listed.ok || !Array.isArray(identities) ||
          !identities.some((i) => (i as { nickname?: unknown } | null)?.nickname === nickname)) {
        return { ok: false, status: 404, data: { error: { code: 'identity_not_found', message: mt('err.identityNotFound') } } }
      }
      // 2. Risk confirmation.
      const ok = await confirmNative(event, mt('comm.export', { name: clip(nickname, 80) }), mt('comm.exportDetail'))
      if (!ok) return cancelled()
      const win = BrowserWindow.fromWebContents(event.sender)
      // 3. Backup password, in main's own isolated window.
      let backupPassword: string | null = await promptBackupPassword(win)
      if (backupPassword === null) return cancelled()
      try {
        if (!isSafeBackupPassword(backupPassword)) return invalidRequest('invalid backup password')
        // 4. Where to save.
        const options: Electron.SaveDialogOptions = {
          title: mt('comm.exportSave'),
          // Downloads, not Documents (which is often iCloud-synced).
          defaultPath: path.join(app.getPath('downloads'), `${backupFileStem(nickname)}.ncryptsec`),
          filters: [{ name: 'NIP-49 ncryptsec', extensions: ['ncryptsec'] }],
        }
        let picked: Electron.SaveDialogReturnValue
        try {
          picked = win ? await dialog.showSaveDialog(win, options) : await dialog.showSaveDialog(options)
        } catch {
          return { ok: false, status: 500, data: { error: { code: 'dialog_failed', message: 'the file dialog could not open' } } }
        }
        if (picked.canceled || !picked.filePath) return cancelled()
        // The daemon never overwrites (O_EXCL) and says so; checking here too
        // gives the user the reason before any key material is touched.
        if (pathExists(picked.filePath)) {
          return { ok: false, status: 409, data: { error: { code: 'target_exists', message: mt('err.targetExists') } } }
        }
        // 5. The daemon.
        return await proxyToBackend({
          method: 'POST', path: '/api/v1/comm/identity/export',
          body: { nickname, path: picked.filePath, backup_password: backupPassword },
        }, EXPORT_TIMEOUT_MS)
      } finally {
        // Best effort (JS strings are immutable): drop main's only reference.
        backupPassword = null
      }
    }),
  )

  // Restore from an encrypted backup. Step 1 (native open dialog) hands the page
  // only a file NAME and a single-use ticket; the path and the contents never
  // leave main. Step 2 takes the ticket, the nickname the page collected and
  // whether it should become the default; main then asks for the backup
  // password in its own window (with an in-window "decrypting…" state and
  // retry on a wrong password) and calls the daemon.
  handle(IpcChannels.CommImportPick, async (event): Promise<CommImportPickResult> => {
    const win = BrowserWindow.fromWebContents(event.sender)
    const options: Electron.OpenDialogOptions = {
      title: mt('comm.restoreOpen'),
      properties: ['openFile'],
      filters: [{ name: 'NIP-49 ncryptsec', extensions: ['ncryptsec', 'txt'] }],
    }
    let picked: Electron.OpenDialogReturnValue
    try {
      picked = win ? await dialog.showOpenDialog(win, options) : await dialog.showOpenDialog(options)
    } catch {
      return { ok: false, cancelled: true }
    }
    const file = picked.filePaths[0]
    if (picked.canceled || !file) return { ok: false, cancelled: true }
    const ticket = randomBytes(16).toString('hex')
    // Replaces any older selection, which thereby stops working.
    pendingBackup = { id: ticket, path: file, expires: Date.now() + IMPORT_TICKET_TTL_MS, attemptsLeft: IMPORT_MAX_ATTEMPTS }
    return { ok: true, ticket, fileName: path.basename(file) }
  })
  handle(IpcChannels.CommIdentityImport, (event, ticket: unknown, nickname: unknown, makeDefault: unknown): Promise<BackendProxyResponse> =>
    exclusive(async () => {
      if (typeof ticket !== 'string' || !isSafeName(nickname) || typeof makeDefault !== 'boolean') {
        return invalidRequest('invalid restore request')
      }
      const entry = pendingBackup
      if (!entry || entry.id !== ticket || entry.expires < Date.now()) {
        return { ok: false, status: 410, data: { error: { code: 'ticket_expired', message: mt('err.noBackupTicket') } } }
      }
      pendingBackup = null // consumed: re-armed below only on a non-success
      const win = BrowserWindow.fromWebContents(event.sender)
      const result = await promptRestorePassword<BackendProxyResponse>(win, async (backupPassword) => {
        const response = await proxyToBackend({
          method: 'POST', path: '/api/v1/comm/identity/import',
          body: { nickname, path: entry.path, backup_password: backupPassword, default: makeDefault },
        }, EXPORT_TIMEOUT_MS)
        // A wrong password, or input the daemon found unusable (`invalid`), is
        // handled inside the window so the user can fix it; everything else ends it.
        const body = response.data as { error?: unknown; message?: unknown } | null
        if (body?.error === 'bad_backup_password') return { done: false, error: mt('backupPw.badPassword') }
        if (body?.error === 'invalid') {
          return { done: false, error: typeof body.message === 'string' ? clip(body.message, 200) : mt('backupPw.restoreInvalid') }
        }
        return { done: true, value: response }
      }, { willBeDefault: makeDefault })
      // Not finished (cancelled, or the daemon said no): keep the selection so
      // the user can fix the nickname or try again without re-choosing the file,
      // but only a few times, never past the original expiry, and never over a
      // newer pick.
      const finished = result !== null && result.ok
      if (!finished && pendingBackup === null && entry.attemptsLeft > 1 && entry.expires > Date.now()) {
        pendingBackup = { ...entry, attemptsLeft: entry.attemptsLeft - 1 }
      }
      return result ?? cancelled()
    }),
  )

  // Elevated comm writes that no longer ride the generic proxy. Everything the
  // page supplies is validated here (checksum-valid npub, names without
  // control characters, bounded content) and the daemon route is fixed.
  // NOTE: no presence proof is attached. The daemon's resolve_target has no
  // CommWrite support yet (CORE-2d / CORE-7c), and the host credential is not
  // subject to the presence check. Contact add is therefore confirmed natively;
  // SEND is NOT (accepted residual risk until CORE-7c/2d), so a compromised page
  // can still send a validated message to a valid recipient.
  // Adding a contact is confirmed in a native dialog that shows the nickname and
  // the FULL address, so a compromised page cannot silently plant a contact.
  handle(IpcChannels.CommContactAdd, (event, nickname: unknown, npub: unknown, role: unknown): Promise<BackendProxyResponse> =>
    exclusive(async () => {
      if (!isSafeName(nickname)) return invalidRequest('invalid nickname')
      if (!isValidNpub(npub)) return invalidRequest('invalid npub')
      if (role !== undefined && role !== '' && !isSafeName(role)) return invalidRequest('invalid role')
      const ok = await confirmNative(event, mt('comm.contactAdd', { name: clip(nickname, 64), npub }), mt('comm.contactAddDetail'))
      if (!ok) return cancelled()
      return proxyToBackend({
        method: 'POST', path: '/api/v1/comm/contact',
        body: { nickname, npub, ...(typeof role === 'string' && role ? { role } : {}) },
      })
    }),
  )
  handle(IpcChannels.CommSend, (_event, to: unknown, content: unknown): Promise<BackendProxyResponse> => {
    if (!(typeof to === 'string' && to.startsWith('npub1') ? isValidNpub(to) : isSafeName(to))) return Promise.resolve(invalidRequest('invalid recipient'))
    if (!isSafeMessage(content)) return Promise.resolve(invalidRequest('invalid message'))
    // The daemon allows 5 s per relay + 10 s (8 relays max).
    return proxyToBackend({ method: 'POST', path: '/api/v1/comm/send', body: { to, content, encrypt: true } }, 90_000)
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
        return { ok: false, status: rec.status === 404 ? 404 : 502, data: { error: { code: 'approval_unavailable', message: mt('err.approvalUnavailable') } } }
      }
      const a = rec.data as {
        kind?: unknown; summary?: unknown; payload?: unknown; run_id?: unknown; status?: unknown; available_decisions?: unknown
      }
      if (a.status !== undefined && a.status !== 'pending') {
        return { ok: false, status: 409, data: { error: { code: 'not_pending', message: mt('err.notPending', { status: String(a.status) }) } } }
      }
      // The record's own decision list wins when it has one.
      if (Array.isArray(a.available_decisions) && !a.available_decisions.includes(d.type)) {
        return invalidRequest('decision not offered for this approval')
      }
      const decisionLabels: Record<string, string> = {
        approve: mt('approval.approve'), approve_for_session: mt('approval.approve_for_session'),
        deny: mt('approval.deny'), abort: mt('approval.abort'),
      }
      const payload = clip(JSON.stringify(a.payload ?? {}), 600)
      const detail = [
        mt('approval.kind', { value: clip(String(a.kind ?? '?'), 40) }),
        mt('approval.summary', { value: clip(String(a.summary ?? ''), 300) }),
        mt('approval.run', { value: clip(String(a.run_id ?? '?'), 80) }),
        mt('approval.details', { value: payload }),
        typeof d.reason === 'string' && d.reason ? mt('approval.note', { value: clip(d.reason, 200) }) : '',
        mt('approval.warning'),
      ].filter(Boolean).join('\n')
      const ok = await confirmNative(event, mt('approval.message', { id, decision: decisionLabels[d.type as string] ?? d.type }), detail)
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
        return { ok: false, status: 404, data: { error: { code: 'module_unavailable', message: mt('err.moduleUnavailable') } } }
      }
      const ok = await confirmNative(
        event,
        mt('module.change', {
          name,
          from: mod.enabled === true ? mt('module.fromEnabled') : mod.enabled === false ? mt('module.fromDisabled') : '',
          to: enabled ? mt('module.enabled') : mt('module.disabled'),
        }),
        mt('module.detail', { version: clip(String(mod.version ?? '?'), 40) }),
      )
      if (!ok) return cancelled()
      return proxyToBackend({ method: 'PATCH', path: `/api/v1/os/${encodeURIComponent(name)}`, body: { enabled } })
    }),
  )

  // A-3: PATCH /api/v1/attached/agentear — disable / re-enable AgentEar.
  // Elevated in route_policy.rs; main sends it with the ProductHost credential,
  // which the daemon does not gate with a presence challenge, so this native
  // dialog is the human confirmation. Only `enabled` comes from the page; the
  // module name is fixed and any further arguments are ignored.
  handle(IpcChannels.AgentEarSetEnabled, (event, enabled: unknown): Promise<BackendProxyResponse> =>
    exclusive(async () => {
      if (typeof enabled !== 'boolean') return invalidRequest('enabled must be a boolean')
      const refused = await agentearPairedOrFailure()
      if (refused) return refused
      const ok = await confirmNative(
        event,
        mt(enabled ? 'agentear.enableMessage' : 'agentear.disableMessage'),
        mt(enabled ? 'agentear.enableDetail' : 'agentear.disableDetail'),
      )
      if (!ok) return cancelled()
      const res = await proxyToBackend({ method: 'PATCH', path: AGENTEAR_PATH, body: { enabled } })
      return res.ok ? res : agentearFailure(res)
    }),
  )

  // A-3: DELETE /api/v1/attached/agentear — revoke the pairing. AgentEar must
  // pair again (its own menu bar + the presence dialog) to come back.
  handle(IpcChannels.AgentEarUnpair, (event): Promise<BackendProxyResponse> =>
    exclusive(async () => {
      const refused = await agentearPairedOrFailure()
      if (refused) return refused
      const ok = await confirmNative(event, mt('agentear.unpairMessage'), mt('agentear.unpairDetail'))
      if (!ok) return cancelled()
      const res = await proxyToBackend({ method: 'DELETE', path: AGENTEAR_PATH })
      return res.ok ? res : agentearFailure(res)
    }),
  )
}
