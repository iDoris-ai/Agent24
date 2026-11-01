// Typed wrappers for the COMM REST surface. The renderer is sandboxed: every
// request goes through the existing Electron IPC backend proxy.
import type { components } from '@agent24/api-client'
import { translate } from '../../i18n'
import { communicationMessages } from './Communication.i18n'

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
  installable?: boolean
  installed_version?: string | null
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
    if (typeof error === 'string' && error.length > 0) {
      // The comm routes answer {ok:false, error:"<code>", message:"<why>"}:
      // keep the code for UI decisions and the message for the user.
      const why = value['message']
      return typeof why === 'string' && why.length > 0 ? { message: why, code: error } : { message: error }
    }
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
  return { message: translate(communicationMessages, 'api.requestFailed', { status }) }
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
    throw new CommApiError(translate(communicationMessages, 'api.invalidProxy'), 0)
  }
  if (!response.ok || response.status >= 400) {
    const fields = errorFields(response.data, response.status)
    throw new CommApiError(fields.message, response.status, fields.code)
  }
  if (!isRecord(response.data)) {
    throw new CommApiError(translate(communicationMessages, 'api.invalidEnvelope'), response.status)
  }
  if (response.data['ok'] === false) {
    const fields = errorFields(response.data, response.status)
    throw new CommApiError(fields.message, response.status, fields.code)
  }
  if (response.data['ok'] !== true || !('data' in response.data)) {
    throw new CommApiError(translate(communicationMessages, 'api.invalidEnvelope'), response.status)
  }
  const data = response.data['data']
  if (!validate(data)) {
    throw new CommApiError(translate(communicationMessages, 'api.malformedData'), response.status)
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
    (value['installable'] === undefined || typeof value['installable'] === 'boolean') &&
    (value['installed_version'] === undefined || nullableString(value['installed_version'])) &&
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
  method: 'GET' | 'POST' | 'PUT',
  path: string,
  validate: (value: unknown) => value is T,
  body?: unknown,
): Promise<T> {
  let response: unknown
  try {
    response = await window.agent24.backendProxy({ method, path, ...(body === undefined ? {} : { body }) })
  } catch {
    throw new CommApiError(translate(communicationMessages, 'api.backendFailed'), 0)
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
    throw new CommApiError(translate(communicationMessages, 'api.unlockFailed'), 0)
  }

  try {
    return decodeEnvelope(response, isUnlockResult)
  } catch (error) {
    if (error instanceof CommApiError && password && error.message.includes(password)) {
      throw new CommApiError(translate(communicationMessages, 'api.unlockFailed'), error.status, error.code)
    }
    throw error
  }
}

export type CommInstallResult = components['schemas']['CommInstallResult']
function isInstallResult(value: unknown): value is CommInstallResult {
  return isRecord(value) && string(value['installed_version']) && string(value['path']) &&
    typeof value['restart_required'] === 'boolean' && nullableString(value['reason'])
}

export async function installComm(): Promise<CommInstallResult> {
  // Main owns the elevated native confirmation and the fixed installation URL.
  const response: unknown = await window.agent24.commInstall()
  return decodeEnvelope(response, isInstallResult)
}

// Create responses are passed through by Hyphae; only the public key and
// nickname are required here. Authoritative identity flags come from list.
export const createCommIdentity = (nickname: string) =>
  request('POST', '/api/v1/comm/identity', (value): value is { nickname: string; npub: string } =>
    isRecord(value) && string(value['nickname']) && string(value['npub']), { nickname, default: true })

/** Replaces the whole relay list server-side; callers pass the full ordered list. */
export const setCommRelay = (relays: string[]) =>
  request('PUT', '/api/v1/comm/relay', isRelayConfig, { relays })

export const probeCommRelay = (url: string) =>
  request('POST', '/api/v1/comm/relay/probe', (value): value is { url: string | null; connected: boolean; error: string | null } =>
    isRecord(value) && nullableString(value['url']) && typeof value['connected'] === 'boolean' && nullableString(value['error']), { url })

export async function importCommIdentity(from: string, password: string) {
  try {
    return await request('POST', '/api/v1/comm/import', (value): value is { identities: number; contacts: number; outbox: number; db_files: string[] } =>
      isRecord(value) && typeof value['identities'] === 'number' && typeof value['contacts'] === 'number' &&
      typeof value['outbox'] === 'number' && Array.isArray(value['db_files']) && value['db_files'].every(string),
    { from, ...(password ? { password } : {}), confirm: true })
  } catch (error) {
    if (password && error instanceof CommApiError && error.message.includes(password)) {
      throw new CommApiError(translate(communicationMessages, 'api.importFailed'), error.status, error.code)
    }
    throw error
  }
}

/** Elevated writes go through dedicated, validated IPC in main, never the generic proxy. */
async function viaIpc<T>(call: () => Promise<unknown>, validate: (value: unknown) => value is T): Promise<T> {
  let response: unknown
  try {
    response = await call()
  } catch {
    throw new CommApiError(translate(communicationMessages, 'api.backendFailed'), 0)
  }
  return decodeEnvelope(response, validate)
}

export const addCommContact = (nickname: string, npub: string, role: string) =>
  viaIpc(() => window.agent24.commContactAdd(nickname, npub, role),
    (value): value is RecordValue => isRecord(value) || value === null)

export interface CommSendResult {
  published_to: number
}

export const sendCommMessage = (to: string, content: string): Promise<CommSendResult> =>
  viaIpc(() => window.agent24.commSend(to, content), (value): value is RecordValue => isRecord(value))
    .then((data) => ({ published_to: typeof data['published_to'] === 'number' ? data['published_to'] : 0 }))

export interface CommHistoryItem {
  id: string
  from: string
  content: string
  at: string
}

function historyTime(raw: unknown): string {
  if (typeof raw === 'number' && Number.isFinite(raw)) {
    return new Date(raw < 1e12 ? raw * 1000 : raw).toLocaleString()
  }
  return typeof raw === 'string' ? raw : ''
}

/** Hyphae's history rows are passed through; keep only what the compact list shows. */
function toHistoryItems(value: unknown[]): CommHistoryItem[] {
  const items: CommHistoryItem[] = []
  for (const row of value) {
    if (!isRecord(row)) continue
    const content = [row['content'], row['plaintext']].find(string)
    if (content === undefined) continue
    const from = [row['from'], row['sender'], row['sender_npub']].find(string) ?? ''
    const id = row['id']
    items.push({
      id: typeof id === 'string' || typeof id === 'number' ? String(id) : `${items.length}`,
      from,
      content,
      at: historyTime(row['created_at'] ?? row['received_at'] ?? row['time'] ?? row['ts']),
    })
  }
  return items
}

export const listCommHistory = (limit = 20): Promise<CommHistoryItem[]> =>
  request('GET', `/api/v1/comm/history?limit=${limit}`, (value): value is unknown[] => Array.isArray(value)).then(toHistoryItems)

export interface CommExportResult {
  nickname: string
  npub: string
  path: string
}

/**
 * Password-encrypted private-key backup (NIP-49). The main process asks for
 * confirmation, lets the user pick the file and has the daemon create it
 * (0600, never overwriting); only {nickname,npub,path} comes back. A cancelled
 * dialog rejects with code `cancelled`; an existing file with `target_exists`.
 */
export const exportCommIdentity = (nickname: string, backupPassword: string): Promise<CommExportResult> =>
  viaIpc(() => window.agent24.commIdentityExport(nickname, backupPassword), (value): value is CommExportResult =>
    isRecord(value) && string(value['nickname']) && string(value['npub']) && string(value['path']))
