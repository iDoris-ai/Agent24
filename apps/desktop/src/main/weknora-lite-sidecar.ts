// KB-LITE-02 (Loop 9, 3b): WeKnora Lite's managed mode under SidecarManager.
// The manager owns start, readiness, health, stop and process-tree reaping
// (same as Open Design, open-design-sidecar.ts); this file supplies the Lite
// specifics: how `WeKnora-lite --managed` is spawned, the handshake it writes
// back (protocol 2, WeKnora#180), and what "ready" means.
//
// Two extra descriptors go to the child:
//   fd 3  handshake: the child writes one `ready` line (origin, pid, and the
//         instance's API key + default KB id) and then one `models` line.
//   fd 4  lifeline: the child exits when it reads EOF. Main holds the other
//         end for as long as it lives, so a crashed or killed App takes the
//         Lite with it; nothing has to clean up across App runs.
// The API key is reissued on every start and the previous one revoked
// (WeKnora#180), so it is held in main-process memory only: never written,
// logged or sent to a renderer (decision 2026-11-27-lite-host-credential-instance).

import type { ChildProcess, spawn } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import type { Readable } from 'node:stream'

import { PosixProcessGroupTree, type KillFn } from './open-design-sidecar'
import type {
  SidecarChild,
  SidecarHealth,
  SidecarLaunch,
  SidecarLauncher,
  SidecarLogger,
  SidecarReady,
  SidecarSpec,
} from './sidecar-manager'

export const WEKNORA_LITE_HANDSHAKE_PROTOCOL = 2
export const WEKNORA_LITE_SIDECAR_ID = 'weknora-lite'
/** First launch migrates a fresh SQLite and probes the embedding model (up to 15 s). */
export const WEKNORA_LITE_READY_PHASE_TIMEOUT_MS = 60_000
export const WEKNORA_LITE_HEALTH_INTERVAL_MS = 10_000
export const WEKNORA_LITE_HEALTH_TIMEOUT_MS = 3_000
export const WEKNORA_LITE_MAX_HEALTH_FAILURES = 3
export const WEKNORA_LITE_TERM_GRACE_MS = 10_000
export const WEKNORA_LITE_KILL_AFTER_MS = 5_000
/** The embedding key reaches the child by environment, never in the YAML file. */
export const WEKNORA_LITE_EMBED_KEY_ENV = 'A24LITE_EMBED_KEY'
const HANDSHAKE_LINE_LIMIT_BYTES = 16_384
const EMBEDDING_MODEL_ID = 'agent24-embedding'
/** The policy's version: an ingest's `expected_policy_version` must equal it (3c's uploads). */
export const WEKNORA_LITE_POLICY_VERSION = 'agent24-lite-1'

export function weknoraLiteSidecarSpec(readyPhaseTimeoutMs = WEKNORA_LITE_READY_PHASE_TIMEOUT_MS, healthIntervalMs = WEKNORA_LITE_HEALTH_INTERVAL_MS): SidecarSpec {
  return {
    sidecarId: WEKNORA_LITE_SIDECAR_ID,
    readyTimeoutMs: readyPhaseTimeoutMs * 2 + 2_000,
    healthIntervalMs,
    healthTimeoutMs: WEKNORA_LITE_HEALTH_TIMEOUT_MS,
    maxHealthFailures: WEKNORA_LITE_MAX_HEALTH_FAILURES,
    shutdown: { termGraceMs: WEKNORA_LITE_TERM_GRACE_MS, killAfterMs: WEKNORA_LITE_KILL_AFTER_MS },
  }
}

/** The embedding the Lite indexes with: an OpenAI-compatible loopback `/v1` (iDoris gateway). */
export interface WeknoraLiteEmbedding {
  readonly baseUrl: string
  readonly apiKey: string
  readonly model: string
  readonly dimension: number
}

export interface WeknoraLiteHandshake {
  readonly origin: string
  readonly pid: number
  readonly version: string
  readonly apiKey: string
  readonly knowledgeBaseId: string
  readonly expiresAt: string | null
}

