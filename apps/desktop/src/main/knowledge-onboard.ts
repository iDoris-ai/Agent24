// L7-K2 (KB-C4 first round, mock activation): the operator drops
// knowledge-activation.json into userData; main calls the server's init route
// once and saves that one default knowledge base. The activation code and the
// returned api key never reach the renderer, a log line, or an error message.

import { randomBytes } from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { validEndpoint, type KnowledgeController } from './knowledge-settings'
import type { KnowledgeOnboardPreflight, KnowledgeOnboardResult } from '../shared/ipc-types'

export type OnboardPreflight = KnowledgeOnboardPreflight
export type OnboardResult = KnowledgeOnboardResult

export interface Activation {
  endpoint: string
  activation_code: string
}

export interface OnboardDeps {
  dir: string
  controller: () => KnowledgeController | null
  fetch: (url: string, init: RequestInit) => Promise<Response>
  sleep?: (ms: number) => Promise<void>
}

const ACTIVATION_FILE = 'knowledge-activation.json'
const PENDING_FILE = 'knowledge-onboard-pending.json'
const ACTIVATION_MAX_BYTES = 4 * 1024
const MAX_RESPONSE_BYTES = 64 * 1024
const INIT_TIMEOUT_MS = 30_000
const HEALTH_TIMEOUT_MS = 5_000
const RETRY_WAIT_MS = 2_000
const MAX_ATTEMPTS = 5
const FORMATS = ['md', 'markdown', 'txt'] as const
const MAX_UPLOAD_BYTES = 10_485_760
/** Printable ASCII, space included. The api key excludes spaces. */
const ACTIVATION_CODE = /^[ -~]{8,512}$/
const API_KEY = /^[!-~]{1,16384}$/
const IDEMPOTENCY_KEY = /^[A-Za-z0-9_-]{22,128}$/

function isPlain(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function activationPath(dir: string): string {
  return path.join(dir, ACTIVATION_FILE)
}

function pendingPath(dir: string): string {
  return path.join(dir, PENDING_FILE)
}

function joinUrl(endpoint: string, suffix: string): string {
  return `${endpoint.replace(/\/+$/, '')}${suffix}`
}

/** Missing file → null. Anything that is not a private, well-formed activation → 'invalid'. */
export function readActivation(dir: string): Activation | null | 'invalid' {
  const file = activationPath(dir)
  let st: fs.Stats
  try {
    st = fs.lstatSync(file)
  } catch (err) {
    return (err as NodeJS.ErrnoException).code === 'ENOENT' ? null : 'invalid'
  }
  // lstat: a symlink is not a regular file, and its target is not read.
  if (!st.isFile()) return 'invalid'
  if (process.platform !== 'win32' && (st.mode & 0o077) !== 0) return 'invalid'
  if (st.size > ACTIVATION_MAX_BYTES) return 'invalid'
  let parsed: unknown
  try {
    parsed = JSON.parse(fs.readFileSync(file, 'utf8'))
  } catch {
    return 'invalid'
  }
  if (!isPlain(parsed)) return 'invalid'
  const keys = Object.keys(parsed)
  if (keys.length !== 2 || !Object.hasOwn(parsed, 'endpoint') || !Object.hasOwn(parsed, 'activation_code')) return 'invalid'
  const endpoint = parsed.endpoint
  const code = parsed.activation_code
  if (!validEndpoint(endpoint) || typeof code !== 'string' || !ACTIVATION_CODE.test(code)) return 'invalid'
  return { endpoint, activation_code: code }
}

function readPending(file: string): string | null {
  try {
    const st = fs.lstatSync(file)
    if (!st.isFile() || st.size > ACTIVATION_MAX_BYTES) return null
    const parsed: unknown = JSON.parse(fs.readFileSync(file, 'utf8'))
    if (!isPlain(parsed) || Object.keys(parsed).length !== 1) return null
    const key = parsed.idempotency_key
    return typeof key === 'string' && IDEMPOTENCY_KEY.test(key) ? key : null
  } catch {
    return null
  }
}

/** Same shape as the login store: exclusive temp file, mode 0600, then rename. */
async function writePending(file: string, key: string): Promise<void> {
  const tmp = `${file}.${randomBytes(8).toString('hex')}.tmp`
  await fs.promises.mkdir(path.dirname(file), { recursive: true, mode: 0o700 })
  try {
    await fs.promises.writeFile(tmp, JSON.stringify({ idempotency_key: key }), { mode: 0o600, flag: 'wx' })
    await fs.promises.rename(tmp, file)
    await fs.promises.chmod(file, 0o600)
  } catch (err) {
    await fs.promises.rm(tmp, { force: true })
    throw err
  }
}

/** The key from the first attempt, reused until one run succeeds. Unreadable or unwritable → in memory for this run only. */
async function idempotencyKey(dir: string): Promise<string> {
  const file = pendingPath(dir)
  const existing = readPending(file)
  if (existing !== null) return existing
  const key = randomBytes(32).toString('base64url')
  try {
    await writePending(file, key)
    return key
  } catch {
    return readPending(file) ?? key
  }
}

async function readJson(response: Response, signal: AbortSignal): Promise<unknown> {
  const reader = response.body?.getReader()
  if (!reader) throw new Error('no body')
  const chunks: Uint8Array[] = []
  let size = 0
  const onAbort = (): void => { void reader.cancel().catch(() => {}) }
  signal.addEventListener('abort', onAbort)
  try {
    if (signal.aborted) throw new Error('aborted')
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      if (!value) continue
      size += value.byteLength
      if (size > MAX_RESPONSE_BYTES) {
        void reader.cancel().catch(() => {})
        throw new Error('too large')
      }
      chunks.push(value)
    }
  } finally {
    signal.removeEventListener('abort', onAbort)
  }
  return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(Buffer.concat(chunks)))
}

