// IPC handler registry. M0/M1 starter handlers; capability modules will
// register their own handlers here via the module loader (later M1 task).

import http from 'node:http'
import { app, ipcMain, shell } from 'electron'
import { IpcChannels } from '../../shared/ipc-types'
import type { BackendProxyRequest, BackendProxyResponse } from '../../shared/ipc-types'

const BACKEND_PORT = 8765
const BACKEND_HOST = '127.0.0.1'

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
