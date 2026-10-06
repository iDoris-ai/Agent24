// Typed wrappers for the COMM REST surface. The renderer is sandboxed: every
// request goes through the existing Electron IPC backend proxy.

export interface CommIdentity {
  nickname: string
  npub: string
  default: boolean
  encrypted: boolean
}

export interface CommContact {
  nickname: string
  npub: string
  role: string
}

export interface CommRelayConfig {
  relays: string[]
  source: string
  configured: boolean
}

export type CommDaemonProcessState =
  | 'stopped'
  | 'starting'
  | 'running'
  | 'backoff'
  | 'locked'
  | 'gave_up'
  | 'not_configured'

export interface CommDaemonStatus {
  process: {
    state: CommDaemonProcessState
    generation: number
    consecutive_failures: number
    reason: string | null
  }
  relay_probe: {
    url: string | null
    connected: boolean
    at_ms: number
    error: string | null
  } | null
  catch_up: {
    state: 'unknown' | 'incomplete'
    last_incomplete_at_ms: number | null
  }
}

export interface CommUnlockResult {
  unlocked: true
  remembered: boolean
}

/** Keeps daemon error codes (e.g. locked/not_configured/binary_rejected) for UI decisions. */
export class CommApiError extends Error {
  readonly code?: string
  readonly status: number

  constructor(message: string, status: number, code?: string) {
    super(message)
    this.name = 'CommApiError'
    this.status = status
    this.code = code
  }
}

type RecordValue = Record<string, unknown>
type ProxyResponse = { ok: boolean; status: number; data: unknown }

