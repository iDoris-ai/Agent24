import type { components } from '@agent24/api-client'
import type { BackendProxyRequest } from '../../../shared/ipc-types'

export type ModelSource = NonNullable<components['schemas']['IdorisConnectRequest']['source']>
export type IdorisStatus = components['schemas']['IdorisStatus'] & { source?: ModelSource }
export type IdorisOperation = Omit<components['schemas']['IdorisOperation'], 'error_code' | 'setup_code'> & { error_code: string | null; setup_code?: string; setup_endpoint?: string }
export type IdorisCandidates = components['schemas']['IdorisCandidates'] & { setup?: ReturnType<typeof parseOmlxSetup> }
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
function bytes(value: unknown): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) throw new IdorisError('invalid_response')
  return value
}
function parseVerification(value: unknown): components['schemas']['IdorisVerification'] {
  const v = object(value)
  const verifiedAt = text(v.verified_at)
  if (!Number.isFinite(Date.parse(verifiedAt))) throw new IdorisError('invalid_response')
  return { model_id: text(v.model_id), record_id: text(v.record_id), locality: choice(v.locality, ['loopback']), verified_at: verifiedAt }
}
function parseBudget(value: unknown): components['schemas']['IdorisBudgetSuggestion'] {
  const v = object(value)
  return { core_reserved_bytes: bytes(v.core_reserved_bytes), idoris_budget_bytes: bytes(v.idoris_budget_bytes) }
}
function parseProgress(value: unknown): components['schemas']['IdorisDownloadProgress'] {
  const v = object(value)
  const done = bytes(v.bytes_done)
  if (v.bytes_total === undefined) return { bytes_done: done }
  const total = bytes(v.bytes_total)
  if (total < done) throw new IdorisError('invalid_response')
  return { bytes_done: done, bytes_total: total }
}
const codes: readonly components['schemas']['IdorisErrorCode'][] = [
  'not_installed', 'not_configured', 'instance_conflict', 'keychain_locked', 'key_unavailable', 'contract_mismatch', 'start_failed', 'stop_timeout', 'busy', 'cancelled', 'not_found', 'not_cancellable', 'shutting_down', 'invalid_request', 'persistence_failed', 'omlx_unreachable', 'omlx_auth_required', 'omlx_auth_rejected', 'unknown_service', 'model_not_found', 'consent_required', 'setup_failed', 'verify_failed', 'replace_required',
]
export function parseStatus(value: unknown): IdorisStatus {
  const v = object(value)
  const source = v.source ?? (v.connection == null ? undefined : object(v.connection).source)
  return {
    ...(v.crashes === undefined ? {} : { crashes: bytes(v.crashes) }),
    ...(v.hangs === undefined ? {} : { hangs: bytes(v.hangs) }),
    ...(v.circuit_open === undefined ? {} : { circuit_open: bool(v.circuit_open) }),
    ...(source === undefined ? {} : { source: choice(source, ['omlx', 'openai_compat']) }),
    state: choice(v.state, ['not_installed', 'not_configured', 'stopped', 'starting', 'degraded', 'running', 'failed', 'instance_conflict']),
    version: v.version === null ? null : text(v.version), data_port: port(v.data_port), admin_port: port(v.admin_port),
    roles: array(v.roles).map(role => choice(role, ['daily', 'deep', 'embed'])),
    last_error: v.last_error === null ? null : choice(v.last_error, codes),
    // Added with ADR-038 C1; an older daemon omits them, which reads as "no migration pending".
    migration_pending: v.migration_pending === undefined ? false : v.migration_pending === true,
    last_migration_error: v.last_migration_error == null ? null : choice(v.last_migration_error, codes),
    current_operation: v.current_operation === null ? null : text(v.current_operation),
    verification: v.verification === null ? null : parseVerification(v.verification),
    budget_suggestion: v.budget_suggestion === null ? null : parseBudget(v.budget_suggestion),
    budget_unavailable_reason: v.budget_unavailable_reason === null ? null : choice(v.budget_unavailable_reason, ['hardware_memory_unavailable', 'hardware_probe_failed', 'host_unavailable']),
  }
}
export function parseOperation(value: unknown): IdorisOperation {
  const v = object(value)
  return {
    operation_id: text(v.operation_id), action: choice(v.action, ['install', 'connect', 'start', 'stop']),
    phase: choice(v.phase, ['queued', 'downloading', 'verifying', 'installing', 'setup', 'issuing_key', 'starting', 'connecting', 'verifying_connection', 'rolling_back', 'done', 'stopping', 'completed']),
    result: v.result === null ? null : choice(v.result, ['succeeded', 'failed', 'cancelled']),
    error_code: v.error_code === null ? null : text(v.error_code),
    cancellable: bool(v.cancellable),
    ...(v.setup_endpoint === undefined ? {} : { setup_endpoint: text(v.setup_endpoint) }),
    ...(v.setup_code === undefined ? {} : { setup_code: text(v.setup_code) }),
    ...(v.download_progress === undefined ? {} : { download_progress: parseProgress(v.download_progress) }),
    ...(v.verification === undefined ? {} : { verification: parseVerification(v.verification) }),
  }
}
function parseOmlxSetup(value: unknown) {
  const v = object(value)
  const engine = object(v.engine)
  return {
    endpoint: text(v.endpoint), mode: choice(v.mode, ['adopt', 'managed']),
    engine: {
      engine_kind: choice(engine.engine_kind, ['omlx']), installed: bool(engine.installed),
      executable: engine.executable === null ? null : text(engine.executable),
      ...(engine.version === undefined ? {} : { version: text(engine.version) }),
    },
  }
}
function parseCandidates(value: unknown): IdorisCandidates {
  const v = object(value)
  return {
    key_from_local_settings: bool(v.key_from_local_settings),
    ...(v.setup === undefined ? {} : { setup: parseOmlxSetup(v.setup) }),
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
export const getUpstreamModels = (endpoint: string) => request({ method: 'GET', path: `/api/v1/idoris/upstream-models?endpoint=${encodeURIComponent(endpoint)}` }, value => ({ models: array(object(value).models).map(text) }))
export const getOperation = (id: string) => request({ method: 'GET', path: `/api/v1/idoris/operations/${encodeURIComponent(id)}` }, parseOperation)
const candidatesPath = '/api/v1/idoris/omlx/candidates'
export const getCandidates = (endpoint?: string) => request({ method: 'GET', path: endpoint ? `${candidatesPath}?endpoint=${encodeURIComponent(endpoint)}` : candidatesPath }, parseCandidates)
export const cancelOperation = (id: string) => request({ method: 'POST', path: `/api/v1/idoris/operations/${encodeURIComponent(id)}/cancel` }, parseOperation)
const actionPaths = {
  install: '/api/v1/idoris/install', connect: '/api/v1/idoris/connect',
  start: '/api/v1/idoris/start', stop: '/api/v1/idoris/stop',
} satisfies Record<IdorisOperation['action'], string>
export const submitOperation = (action: IdorisOperation['action'], body?: ConnectRequest | { consent: boolean }) => request({ method: 'POST', path: actionPaths[action], ...(body === undefined ? {} : { body }), idempotencyKey: crypto.randomUUID() }, parseOperation)
