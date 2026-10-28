import type { components } from '@agent24/api-client'
import type { BackendProxyRequest } from '../../../shared/ipc-types'

export type IdorisStatus = components['schemas']['IdorisStatus']
export type IdorisOperation = components['schemas']['IdorisOperation']
export type IdorisCandidates = components['schemas']['IdorisCandidates']
export type ConnectRequest = components['schemas']['IdorisConnectRequest']

export class IdorisError extends Error {
  constructor(readonly code: string) { super(code) }
}
function object(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) throw new IdorisError('invalid_response')
  return value as Record<string, unknown>
}
function text(value: unknown): string {
  if (typeof value !== 'string') throw new IdorisError('invalid_response')
  return value
}
function bool(value: unknown): boolean {
  if (typeof value !== 'boolean') throw new IdorisError('invalid_response')
  return value
}
function array(value: unknown): unknown[] {
  if (!Array.isArray(value)) throw new IdorisError('invalid_response')
  return value
}
function choice<const T extends string>(value: unknown, choices: readonly T[]): T {
  const found = choices.find(c => c === value)
  if (found === undefined) throw new IdorisError('invalid_response')
  return found
}
function port(value: unknown): number | null {
  if (value === null) return null
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 1 || value > 65535) throw new IdorisError('invalid_response')
  return value
}
const codes: readonly components['schemas']['IdorisErrorCode'][] = [
  'not_installed', 'not_configured', 'instance_conflict', 'keychain_locked', 'key_unavailable', 'contract_mismatch', 'start_failed', 'stop_timeout', 'busy', 'cancelled', 'not_found', 'not_cancellable', 'shutting_down', 'invalid_request', 'persistence_failed', 'omlx_unreachable', 'omlx_auth_required', 'omlx_auth_rejected', 'unknown_service', 'model_not_found', 'consent_required', 'setup_failed', 'verify_failed', 'replace_required',
]
export function parseStatus(value: unknown): IdorisStatus {
  const v = object(value)
  return {
    state: choice(v.state, ['not_installed', 'not_configured', 'stopped', 'starting', 'running', 'failed', 'instance_conflict']),
    version: v.version === null ? null : text(v.version), data_port: port(v.data_port), admin_port: port(v.admin_port),
    roles: array(v.roles).map(role => choice(role, ['daily', 'deep'])),
    last_error: v.last_error === null ? null : choice(v.last_error, codes),
    current_operation: v.current_operation === null ? null : text(v.current_operation),
  }
}
export function parseOperation(value: unknown): IdorisOperation {
  const v = object(value)
  return {
    operation_id: text(v.operation_id), action: choice(v.action, ['install', 'connect', 'start', 'stop']),
    phase: choice(v.phase, ['queued', 'downloading', 'verifying', 'installing', 'setup', 'issuing_key', 'starting', 'connecting', 'verifying_connection', 'rolling_back', 'done', 'stopping', 'completed']),
    result: v.result === null ? null : choice(v.result, ['succeeded', 'failed', 'cancelled']),
    error_code: v.error_code === null ? null : choice(v.error_code, codes),
    cancellable: bool(v.cancellable), idempotency_key: text(v.idempotency_key),
  }
}
function parseCandidates(value: unknown): IdorisCandidates {
  const v = object(value)
  return {
    key_from_local_settings: bool(v.key_from_local_settings),
    candidates: array(v.candidates).map(value => {
      const c = object(value)
      return { endpoint: text(c.endpoint), identity: choice(c.identity, ['omlx', 'unknown_service']), authentication: choice(c.authentication, ['ready', 'auth_required', 'auth_rejected', 'unreachable', 'unknown_service', 'invalid_response']), models: array(c.models).map(text) }
    }),
  }
}
async function request<T>(req: BackendProxyRequest, parse: (value: unknown) => T): Promise<T> {
  let res
  try { res = await window.agent24.backendProxy(req) } catch { throw new IdorisError('backend_not_ready') }
  if (!res.ok) {
    const error = object(object(res.data).error)
    throw new IdorisError(text(error.code))
  }
  return parse(res.data)
}
export const getStatus = () => request({ method: 'GET', path: '/api/v1/idoris/status' }, parseStatus)
export const getOperation = (id: string) => request({ method: 'GET', path: `/api/v1/idoris/operations/${encodeURIComponent(id)}` }, parseOperation)
const candidatesPath = '/api/v1/idoris/omlx/candidates'
export const getCandidates = (endpoint?: string) => request({ method: 'GET', path: endpoint ? `${candidatesPath}?endpoint=${encodeURIComponent(endpoint)}` : candidatesPath }, parseCandidates)
export const cancelOperation = (id: string) => request({ method: 'POST', path: `/api/v1/idoris/operations/${encodeURIComponent(id)}/cancel` }, parseOperation)
const actionPaths = {
  install: '/api/v1/idoris/install', connect: '/api/v1/idoris/connect',
  start: '/api/v1/idoris/start', stop: '/api/v1/idoris/stop',
} satisfies Record<IdorisOperation['action'], string>
export const submitOperation = (action: IdorisOperation['action'], body?: ConnectRequest | { consent: boolean }) => request({ method: 'POST', path: actionPaths[action], ...(body === undefined ? {} : { body }), idempotencyKey: crypto.randomUUID() }, parseOperation)