function inProgress(json: unknown): boolean {
  if (!isPlain(json) || !isPlain(json.error)) return false
  const details = json.error.details
  return isPlain(details) && details.reason === 'request_in_progress'
}

function parseSuccess(json: unknown): { knowledgeBaseId: string; outcome: 'created' | 'existing'; apiKey: string; configMismatch: boolean } | null {
  if (!isPlain(json)) return null
  const knowledgeBaseId = json.knowledge_base_id
  if (typeof knowledgeBaseId !== 'string' || knowledgeBaseId.length < 1 || knowledgeBaseId.length > 256) return null
  if (json.outcome !== 'created' && json.outcome !== 'existing') return null
  if (!isPlain(json.credential) || typeof json.credential.api_key !== 'string' || !API_KEY.test(json.credential.api_key)) return null
  if (json.details !== undefined && !isPlain(json.details)) return null
  return {
    knowledgeBaseId,
    outcome: json.outcome,
    apiKey: json.credential.api_key,
    configMismatch: isPlain(json.details) && json.details.config_mismatch === true,
  }
}

type Posted =
  | { kind: 'ok'; knowledgeBaseId: string; outcome: 'created' | 'existing'; apiKey: string; configMismatch: boolean }
  | { kind: 'in_progress' }
  | { kind: 'activation_invalid' }
  | { kind: 'not_offered' }
  | { kind: 'unreachable' }
  | { kind: 'server_error' }

function classify(status: number, json: unknown, parsed: boolean): Posted {
  if (status === 401) return { kind: 'activation_invalid' }
  if (status === 404) return { kind: 'not_offered' }
  // Only this 503 is retried. Overloaded, and any other status, is a server error.
  if (status === 503 && parsed && inProgress(json)) return { kind: 'in_progress' }
  if (status === 200 && parsed) {
    const ok = parseSuccess(json)
    if (ok) return { kind: 'ok', ...ok }
  }
  return { kind: 'server_error' }
}

async function postInit(fetch: OnboardDeps['fetch'], activation: Activation, key: string): Promise<Posted> {
  const timeout = new AbortController()
  const timer = setTimeout(() => timeout.abort(), INIT_TIMEOUT_MS)
  try {
    let response: Response
    try {
      response = await fetch(joinUrl(activation.endpoint, '/api/v1/agent24/knowledge/init'), {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ activation_code: activation.activation_code, idempotency_key: key }),
        redirect: 'error',
        cache: 'no-store',
        signal: timeout.signal,
      })
    } catch {
      return { kind: 'unreachable' }
    }
    let json: unknown
    try {
      json = await readJson(response, timeout.signal)
    } catch {
      if (timeout.signal.aborted) return { kind: 'unreachable' }
      return classify(response.status, undefined, false)
    }
    if (timeout.signal.aborted) return { kind: 'unreachable' }
    return classify(response.status, json, true)
  } finally {
    clearTimeout(timer)
  }
}