const TOKEN = /^[\x21-\x7e]{1,4096}$/

/** Line one of the handshake (`ProcessReady`, WeKnora `internal/agent24managed/handshake.go`). */
export function parseWeknoraLiteHandshake(value: unknown, expectedPid: number): WeknoraLiteHandshake {
  if (value == null || typeof value !== 'object' || Array.isArray(value)) throw new Error('WeKnora Lite handshake must be an object')
  const raw = value as Record<string, unknown>
  if (raw.type !== 'ready' || raw.phase !== 'process' || raw.protocol !== WEKNORA_LITE_HANDSHAKE_PROTOCOL) {
    throw new Error('WeKnora Lite handshake protocol mismatch')
  }
  if (raw.pid !== expectedPid) throw new Error('WeKnora Lite handshake does not match the launched process')
  if (typeof raw.origin !== 'string') throw new Error('WeKnora Lite origin is invalid')
  const url = new URL(raw.origin)
  if (url.protocol !== 'http:' || url.hostname !== '127.0.0.1' || !url.port || url.username || url.password
    || url.pathname !== '/' || url.search || url.hash) {
    throw new Error('WeKnora Lite origin must be an exact 127.0.0.1 HTTP origin')
  }
  const credential = raw.credential as Record<string, unknown> | null | undefined
  if (credential == null || typeof credential !== 'object' || Array.isArray(credential)) throw new Error('WeKnora Lite credential is missing')
  const { api_key: apiKey, knowledge_base_id: kb, expires_at: expiresAt } = credential
  // Never echo the key: the message names the field only.
  if (typeof apiKey !== 'string' || !TOKEN.test(apiKey)) throw new Error('WeKnora Lite credential api_key is invalid')
  if (typeof kb !== 'string' || !TOKEN.test(kb)) throw new Error('WeKnora Lite credential knowledge_base_id is invalid')
  if (expiresAt !== null && expiresAt !== undefined && typeof expiresAt !== 'string') throw new Error('WeKnora Lite credential expires_at is invalid')
  return {
    origin: url.origin,
    pid: expectedPid,
    version: typeof raw.version === 'string' ? raw.version.slice(0, 64) : '',
    apiKey,
    knowledgeBaseId: kb,
    expiresAt: expiresAt ?? null,
  }
}

/** `builtin_models.yaml` for BUILTIN_MODELS_CONFIG. The key stays a `${VAR}` placeholder. */
export function weknoraLiteModelsYaml(embedding: WeknoraLiteEmbedding): string {
  const quote = (s: string) => JSON.stringify(s)
  return [
    'builtin_models:',
    `  - id: ${EMBEDDING_MODEL_ID}`,
    '    type: Embedding',
    '    source: remote',
    '    is_default: true',
    `    name: ${quote(embedding.model)}`,
    '    parameters:',
    `      base_url: ${quote(embedding.baseUrl)}`,
    `      api_key: "\${${WEKNORA_LITE_EMBED_KEY_ENV}}"`,
    '      provider: generic',
    '      embedding_parameters:',
    `        dimension: ${embedding.dimension}`,
    '        truncate_prompt_tokens: 0',
    '',
  ].join('\n')
}

/** AGENT24_KNOWLEDGE_POLICY: embeddings may go to the given gateway only (the default denies everything). */
export function weknoraLitePolicy(embedding: WeknoraLiteEmbedding): string {
  const url = new URL(embedding.baseUrl)
  return JSON.stringify({
    version: WEKNORA_LITE_POLICY_VERSION,
    destinations: [{ category: 'embedding', endpoint: url.origin, model: embedding.model }],
    budget: { max_external_calls: 64, max_input_bytes: 1_048_576, max_inflight_external: 4 },
  })
}

/** The handshake's first line. A JSON syntax error is reported with a fixed
 * message: V8 quotes the text around the error position, which can be the key
 * (PR-Daemon #2108 G5); the message reaches `status.error` and the log. */
