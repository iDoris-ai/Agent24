// Service mode: a FirstPartyShell token may read a module, but a write is
// 403 `module_delegation_required` until `POST /api/v1/os/{name}/delegation`
// has been confirmed. The desktop never called that route. This is the one
// place that does, then retries the write once.
//
// The module name is the `name` on `GET /api/v1/os` whose `namespace` owns
// the request path. It is not a fixed table.
//
// FU: two writes that both observe the 403 each ask once. A documents
// client whose paths are not under `/api/v1/documents` cannot be matched.

import type { BackendProxyRequest, BackendProxyResponse } from '../shared/ipc-types'
import { daemonConnection, daemonElevation } from './backend-manager'
import { proxyError, proxyRequest, type ProxyOptions } from './backend-proxy'
import { sendElevating, type ElevatingDeps } from './elevated-request'

export const MODULE_DELEGATION_REQUIRED = 'module_delegation_required'

/** Every documents-client request is under this prefix (`BASE` in
 * `agent24-documents-client`). The module's name still comes from the OS list. */
export const DOCUMENTS_MODULE_PREFIX = '/api/v1/documents'

const READ: ReadonlySet<string> = new Set(['GET', 'HEAD'])

export interface DelegationDeps extends ElevatingDeps {}

export function daemonDelegationDeps(): DelegationDeps {
  return { connection: daemonConnection, elevation: daemonElevation, send: proxyRequest }
}

export function isModuleDelegationRequired(res: BackendProxyResponse): boolean {
  return res.status === 403 && errorCode(res.data) === MODULE_DELEGATION_REQUIRED
}

/** Longest namespace that owns `requestPath`, then that module's name. */
export function moduleNameForPath(requestPath: string, modules: readonly { name: string; namespace: string }[]): string | null {
  const path = requestPath.split(/[?#]/, 1)[0] ?? ''
  let best: { name: string; namespace: string } | null = null
  for (const mod of modules) {
    const ns = mod.namespace
    if (!ns.startsWith('/api/v1/') || ns.includes('..')) continue
    const owns = path === ns || path.startsWith(ns.endsWith('/') ? ns : `${ns}/`)
    if (!owns) continue
    if (!best || ns.length > best.namespace.length) best = mod
  }
  if (!best || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(best.name)) return null
  return best.name
}

/** Send `req`. On 403 `module_delegation_required` for a write, confirm
 * delegation once (the presence box is `sendElevating`) and send `req` once
 * more. A refusal or any other failure is returned as-is, with a message. */
export async function sendModuleWrite(
  req: BackendProxyRequest,
  opts: ProxyOptions,
  deps: DelegationDeps,
): Promise<BackendProxyResponse> {
  const first = await sendElevating(req, opts, deps)
  if (READ.has(req.method) || !isModuleDelegationRequired(first)) return first
  const established = await establishModuleDelegation(req.path, opts, deps)
  if (!established.ok) return displayed(established)
  return sendElevating(req, opts, deps)
}

/** `POST /api/v1/os/{name}/delegation` for the module that owns `requestPath`. */
export async function establishModuleDelegation(
  requestPath: string,
  opts: ProxyOptions,
  deps: DelegationDeps,
): Promise<BackendProxyResponse> {
  const send = deps.send ?? proxyRequest
  const conn = deps.connection()
  if (!conn) return proxyError('backend_not_ready', 'Backend daemon is not ready (no announced port or credential)')
  const list = await send(conn, { method: 'GET', path: '/api/v1/os' }, withoutActor(opts))
  if (!list.ok) return displayed(list)
  const name = moduleNameForPath(requestPath, modulesOf(list.data))
  if (!name) {
    return displayed({
      ok: false,
      status: 403,
      data: { error: { code: 'module_unknown', message: 'No installed module owns this request' } },
    })
  }
  return sendElevating(
    { method: 'POST', path: `/api/v1/os/${encodeURIComponent(name)}/delegation` },
    withoutActor(opts),
    deps,
  )
}

/** A failure the page can show. A body that already has a message is kept. */
export function displayed(res: BackendProxyResponse): BackendProxyResponse {
  if (errorMessage(res.data)) return res
  return {
    ok: false,
    status: res.status || 403,
    data: {
      error: {
        code: errorCode(res.data) || 'module_delegation_denied',
        message: 'The desktop could not get permission to write this module',
      },
    },
  }
}

function withoutActor(opts: ProxyOptions): ProxyOptions {
  if (opts.actorKey === undefined) return opts
  const rest: ProxyOptions = { ...opts }
  delete rest.actorKey
  return rest
}

function modulesOf(data: unknown): { name: string; namespace: string }[] {
  if (typeof data !== 'object' || data === null) return []
  const modules = (data as { modules?: unknown }).modules
  if (!Array.isArray(modules)) return []
  const out: { name: string; namespace: string }[] = []
  for (const row of modules) {
    if (typeof row !== 'object' || row === null) continue
    const name = (row as { name?: unknown }).name
    const namespace = (row as { namespace?: unknown }).namespace
    if (typeof name === 'string' && typeof namespace === 'string') out.push({ name, namespace })
  }
  return out
}

function errorCode(data: unknown): string {
  const code = errorOf(data)?.code
  return typeof code === 'string' ? code : ''
}

function errorMessage(data: unknown): string {
  const message = errorOf(data)?.message
  return typeof message === 'string' ? message : ''
}

function errorOf(data: unknown): { code?: unknown; message?: unknown } | null {
  if (typeof data !== 'object' || data === null || !('error' in data)) return null
  const error = (data as { error?: unknown }).error
  return typeof error === 'object' && error !== null ? error as { code?: unknown; message?: unknown } : null
}
