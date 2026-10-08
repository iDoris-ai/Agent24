import http from 'node:http'
import { close, createReadStream } from 'node:fs'
import type { IdorisStatusResponse, IdorisStatusResult, IdorisUnavailableReason } from '../shared/ipc-types'

const TOKEN_ARG = '--idoris-admin-token-fd'
const TOKEN_FRAME_BYTES = 65
const TOKEN_READ_TIMEOUT_MS = 5_000
const ADMIN_RESPONSE_TIMEOUT_MS = 5_000
const MAX_ADMIN_RESPONSE_BYTES = 256 * 1024
const DEFAULT_ADMIN_PORT = 8741
const ADMIN_PORT_ENV = 'IDORIS_ADMIN_PORT'
const ADMIN_HOST = '127.0.0.1'
const STATUS_PATH = '/admin/api/v1/status'

type StatusProvider = () => Promise<IdorisStatusResult>

function unavailable(reason: IdorisUnavailableReason): IdorisStatusResult {
  return { state: 'unavailable', reason }
}

export function parseAdminTokenFd(argv: readonly string[]): number | 'missing' | 'invalid' {
  if (argv.some((arg) => arg.startsWith(`${TOKEN_ARG}=`))) return 'invalid'
  const indexes = argv.flatMap((arg, index) => arg === TOKEN_ARG ? [index] : [])
  if (indexes.length === 0) return 'missing'
  if (indexes.length !== 1) return 'invalid'
  const raw = argv[indexes[0] + 1]
  if (!raw || !/^\d+$/.test(raw)) return 'invalid'
  const fd = Number(raw)
  return Number.isSafeInteger(fd) && fd >= 3 && fd <= 0x7fffffff ? fd : 'invalid'
}

function claimedAdminTokenFds(argv: readonly string[]): number[] {
  const fds = new Set<number>()
  argv.forEach((arg, index) => {
    const raw = arg === TOKEN_ARG
      ? argv[index + 1]
      : arg.startsWith(`${TOKEN_ARG}=`) ? arg.slice(TOKEN_ARG.length + 1) : undefined
    if (!raw || !/^\d+$/.test(raw)) return
    const fd = Number(raw)
    if (Number.isSafeInteger(fd) && fd >= 3 && fd <= 0x7fffffff) fds.add(fd)
  })
  return [...fds]
}

async function closeClaimedAdminTokenFds(argv: readonly string[]): Promise<void> {
  await Promise.all(claimedAdminTokenFds(argv).map((fd) => new Promise<void>((resolve) => {
    close(fd, () => resolve())
  })))
}

export function parseAdminPort(raw: string | undefined): number | null {
  if (raw === undefined || raw.trim() === '') return DEFAULT_ADMIN_PORT
  const value = raw.trim()
  if (!/^\d+$/.test(value)) return null
  const port = Number(value)
  return Number.isInteger(port) && port >= 1 && port <= 65_535 ? port : null
}

export function parseTokenFrame(frame: Buffer): string | null {
  if (frame.length !== TOKEN_FRAME_BYTES || frame[64] !== 0x0a) return null
  const token = frame.subarray(0, 64).toString('ascii')
  return /^[0-9a-f]{64}$/.test(token) ? token : null
}

export function readAdminTokenFromFd(fd: number, timeoutMs = TOKEN_READ_TIMEOUT_MS): Promise<string | null> {
  return new Promise((resolve) => {
    const stream = createReadStream('', { fd, autoClose: false })
    const chunks: Buffer[] = []
    let bytes = 0
    let settled = false
    const timer = setTimeout(() => finish(null), timeoutMs)

    const finish = (token: string | null): void => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      stream.destroy()
      close(fd, () => resolve(token))
    }

    stream.on('data', (chunk: string | Buffer) => {
      const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk)
      bytes += buffer.length
      if (bytes > TOKEN_FRAME_BYTES) return finish(null)
      chunks.push(buffer)
    })
    stream.on('end', () => finish(parseTokenFrame(Buffer.concat(chunks))))
    stream.on('error', () => finish(null))
  })
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function exactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value)
  return actual.length === keys.length && actual.every((key) => keys.includes(key))
}