export function parseWeknoraLiteHandshakeLine(line: string, expectedPid: number): WeknoraLiteHandshake {
  let value: unknown
  try {
    value = JSON.parse(line)
  } catch {
    throw new Error('WeKnora Lite handshake is not valid JSON')
  }
  return parseWeknoraLiteHandshake(value, expectedPid)
}

export function validateWeknoraLiteEmbedding(embedding: WeknoraLiteEmbedding): void {
  const url = new URL(embedding.baseUrl)
  if (url.protocol !== 'http:' || !['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname) || url.username || url.password) {
    throw new Error('WeKnora Lite embedding must be a loopback HTTP endpoint')
  }
  if (!TOKEN.test(embedding.apiKey)) throw new Error('WeKnora Lite embedding key is invalid')
  if (!/^[A-Za-z0-9._:/-]{1,128}$/.test(embedding.model)) throw new Error('WeKnora Lite embedding model is invalid')
  if (!Number.isInteger(embedding.dimension) || embedding.dimension < 1 || embedding.dimension > 8192) {
    throw new Error('WeKnora Lite embedding dimension is invalid')
  }
}

const PASSED_ENV = ['HOME', 'PATH', 'TMPDIR', 'USER', 'LOGNAME', 'LANG', 'LC_ALL', 'TZ']

/** Only what the Lite needs: no A24_*, no DB_* or JWT_* from the App's own environment. */
export function weknoraLiteChildEnvironment(base: NodeJS.ProcessEnv, plan: WeknoraLiteLaunchPlan): NodeJS.ProcessEnv {
  const env = Object.create(null) as NodeJS.ProcessEnv
  for (const key of PASSED_ENV) if (base[key] !== undefined) env[key] = base[key]
  env.BUILTIN_MODELS_CONFIG = plan.modelsConfigPath
  env[WEKNORA_LITE_EMBED_KEY_ENV] = plan.embedding.apiKey
  env.SSRF_WHITELIST = '127.0.0.1'
  env.AGENT24_KNOWLEDGE_POLICY = weknoraLitePolicy(plan.embedding)
  return env
}

export interface WeknoraLiteLaunchPlan {
  /** The verified WeKnora-lite program. */
  readonly binary: string
  /** Its install dir: config/, migrations/, dict/ are found relative to it. */
  readonly cwd: string
  /** ~/.agent24/data/weknora-lite (0700): state dir, SQLite and files. */
  readonly stateDir: string
  readonly modelsConfigPath: string
  /** Lite's stdout/stderr (no keys: WeKnora#180 keeps them out of its logs). */
  readonly logPath: string
  readonly embedding: WeknoraLiteEmbedding
  readonly environment: NodeJS.ProcessEnv
  readonly readyPhaseTimeoutMs: number
}

type FetchFn = typeof fetch
type SpawnFn = typeof spawn
type LaunchedChild = ChildProcess & { stdio: [null, null, null, Readable, unknown] }

/** `POST /api/v1/agent24/knowledge/ready` with the instance key (integration route, X-API-Key). */
export async function weknoraLiteReady(fetchFn: FetchFn, origin: string, apiKey: string, knowledgeBaseId: string, signal: AbortSignal): Promise<number> {
  const response = await fetchFn(`${origin}/api/v1/agent24/knowledge/ready`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'X-API-Key': apiKey },
    body: JSON.stringify({ protocol_version: 1, request_id: 'agent24-lite-ready', generation: 1, scope: { knowledge_base_ids: [knowledgeBaseId] } }),
    signal,
  })
  void response.body?.cancel().catch(() => {})
  return response.status
}

/** The Lite's health is its ready route. Needs the current KB id, which only the handshake gives. */
export class WeknoraLiteHealth implements SidecarHealth {
  constructor(private readonly knowledgeBaseId: () => string | null, private readonly fetchFn: FetchFn = fetch) {}