async function serverUp(fetch: OnboardDeps['fetch'], endpoint: string): Promise<boolean> {
  const timeout = new AbortController()
  const timer = setTimeout(() => timeout.abort(), HEALTH_TIMEOUT_MS)
  try {
    const response = await fetch(joinUrl(endpoint, '/health'), {
      method: 'GET',
      redirect: 'error',
      cache: 'no-store',
      signal: timeout.signal,
    })
    const ok = response.status >= 200 && response.status < 300
    try {
      void response.body?.cancel().catch(() => {})
    } catch {
      // The status is already known.
    }
    return ok
  } catch {
    return false
  } finally {
    clearTimeout(timer)
  }
}

async function preflight(deps: OnboardDeps): Promise<OnboardPreflight> {
  const activation = readActivation(deps.dir)
  const usable = typeof activation === 'object' && activation !== null ? activation : null
  const endpoint = deps.controller()?.status().endpoint ?? ''
  return {
    activated: usable !== null,
    invalidActivation: activation === 'invalid',
    alreadyBound: endpoint !== '',
    reachable: usable ? await serverUp(deps.fetch, usable.endpoint) : null,
    formats: FORMATS,
    maxBytes: MAX_UPLOAD_BYTES,
  }
}

async function execute(deps: OnboardDeps, input: { localOnly: boolean }): Promise<OnboardResult> {
  // This round only offers the remote service. Do not read the file or the network.
  if (input.localOnly) return { ok: false, code: 'local_only_unavailable' }
  const controller = deps.controller()
  if (!controller) return { ok: false, code: 'unavailable' }
  // Never overwrite a knowledge base the user already configured.
  if (controller.status().endpoint !== '') return { ok: false, code: 'already_bound' }
  const activation = readActivation(deps.dir)
  if (activation === null) return { ok: false, code: 'not_activated' }
  if (activation === 'invalid') return { ok: false, code: 'activation_invalid' }

  const key = await idempotencyKey(deps.dir)
  const sleep = deps.sleep ?? ((ms: number) => new Promise((resolve) => { setTimeout(resolve, ms) }))
  for (let attempt = 1; attempt <= MAX_ATTEMPTS; attempt++) {
    const posted = await postInit(deps.fetch, activation, key)
    if (posted.kind === 'in_progress') {
      if (attempt === MAX_ATTEMPTS) return { ok: false, code: 'in_progress' }
      await sleep(RETRY_WAIT_MS)
      continue
    }
    if (posted.kind !== 'ok') return { ok: false, code: posted.kind }
    const saved = await controller.configure({
      endpoint: activation.endpoint,
      knowledgeBaseIds: [posted.knowledgeBaseId],
      apiKey: posted.apiKey,
    })
    if (!saved.ok) return { ok: false, code: 'not_saved' }
    const enabled = await controller.setEnabled(true)
    if (!enabled.ok) return { ok: false, code: 'not_saved' }
    // Delete only once the binding is saved. A network failure or a failed save keeps the key,
    // because the server may already have created the base and the next run continues it.
    try {
      await fs.promises.rm(pendingPath(deps.dir), { force: true })
    } catch {
      // The binding is saved. A leftover key is unused while the endpoint stays set.
    }
    return { ok: true, outcome: posted.outcome, knowledgeBaseId: posted.knowledgeBaseId, configMismatch: posted.configMismatch }
  }
  return { ok: false, code: 'in_progress' }
}

export function knowledgeOnboarding(deps: OnboardDeps): {
  preflight: () => Promise<OnboardPreflight>
  run: (input: { localOnly: boolean }) => Promise<OnboardResult>
} {
  let inflight: Promise<OnboardResult> | null = null
  return {
    preflight: () => preflight(deps),
    run: (input) => {
      inflight ??= execute(deps, input).finally(() => { inflight = null })
      return inflight
    },
  }
}