export function parseStatusResponse(value: unknown): IdorisStatusResponse | null {
  if (!isObject(value)) return null
  const keys = ['status', 'service', 'version', 'contract_version', 'instance_id', 'components', 'runtimes', 'subscriptions', 'budget_configured', 'audit_configured', 'capacity'] as const
  if (!exactKeys(value, keys) || value.status !== 'ok' || value.service !== 'idoris') return null
  const textKeys = ['version', 'contract_version', 'instance_id'] as const
  const countKeys = ['components', 'runtimes', 'subscriptions'] as const
  if (textKeys.some((key) => typeof value[key] !== 'string' || (value[key] as string).length === 0)) return null
  if (countKeys.some((key) => !Number.isInteger(value[key]) || (value[key] as number) < 0)) return null
  if (typeof value.budget_configured !== 'boolean' || typeof value.audit_configured !== 'boolean') return null

  const capacity = value.capacity
  if (!isObject(capacity) || typeof capacity.state !== 'string') return null
  if (capacity.state === 'unavailable' || capacity.state === 'error') {
    if (!exactKeys(capacity, ['state'])) return null
  } else if (capacity.state === 'observed') {
    if (!exactKeys(capacity, ['state', 'entries']) || !Array.isArray(capacity.entries)) return null
    for (const entry of capacity.entries) {
      if (!isObject(entry) || !exactKeys(entry, ['id', 'capability', 'resident', 'estimated_memory_gb', 'ctx_limit', 'queue_depth', 'admission_status'])) return null
      if (typeof entry.id !== 'string' || typeof entry.capability !== 'string' || typeof entry.resident !== 'boolean') return null
      if (typeof entry.estimated_memory_gb !== 'number' || !Number.isFinite(entry.estimated_memory_gb) || entry.estimated_memory_gb < 0) return null
      if (!Number.isInteger(entry.ctx_limit) || (entry.ctx_limit as number) < 0 || !Number.isInteger(entry.queue_depth) || (entry.queue_depth as number) < 0) return null
      if (!['ready', 'requires_eviction', 'blocked'].includes(String(entry.admission_status))) return null
    }
  } else return null

  return value as unknown as IdorisStatusResponse
}

export function fetchIdorisStatus(token: string, port: number, timeoutMs = ADMIN_RESPONSE_TIMEOUT_MS, maxBytes = MAX_ADMIN_RESPONSE_BYTES): Promise<IdorisStatusResult> {
  return new Promise((resolve) => {
    let settled = false
    let deadline: NodeJS.Timeout | null = null
    const finish = (result: IdorisStatusResult): void => {
      if (settled) return
      settled = true
      if (deadline) clearTimeout(deadline)
      resolve(result)
    }
    const req = http.request({
      host: ADMIN_HOST,
      port,
      path: STATUS_PATH,
      method: 'GET',
      agent: false,
      headers: { Accept: 'application/json', Authorization: `Bearer ${token}` },
    }, (res) => {
      if (res.statusCode !== 200) {
        res.destroy()
        finish(unavailable('http-error'))
        return
      }
      const chunks: Buffer[] = []
      let bytes = 0
      res.on('data', (chunk: Buffer) => {
        bytes += chunk.length
        if (bytes > maxBytes) {
          res.destroy()
          finish(unavailable('invalid-response'))
        }
        else chunks.push(chunk)
      })
      res.on('end', () => {
        if (bytes > maxBytes) return
        try {
          const status = parseStatusResponse(JSON.parse(Buffer.concat(chunks).toString('utf8')))
          finish(status ? { state: 'available', status } : unavailable('invalid-response'))
        } catch { finish(unavailable('invalid-response')) }
      })
    })
    req.on('error', () => finish(unavailable('unreachable')))
    deadline = setTimeout(() => {
      req.destroy()
      finish(unavailable('unreachable'))
    }, timeoutMs)
    req.end()
  })
}

export async function createIdorisStatusProvider(
  argv: readonly string[] = process.argv,
  env: NodeJS.ProcessEnv = process.env,
): Promise<StatusProvider> {
  const selector = parseAdminTokenFd(argv)
  if (selector === 'missing') return async () => unavailable('handoff-missing')
  if (selector === 'invalid') {
    // These descriptors were explicitly claimed by launcher argv as secret
    // handoff channels. Close every safe candidate before later child spawns.
    await closeClaimedAdminTokenFds(argv)
    return async () => unavailable('handoff-invalid')
  }
  const [token, port] = await Promise.all([
    readAdminTokenFromFd(selector),
    Promise.resolve(parseAdminPort(env[ADMIN_PORT_ENV])),
  ])
  if (!token) return async () => unavailable('handoff-invalid')
  if (port === null) return async () => unavailable('invalid-config')
  return () => fetchIdorisStatus(token, port)
}