  async check(endpoint: SidecarReady['endpoint'], timeoutMs: number): Promise<boolean> {
    const kb = this.knowledgeBaseId()
    if (!kb || !endpoint.secret) return false
    const controller = new AbortController()
    const timer = setTimeout(() => controller.abort(), timeoutMs)
    timer.unref?.()
    try {
      return (await weknoraLiteReady(this.fetchFn, endpoint.origin, endpoint.secret, kb, controller.signal)) === 200
    } catch {
      return false
    } finally {
      clearTimeout(timer)
    }
  }
}

export class WeknoraLiteLauncher implements SidecarLauncher {
  constructor(
    private readonly plan: () => WeknoraLiteLaunchPlan,
    /** Receives each launch's handshake before its readiness resolves. */
    private readonly onHandshake: (handshake: WeknoraLiteHandshake) => void,
    private readonly spawnFn: SpawnFn,
    private readonly fetchFn: FetchFn,
    private readonly logger: SidecarLogger,
    private readonly platform: NodeJS.Platform = process.platform,
    private readonly kill: KillFn = (target, signal) => process.kill(target, signal),
  ) {}

  launch(): SidecarLaunch {
    if (this.platform === 'win32') throw new Error('unsupported platform')
    const plan = this.plan()
    const log = fs.openSync(plan.logPath, 'a', 0o600)
    let child: LaunchedChild
    try {
      child = this.spawnFn(
        plan.binary,
        ['--managed', '--state-dir', plan.stateDir, '--port', '0', '--handshake-fd', '3', '--lifeline-fd', '4'],
        // fd 3/4 are socket pairs: the child writes the handshake to 3 and reads 4 until EOF.
        // detached: the Lite leads its own process group, so stop never signals Agent24's group.
        { cwd: plan.cwd, env: weknoraLiteChildEnvironment(plan.environment, plan), stdio: ['ignore', log, log, 'pipe', 'pipe'], detached: true },
      ) as LaunchedChild
    } finally {
      fs.closeSync(log)
    }
    // The lifeline: held, never written, closed only when this process goes away or the child exits.
    const lifeline = child.stdio[4] as { on?: (event: string, fn: () => void) => void } | undefined
    lifeline?.on?.('error', () => {})
    const adapter = new ChildAdapter(child)
    const cancellation = new AbortController()
    adapter.onExit((code) => {
      cancellation.abort()
      this.logger.warn('weknora-lite.exited', { code: code ?? -1 })
    })
    const pid = child.pid
    const hasPid = typeof pid === 'number' && Number.isInteger(pid) && pid > 1
    const group = hasPid ? new PosixProcessGroupTree(pid, this.kill) : null
    const ready = hasPid
      ? this.waitForReady(child, pid, plan, cancellation.signal)
      : Promise.reject(new Error('WeKnora Lite did not start'))
    return {
      child: adapter,
      processGroupId: hasPid ? pid : undefined,
      dedicatedProcessGroup: hasPid,
      tree: group
        ? { signal: async (force) => { cancellation.abort(); await group.signal(force) }, isEmpty: () => group.isEmpty() }
        : { signal: async () => {}, isEmpty: async () => true },
      ready,
    }
  }

  private async waitForReady(child: LaunchedChild, pid: number, plan: WeknoraLiteLaunchPlan, signal: AbortSignal): Promise<SidecarReady> {
    const handshake = await this.readHandshake(child, pid, plan.readyPhaseTimeoutMs)
    await this.pollReady(handshake, plan.readyPhaseTimeoutMs, signal)
    this.onHandshake(handshake)
    return { endpoint: { origin: handshake.origin, auth: 'secret', secret: handshake.apiKey } }
  }