function isRecord(value: unknown): value is RecordValue {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function errorFields(value: unknown, status: number): { message: string; code?: string } {
  if (isRecord(value)) {
    const error = value['error']
    if (typeof error === 'string' && error.length > 0) return { message: error }
    if (isRecord(error)) {
      const message = error['message']
      const code = error['code']
      if (typeof message === 'string' && message.length > 0) {
        return { message, ...(typeof code === 'string' ? { code } : {}) }
      }
    }
    const message = value['message']
    const code = value['code']
    if (typeof message === 'string' && message.length > 0) {
      return { message, ...(typeof code === 'string' ? { code } : {}) }
    }
  }
  return { message: `COMM request failed (HTTP ${status})` }
}

function isProxyResponse(value: unknown): value is ProxyResponse {
  return (
    isRecord(value) &&
    typeof value['ok'] === 'boolean' &&
    typeof value['status'] === 'number' &&
    Number.isInteger(value['status']) &&
    'data' in value
  )
}

function decodeEnvelope<T>(response: unknown, validate: (value: unknown) => value is T): T {
  if (!isProxyResponse(response)) {
    throw new CommApiError('Invalid backend proxy response', 0)
  }
  if (!response.ok || response.status >= 400) {
    const fields = errorFields(response.data, response.status)
    throw new CommApiError(fields.message, response.status, fields.code)
  }
  if (!isRecord(response.data)) {
    throw new CommApiError('Invalid COMM response envelope', response.status)
  }
  if (response.data['ok'] === false) {
    const fields = errorFields(response.data, response.status)
    throw new CommApiError(fields.message, response.status, fields.code)
  }
  if (response.data['ok'] !== true || !('data' in response.data)) {
    throw new CommApiError('Invalid COMM response envelope', response.status)
  }
  const data = response.data['data']
  if (!validate(data)) {
    throw new CommApiError('Malformed COMM response data', response.status)
  }
  return data
}

function string(value: unknown): value is string {
  return typeof value === 'string'
}

function nullableString(value: unknown): value is string | null {
  return value === null || string(value)
}

function isIdentity(value: unknown): value is CommIdentity {
  return (
    isRecord(value) &&
    string(value['nickname']) && value['nickname'].length > 0 &&
    string(value['npub']) && value['npub'].length > 0 &&
    typeof value['default'] === 'boolean' &&
    typeof value['encrypted'] === 'boolean'
  )
}

function isContact(value: unknown): value is CommContact {
  return (
    isRecord(value) &&
    string(value['nickname']) && value['nickname'].length > 0 &&
    string(value['npub']) && value['npub'].length > 0 &&
    string(value['role'])
  )
}

function isIdentityList(value: unknown): value is CommIdentity[] {
  return Array.isArray(value) && value.every(isIdentity)
}

function isContactList(value: unknown): value is CommContact[] {
  return Array.isArray(value) && value.every(isContact)
}

function isRelayConfig(value: unknown): value is CommRelayConfig {
  return (
    isRecord(value) &&
    Array.isArray(value['relays']) && value['relays'].every((relay) => string(relay) && relay.length > 0) &&
    string(value['source']) && value['source'].length > 0 &&
    typeof value['configured'] === 'boolean'
  )
}

function isRelayProbe(value: unknown): value is NonNullable<CommDaemonStatus['relay_probe']> {
  return (
    isRecord(value) &&
    nullableString(value['url']) &&
    typeof value['connected'] === 'boolean' &&
    typeof value['at_ms'] === 'number' && Number.isFinite(value['at_ms']) &&
    nullableString(value['error'])
  )
}

function isDaemonStatus(value: unknown): value is CommDaemonStatus {
  if (!isRecord(value) || !isRecord(value['process']) || !isRecord(value['catch_up'])) return false
  const process = value['process']
  const catchUp = value['catch_up']
  const states: readonly string[] = ['stopped', 'starting', 'running', 'backoff', 'locked', 'gave_up', 'not_configured']
  return (
    string(process['state']) && states.includes(process['state']) &&
    typeof process['generation'] === 'number' && Number.isInteger(process['generation']) &&
    typeof process['consecutive_failures'] === 'number' && Number.isInteger(process['consecutive_failures']) &&
    nullableString(process['reason']) &&
    (value['relay_probe'] === null || isRelayProbe(value['relay_probe'])) &&
    (catchUp['state'] === 'unknown' || catchUp['state'] === 'incomplete') &&
    (catchUp['last_incomplete_at_ms'] === null ||
      (typeof catchUp['last_incomplete_at_ms'] === 'number' && Number.isFinite(catchUp['last_incomplete_at_ms'])))
  )
}

function isUnlockResult(value: unknown): value is CommUnlockResult {
  return isRecord(value) && value['unlocked'] === true && typeof value['remembered'] === 'boolean'
}

async function request<T>(
  method: 'GET' | 'POST',
  path: string,
  validate: (value: unknown) => value is T,
  body?: unknown,
): Promise<T> {
  let response: unknown
  try {
    response = await window.agent24.backendProxy({ method, path, ...(body === undefined ? {} : { body }) })
  } catch {
    throw new CommApiError('COMM backend request failed', 0)
  }
  return decodeEnvelope(response, validate)
}

export const listCommIdentities = (): Promise<CommIdentity[]> =>
  request('GET', '/api/v1/comm/identity', isIdentityList)

export const listCommContacts = (): Promise<CommContact[]> =>
  request('GET', '/api/v1/comm/contact', isContactList)

export const listCommRelays = (): Promise<CommRelayConfig> =>
  request('GET', '/api/v1/comm/relay', isRelayConfig)

export const getCommStatus = (): Promise<CommDaemonStatus> =>
  request('GET', '/api/v1/comm/daemon', isDaemonStatus)

export const startCommDaemon = (): Promise<CommDaemonStatus> =>
  request('POST', '/api/v1/comm/daemon/start', isDaemonStatus)

export const stopCommDaemon = (): Promise<CommDaemonStatus> =>
  request('POST', '/api/v1/comm/daemon/stop', isDaemonStatus)

/** Unlock is deliberately sent once. Password is only present in this IPC body and is never logged or persisted here. */
export async function unlockComm(password: string, remember = false): Promise<CommUnlockResult> {
  let response: unknown
  try {
    response = await window.agent24.backendProxy({
      method: 'POST',
      path: '/api/v1/comm/unlock',
      body: { password, remember },
    })
  } catch {
    // IPC/native errors can contain implementation details; keep this path
    // generic so a rejected call can never echo a password into UI text.
    throw new CommApiError('Unlock request failed', 0)
  }

  try {
    return decodeEnvelope(response, isUnlockResult)
  } catch (error) {
    if (error instanceof CommApiError && password && error.message.includes(password)) {
      throw new CommApiError('Unlock request failed', error.status, error.code)
    }
    throw error
  }
}
