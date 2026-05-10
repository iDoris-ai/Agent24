// IPC handler registry. M0/M1 starter handlers; capability modules will
// register their own handlers here via the module loader (later M1 task).

import http from 'node:http'
import { execFile, type ChildProcess } from 'node:child_process'
import { app, ipcMain, shell } from 'electron'
import { IpcChannels } from '../../shared/ipc-types'
import type {
  BackendProxyRequest,
  BackendProxyResponse,
  OmlxDetectResult,
  OmlxModelsResult,
  OmlxStartResult,
  OmlxStopResult,
} from '../../shared/ipc-types'

const BACKEND_PORT = 8765
const BACKEND_HOST = '127.0.0.1'

// oMLX server management
let omlxProcess: ChildProcess | null = null
const OMLX_PROBE_CANDIDATES = [
  { url: 'http://127.0.0.1:8088', apiKey: 'xiaobao8088' },
  { url: 'http://127.0.0.1:8000', apiKey: '' },
  { url: 'http://127.0.0.1:8001', apiKey: '' },
  { url: 'http://localhost:8088', apiKey: 'xiaobao8088' },
]

function fetchOmlxModels(url: string, apiKey: string): Promise<string[]> {
  return new Promise((resolve) => {
    const headers: Record<string, string> = { 'Accept': 'application/json' }
    if (apiKey) headers['Authorization'] = `Bearer ${apiKey}`
    const parsedUrl = new URL(`${url}/v1/models`)
    const options: http.RequestOptions = {
      hostname: parsedUrl.hostname,
      port: parsedUrl.port || 80,
      path: parsedUrl.pathname,
      method: 'GET',
      headers,
      timeout: 2000,
    }
    const req = http.request(options, (res) => {
      const chunks: Buffer[] = []
      res.on('data', (c: Buffer) => chunks.push(c))
      res.on('end', () => {
        try {
          const body = JSON.parse(Buffer.concat(chunks).toString()) as { data?: { id: string }[] }
          resolve((body.data ?? []).map((m) => m.id))
        } catch { resolve([]) }
      })
    })
    req.on('error', () => resolve([]))
    req.on('timeout', () => { req.destroy(); resolve([]) })
    req.end()
  })
}

// Allowlist of URL prefixes permitted for shell.openExternal. Anything
// not matching is rejected silently — prevents IPC injection opening
// file:// or other dangerous schemes.
const EXTERNAL_URL_ALLOWLIST = ['https://', 'http://'] as const

function proxyToBackend(req: BackendProxyRequest): Promise<BackendProxyResponse> {
  return new Promise((resolve) => {
    const body = req.body !== undefined ? JSON.stringify(req.body) : undefined
    const options: http.RequestOptions = {
      host: BACKEND_HOST,
      port: BACKEND_PORT,
      path: req.path,
      method: req.method,
      headers: {
        'Content-Type': 'application/json',
        ...(body ? { 'Content-Length': Buffer.byteLength(body) } : {}),
      },
    }

    const outReq = http.request(options, (res) => {
      const chunks: Buffer[] = []
      res.on('data', (chunk: Buffer) => chunks.push(chunk))
      res.on('end', () => {
        let data: unknown = null
        try { data = JSON.parse(Buffer.concat(chunks).toString()) } catch { data = null }
        resolve({ ok: (res.statusCode ?? 500) < 400, status: res.statusCode ?? 500, data })
      })
    })

    outReq.on('error', (err) => {
      resolve({ ok: false, status: 503, data: { error: (err as Error).message } })
    })

    if (body) outReq.write(body)
    outReq.end()
  })
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

export function registerIpcHandlers(): void {
  // oMLX: auto-detect running server
  ipcMain.handle(IpcChannels.OmlxDetect, async (): Promise<OmlxDetectResult | null> => {
    for (const candidate of OMLX_PROBE_CANDIDATES) {
      const models = await fetchOmlxModels(candidate.url, candidate.apiKey)
      if (models.length > 0) return { ...candidate, models }
    }
    return null
  })

  // oMLX: list models from given url+key
  ipcMain.handle(IpcChannels.OmlxModels, async (_event, url: unknown, apiKey: unknown): Promise<OmlxModelsResult> => {
    if (typeof url !== 'string') return { ok: false, models: [], error: 'invalid url' }
    const models = await fetchOmlxModels(url, typeof apiKey === 'string' ? apiKey : '')
    return models.length > 0 ? { ok: true, models } : { ok: false, models: [], error: 'no response or no models' }
  })

  // oMLX: start server via `omlx serve`
  ipcMain.handle(IpcChannels.OmlxStart, (_event, port: unknown, apiKey: unknown): OmlxStartResult => {
    if (omlxProcess) return { ok: true, url: `http://127.0.0.1:${port ?? 8000}` }
    const args = ['serve', '--port', String(port ?? 8000)]
    if (typeof apiKey === 'string' && apiKey) args.push('--api-key', apiKey)
    omlxProcess = execFile('omlx', args, { env: { ...process.env } })
    omlxProcess.on('exit', () => { omlxProcess = null })
    return { ok: true, url: `http://127.0.0.1:${port ?? 8000}` }
  })

  // oMLX: stop server
  ipcMain.handle(IpcChannels.OmlxStop, (): OmlxStopResult => {
    if (omlxProcess) { omlxProcess.kill('SIGTERM'); omlxProcess = null }
    return { ok: true }
  })
  ipcMain.handle(IpcChannels.AppPing, () => 'pong')
  ipcMain.handle(IpcChannels.AppVersion, () => app.getVersion())
  ipcMain.handle(IpcChannels.ShellOpenExternal, (_event, url: unknown) => {
    if (typeof url !== 'string') return
    const allowed = EXTERNAL_URL_ALLOWLIST.some((prefix) => url.startsWith(prefix))
    if (allowed) {
      shell.openExternal(url).catch((err) => console.error('openExternal failed', err))
    }
  })
  ipcMain.handle(IpcChannels.BackendProxy, (_event, req: unknown) => {
    if (!isBackendProxyRequest(req)) {
      return {
        ok: false,
        status: 400,
        data: { error: 'valid method and absolute path required' },
      } satisfies BackendProxyResponse
    }
    return proxyToBackend(req)
  })
}