  private readHandshake(child: LaunchedChild, pid: number, timeoutMs: number): Promise<WeknoraLiteHandshake> {
    const stream = child.stdio[3]
    return new Promise((resolve, reject) => {
      let buffered = ''
      let first: WeknoraLiteHandshake | null = null
      let settled = false
      const timer = setTimeout(() => done(new Error('WeKnora Lite handshake timed out')), timeoutMs)
      timer.unref?.()
      const done = (value: Error | WeknoraLiteHandshake): void => {
        if (settled) return
        settled = true
        clearTimeout(timer)
        child.off('exit', onExit)
        if (value instanceof Error) {
          stream.off('data', onData)
          reject(value)
        } else resolve(value)
      }
      const onLine = (line: string): void => {
        if (!first) {
          try { first = parseWeknoraLiteHandshakeLine(line, pid) } catch (error) {
            done(error instanceof Error ? error : new Error(String(error)))
            return
          }
          done(first)
          return
        }
        // Line two reports the models; only the embedding state is logged.
        try {
          const state = (JSON.parse(line) as { embedding?: { state?: unknown } }).embedding?.state
          this.logger.info('weknora-lite.models', { embedding: typeof state === 'string' ? state.slice(0, 32) : 'unknown' })
        } catch { /* not ours to fail on */ }
        stream.off('data', onData)
      }
      const onData = (chunk: Buffer | string): void => {
        buffered += chunk.toString()
        if (Buffer.byteLength(buffered, 'utf8') > HANDSHAKE_LINE_LIMIT_BYTES) {
          done(new Error('WeKnora Lite handshake exceeded the host limit'))
          return
        }
        for (let newline = buffered.indexOf('\n'); newline >= 0; newline = buffered.indexOf('\n')) {
          const line = buffered.slice(0, newline).trim()
          buffered = buffered.slice(newline + 1)
          if (line) onLine(line)
        }
      }
      const onExit = (code: number | null): void => done(new Error(`WeKnora Lite exited before its handshake (${code ?? 'signal'})`))
      stream.on('data', onData)
      stream.on('error', () => {})
      child.once('exit', onExit)
    })
  }

  private async pollReady(handshake: WeknoraLiteHandshake, timeoutMs: number, signal: AbortSignal): Promise<void> {
    const deadline = Date.now() + timeoutMs
    let last = 'not ready'
    while (Date.now() < deadline) {
      if (signal.aborted) throw new Error('WeKnora Lite exited before ready or was cancelled')
      const controller = new AbortController()
      const timer = setTimeout(() => controller.abort(), Math.min(3_000, Math.max(1, deadline - Date.now())))
      const cancel = () => controller.abort()
      signal.addEventListener('abort', cancel, { once: true })
      try {
        const status = await weknoraLiteReady(this.fetchFn, handshake.origin, handshake.apiKey, handshake.knowledgeBaseId, controller.signal)
        if (status === 200) return
        last = `HTTP ${status}`
      } catch (error) {
        last = error instanceof Error ? error.message : String(error)
      } finally {
        clearTimeout(timer)
        signal.removeEventListener('abort', cancel)
      }
      await new Promise((resolve) => setTimeout(resolve, 100))
    }
    throw new Error(`WeKnora Lite ready timed out: ${last}`)
  }
}

/** Presents a `ChildProcess` as the manager's `SidecarChild`; also gives `error` a listener. */
class ChildAdapter implements SidecarChild {
  constructor(private readonly child: ChildProcess) {
    child.on('error', () => {})
  }

  get pid(): number | undefined { return this.child.pid }
  get exitCode(): number | null { return this.child.exitCode }
  get signalCode(): NodeJS.Signals | null { return this.child.signalCode }

  onExit(listener: (code: number | null) => void): () => void {
    const handler = (code: number | null): void => listener(code)
    this.child.once('exit', handler)
    return () => { this.child.off('exit', handler) }
  }
}

/** ~/.agent24/data/weknora-lite, created 0700 (and tightened to 0700 if it exists). */
export function ensureWeknoraLiteDataDir(dir: string): void {
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 })
  fs.chmodSync(dir, 0o700)
}

export function writeWeknoraLiteModelsConfig(stateDir: string, embedding: WeknoraLiteEmbedding): string {
  const file = path.join(stateDir, 'agent24-builtin-models.yaml')
  fs.writeFileSync(file, weknoraLiteModelsYaml(embedding), { encoding: 'utf8', mode: 0o600 })
  fs.chmodSync(file, 0o600)
  return file
}
