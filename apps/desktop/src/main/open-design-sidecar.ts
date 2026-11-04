import { execFileSync, type ChildProcess, type spawn } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import type { Readable } from 'node:stream'

import type {
  SidecarChild,
  SidecarHealth,
  SidecarLaunch,
  SidecarLauncher,
  SidecarLogger,
  SidecarReady,
  SidecarSpec,
  SidecarTreeController,
} from './sidecar-manager'

/**
 * Open Design under `SidecarManager` (ADR-038 C8). The manager owns start, readiness,
 * health, stop and process-tree reaping; this file supplies the Open Design specifics:
 * how the headless launcher is spawned, what "ready" and "healthy" mean, and how the
 * launcher's POSIX process group is signalled.
 */

// Headless protocol v2. The pinned fork (packaged 0.26.1) accepts an explicit
// resourceSafeBase; the ready payload's pinVersion must match the host's pin.
export const AGENT24_HEADLESS_PROTOCOL = 2

/** Each readiness phase (ready line, /api/ready); the total budget is two phases plus 2 s. */
export const OPEN_DESIGN_READY_PHASE_TIMEOUT_MS = 14_000
export const OPEN_DESIGN_HEALTH_INTERVAL_MS = 10_000
export const OPEN_DESIGN_HEALTH_TIMEOUT_MS = 3_000
export const OPEN_DESIGN_MAX_HEALTH_FAILURES = 3
/** ADR-038 D5 default grace; only the launcher is asked to exit, so OD retires web, then daemon. */
export const OPEN_DESIGN_TERM_GRACE_MS = 15_000
export const OPEN_DESIGN_KILL_AFTER_MS = 5_000

export interface OpenDesignSidecarTimings {
  /** Per readiness phase. The manager's total is `2 * phase + 2 s`, so OD's own error text wins the race. */
  readyPhaseTimeoutMs?: number
  healthIntervalMs?: number
  healthTimeoutMs?: number
  termGraceMs?: number
  killAfterMs?: number
}

export function openDesignSidecarSpec(timings: OpenDesignSidecarTimings = {}): SidecarSpec {
  const phase = timings.readyPhaseTimeoutMs ?? OPEN_DESIGN_READY_PHASE_TIMEOUT_MS
  return {
    sidecarId: 'open-design',
    readyTimeoutMs: phase * 2 + 2_000,
    healthIntervalMs: timings.healthIntervalMs ?? OPEN_DESIGN_HEALTH_INTERVAL_MS,
    healthTimeoutMs: timings.healthTimeoutMs ?? OPEN_DESIGN_HEALTH_TIMEOUT_MS,
    maxHealthFailures: OPEN_DESIGN_MAX_HEALTH_FAILURES,
    shutdown: {
      termGraceMs: timings.termGraceMs ?? OPEN_DESIGN_TERM_GRACE_MS,
      killAfterMs: timings.killAfterMs ?? OPEN_DESIGN_KILL_AFTER_MS,
    },
  }
}

/**
 * The executable Open Design's Node children (headless launcher, daemon, web
 * sidecar) run under ELECTRON_RUN_AS_NODE. On macOS this is the bundle's
 * `<Name> Helper` (LSUIElement): running the main `Contents/MacOS/<Name>`
 * directly registers each child as a foreground app, so `next-server` showed up
 * in the Dock as a generic "exec" tile. Falls back to the main executable.
 */
export function resolveNodeRuntimeExecutable(
  execPath: string = process.execPath,
  platform: NodeJS.Platform = process.platform,
): string {
  if (platform !== 'darwin') return execPath
  const name = path.basename(execPath)
  const contents = path.dirname(path.dirname(execPath))
  const helper = path.join(contents, 'Frameworks', `${name} Helper.app`, 'Contents', 'MacOS', `${name} Helper`)
  return fs.existsSync(helper) ? helper : execPath
}

/** `Contents/Resources/backend/agent24` inside the running Agent24.app, if present. */
export function resolveBundledAgent24Cli(
  resources: string | undefined = (process as NodeJS.Process & { resourcesPath?: string }).resourcesPath,
): string | null {
  if (!resources) return null
  const cli = path.join(resources, 'backend', 'agent24')
  return fs.existsSync(cli) ? cli : null
}

export function creativeChildEnvironment(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  // A24_* belongs to the Agent24 host authority/bootstrap namespace. The
  // desktop may consume A24_OPEN_DESIGN_* to choose how to launch Creative,
  // but the less-trusted Open Design child must never inherit host secrets.
  // Keep the rest of the environment intact so Open Design's own provider,
  // media, proxy, locale, and toolchain configuration continues to work.
  const childEnv = Object.create(null) as NodeJS.ProcessEnv
  for (const [key, value] of Object.entries(env)) {
    if (!key.toUpperCase().startsWith('A24_')) childEnv[key] = value
  }
  return childEnv
}

function canonicalHeadlessOrigin(raw: unknown, field: string): string {
  if (typeof raw !== 'string') throw new Error(`Open Design ${field} is invalid`)
  const url = new URL(raw)
  if (url.protocol !== 'http:' || url.hostname !== '127.0.0.1' || !url.port) {
    throw new Error(`Open Design ${field} must use an exact 127.0.0.1 HTTP origin`)
  }
  if (url.username || url.password || url.pathname !== '/' || url.search || url.hash) {
    throw new Error(`Open Design ${field} must be an origin without credentials or path data`)
  }
  return url.origin
}

export interface Agent24HeadlessReady {
  type: 'ready'
  protocol: 2
  instanceId: string
  pinVersion: string
  webOrigin: string
  daemonOrigin: string
  ownership: { ownerPid: number; kind: 'process-tree' }
}

export function parseAgent24HeadlessReady(
  value: unknown,
  expectedPinVersion: string,
  expectedOwnerPid: number,
): Agent24HeadlessReady {
  if (value == null || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('Open Design headless readiness must be an object')
  }
  const raw = value as Record<string, unknown>
  const allowed = new Set(['type', 'protocol', 'instanceId', 'pinVersion', 'webOrigin', 'daemonOrigin', 'ownership'])
  const unknown = Object.keys(raw).find((key) => !allowed.has(key))
  if (unknown) throw new Error(`Open Design headless readiness contains unsupported field: ${unknown}`)
  if (raw.type !== 'ready' || raw.protocol !== AGENT24_HEADLESS_PROTOCOL) {
    throw new Error('Open Design headless readiness protocol mismatch')
  }
  if (raw.pinVersion !== expectedPinVersion) throw new Error('Open Design headless pin version mismatch')
  if (typeof raw.instanceId !== 'string' || !raw.instanceId.trim() || /\s/u.test(raw.instanceId)) {
    throw new Error('Open Design headless instance id is invalid')
  }
  if (raw.ownership == null || typeof raw.ownership !== 'object' || Array.isArray(raw.ownership)) {
    throw new Error('Open Design headless ownership is invalid')
  }
  const ownership = raw.ownership as Record<string, unknown>
  const unknownOwnership = Object.keys(ownership).find((key) => key !== 'ownerPid' && key !== 'kind')
  if (unknownOwnership) {
    throw new Error(`Open Design headless ownership contains unsupported field: ${unknownOwnership}`)
  }
  if (ownership.kind !== 'process-tree' || ownership.ownerPid !== expectedOwnerPid) {
    throw new Error('Open Design headless ownership does not match the launched process')
  }
  return {
    type: 'ready',
    protocol: AGENT24_HEADLESS_PROTOCOL,
    instanceId: raw.instanceId,
    pinVersion: expectedPinVersion,
    webOrigin: canonicalHeadlessOrigin(raw.webOrigin, 'web origin'),
    daemonOrigin: canonicalHeadlessOrigin(raw.daemonOrigin, 'daemon origin'),
    ownership: { ownerPid: expectedOwnerPid, kind: 'process-tree' },
  }
}

// ---------------------------------------------------------------------------
// Process-group tree control (POSIX)
// ---------------------------------------------------------------------------

export type KillFn = (pid: number, signal: NodeJS.Signals | 0) => unknown

const errnoCode = (error: unknown): string | undefined => (error as NodeJS.ErrnoException | null)?.code

/**
 * Signals the launcher's own process group. The launcher is spawned `detached`, so its
 * pgid equals its pid and the group holds the launcher plus the daemon and web sidecars
 * it starts (they do not detach on POSIX).
 *
 * - graceful: SIGTERM to the launcher pid only. A group-wide TERM would hit web and daemon
 *   at once and defeat the launcher's ordered shutdown (web first, then daemon).
 * - force: SIGKILL to `-pgid`. Only safe for a group this host created, which is why the
 *   controller is bound to one launcher and retires itself.
 * - empty: `kill(-pgid, 0)` reports ESRCH. EPERM means some member exists, so not empty.
 *
 * After the group has been observed empty the controller is retired and sends nothing
 * more: the kernel may hand the same pgid to an unrelated group later.
 */
export class PosixProcessGroupTree implements SidecarTreeController {
  private retired = false

  constructor(
    private readonly pid: number,
    private readonly kill: KillFn = (target, signal) => process.kill(target, signal),
  ) {
    if (!Number.isInteger(pid) || pid < 2) throw new Error('process group id is invalid')
  }

  async signal(force: boolean): Promise<void> {
    if (this.retired) return
    if (!force) {
      if (await this.isEmpty()) return
      try { this.kill(this.pid, 'SIGTERM') } catch (error) {
        if (errnoCode(error) !== 'ESRCH') throw error
      }
      return
    }
    // Do not SIGKILL a group that is already gone: its id could belong to someone else now.
    if (await this.isEmpty()) return
    try { this.kill(-this.pid, 'SIGKILL') } catch (error) {
      if (errnoCode(error) !== 'ESRCH') throw error
      this.retired = true
    }
  }

  async isEmpty(): Promise<boolean> {
    if (this.retired) return true
    try {
      this.kill(-this.pid, 0)
      return false
    } catch (error) {
      const code = errnoCode(error)
      if (code === 'ESRCH') {
        this.retired = true
        return true
      }
      if (code === 'EPERM') return false
      throw error
    }
  }
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

type FetchFn = typeof fetch

/** `GET /api/ready` through the web origin. The web sidecar proxies to the daemon, which answers 503 while closing. */
export class OpenDesignHealth implements SidecarHealth {
  constructor(private readonly fetchFn: FetchFn = fetch) {}

  async check(endpoint: SidecarReady['endpoint'], timeoutMs: number): Promise<boolean> {
    const controller = new AbortController()
    const timer = setTimeout(() => controller.abort(), timeoutMs)
    timer.unref?.()
    try {
      const response = await this.fetchFn(`${endpoint.origin}/api/ready`, { signal: controller.signal })
      void response.body?.cancel().catch(() => {})
      return response.ok
    } catch {
      return false
    } finally {
      clearTimeout(timer)
    }
  }
}

// ---------------------------------------------------------------------------
// Launcher
// ---------------------------------------------------------------------------

export interface OpenDesignLaunchPlan {
  readonly headlessEntry: string
  /** Becomes the launcher's cwd. */
  readonly resourcesPath: string
  readonly runtimeExecutable: string
  readonly configPath: string
  readonly config: Record<string, unknown>
  readonly dataRoot: string
  readonly runtimeRoot: string
  readonly pinVersion: string
  readonly environment: NodeJS.ProcessEnv
  /** The CLI Open Design's `agent24` runtime runs, as AGENT24_BIN. Null/undefined: not set. */
  readonly agent24Cli?: string | null
  readonly readyPhaseTimeoutMs: number
}

type SpawnFn = typeof spawn
type LaunchedChild = ChildProcess & { stdout: Readable; stderr: Readable }

const INERT_TREE: SidecarTreeController = { signal: async () => {}, isEmpty: async () => true }
const STDERR_TAIL_BYTES = 2_000
const READY_LINE_LIMIT_BYTES = 16_384

/**
 * Reports, never signals, a launcher left over from an earlier app run.
 * A pid file plus a `ps` snapshot cannot prove the group is still ours by the time a
 * signal lands (the id can be reused in between), so cross-run cleanup is left to a
 * lifetime-bound supervisor (sidecar-host). This only logs what it finds.
 */
export function reapStaleLauncher(
  pidFile: string,
  plan: OpenDesignLaunchPlan,
  kill: KillFn,
  ps: (pid: number) => string,
  logger: SidecarLogger = { info: () => {}, warn: () => {} },
): void {
  try {
    const record = fs.readFileSync(pidFile, 'utf8')
    if (!/^[1-9][0-9]*$/.test(record)) return
    const pid = Number(record)
    if (!Number.isSafeInteger(pid) || pid < 2 || pid === process.pid) return
    kill(-pid, 0)
    const expected = `${pid} ${plan.runtimeExecutable} ${plan.headlessEntry} --config ${plan.configPath}`
    try { kill(pid, 0) } catch (error) {
      if (errnoCode(error) === 'ESRCH') logger.warn('open-design.stale-group-unverified', { pid })
      return
    }
    if (ps(pid).trim().replace(/^([0-9]+)\s+/, '$1 ') !== expected) return
    logger.warn('open-design.stale-launcher-running', { pid })
  } catch { /* Missing, dead, inaccessible or unverifiable: nothing to report. */ }
}

export class OpenDesignLauncher implements SidecarLauncher {
  constructor(
    private readonly plan: () => OpenDesignLaunchPlan,
    private readonly spawnFn: SpawnFn,
    private readonly fetchFn: FetchFn,
    private readonly logger: SidecarLogger,
    private readonly platform: NodeJS.Platform = process.platform,
    private readonly kill: KillFn = (target, signal) => process.kill(target, signal),
    private readonly ps: (pid: number) => string = (pid) => execFileSync('ps', ['-p', String(pid), '-o', 'pgid=,command='], { encoding: 'utf8', timeout: 1000 }),
    private readonly files: typeof fs = fs,
  ) {}

  launch(): SidecarLaunch {
    // Job Objects are not wired yet (sidecar-host); without a verifiable tree there is no safe stop.
    if (this.platform === 'win32') throw new Error('unsupported platform')
    const plan = this.plan()
    const fs = this.files

    fs.mkdirSync(plan.dataRoot, { recursive: true })
    fs.mkdirSync(plan.runtimeRoot, { recursive: true })
    const pidFile = path.join(plan.runtimeRoot, 'open-design.pgid')
    reapStaleLauncher(pidFile, plan, this.kill, this.ps, this.logger)
    fs.writeFileSync(plan.configPath, `${JSON.stringify(plan.config)}\n`, { encoding: 'utf8', mode: 0o600 })
    fs.chmodSync(plan.configPath, 0o600)

    const childEnv = creativeChildEnvironment(plan.environment)
    childEnv.ELECTRON_RUN_AS_NODE = '1'
    // Open Design looks `agent24` up on PATH unless AGENT24_BIN says otherwise; a
    // machine without the CLI installed would otherwise have no Agent24 runtime.
    if (plan.agent24Cli && !childEnv.AGENT24_BIN) childEnv.AGENT24_BIN = plan.agent24Cli

    const child = this.spawnFn(
      plan.runtimeExecutable,
      [plan.headlessEntry, '--config', plan.configPath],
      // detached: the launcher leads its own process group, so the whole tree can be reaped
      // without ever signalling Agent24's own group.
      { cwd: plan.resourcesPath, env: childEnv, stdio: ['ignore', 'pipe', 'pipe'], detached: true },
    ) as LaunchedChild

    const pid = child.pid
    if (pid && pid >= 2) {
      try { fs.writeFileSync(pidFile, String(pid), { mode: 0o600 }) } catch (error) {
        try { this.kill(-pid, 'SIGKILL') } catch (killError) {
          this.logger.warn('open-design.startup-kill-failed', { error: String(killError) })
        }
        throw new Error(`Open Design startup failed: cannot record process group: ${String(error)}`)
      }
    }
    const cancellation = new AbortController()
    const adapter = new ChildAdapter(child)
    const tail = { value: '' }
    child.stderr.on('data', (chunk: Buffer | string) => {
      tail.value = (tail.value + chunk.toString()).slice(-STDERR_TAIL_BYTES)
    })
    adapter.onExit((code) => {
      cancellation.abort()
      try {
        if (fs.readFileSync(pidFile, 'utf8') === String(pid)) fs.unlinkSync(pidFile)
      } catch { /* Missing record is already retired. */ }
      this.logger.warn('open-design.exited', { code: code ?? -1, stderr: tail.value.trim() })
    })

    const ready = this.waitForReady(child, adapter, plan, tail, cancellation.signal)
    const hasPid = typeof pid === 'number' && Number.isInteger(pid) && pid > 1
    const group = hasPid ? new PosixProcessGroupTree(pid, this.kill) : INERT_TREE
    return {
      child: adapter,
      processGroupId: hasPid ? pid : undefined,
      dedicatedProcessGroup: hasPid,
      // Without a pid nothing was spawned, so there is nothing to signal; the manager rejects the launch.
      tree: hasPid ? {
        signal: async (force) => { cancellation.abort(); await group.signal(force) },
        isEmpty: () => group.isEmpty(),
      } : INERT_TREE,
      ready,
    }
  }

  private async waitForReady(
    child: LaunchedChild,
    adapter: ChildAdapter,
    plan: OpenDesignLaunchPlan,
    tail: { value: string },
    signal: AbortSignal,
  ): Promise<SidecarReady> {
    if (!Number.isInteger(child.pid) || !child.pid || child.pid < 1) {
      throw new Error('Open Design headless launcher did not provide a pid')
    }
    const ready = await this.readReadyLine(child, plan, tail)
    await this.pollApiReady(ready.webOrigin, plan.readyPhaseTimeoutMs, adapter, signal)
    // Open Design's loopback port does not authenticate (see ADR-038 C8 notes): say so, do not fake a secret.
    return { endpoint: { origin: ready.webOrigin, auth: 'none' } }
  }

  private readReadyLine(
    child: LaunchedChild,
    plan: OpenDesignLaunchPlan,
    tail: { value: string },
  ): Promise<Agent24HeadlessReady> {
    return new Promise((resolve, reject) => {
      let stdout = ''
      let settled = false
      const timeout = setTimeout(() => done(new Error('Open Design headless readiness output timed out')), plan.readyPhaseTimeoutMs)
      timeout.unref?.()
      const done = (value: Error | Agent24HeadlessReady): void => {
        if (settled) return
        settled = true
        clearTimeout(timeout)
        child.stdout.off('data', onStdout)
        child.off('error', onError)
        child.off('exit', onExit)
        // Keep draining: an unread stdout pipe would eventually block the launcher.
        child.stdout.on('data', () => {})
        value instanceof Error ? reject(value) : resolve(value)
      }
      const inspect = (): void => {
        if (Buffer.byteLength(stdout, 'utf8') > READY_LINE_LIMIT_BYTES) {
          done(new Error('Open Design headless readiness output exceeded the host limit'))
          return
        }
        const newline = stdout.indexOf('\n')
        if (newline < 0) return
        const line = stdout.slice(0, newline).trim()
        if (!line) {
          done(new Error('Open Design headless readiness line is empty'))
          return
        }
        try {
          done(parseAgent24HeadlessReady(JSON.parse(line), plan.pinVersion, child.pid as number))
        } catch (error) {
          done(error instanceof Error ? error : new Error(String(error)))
        }
      }
      const onStdout = (chunk: Buffer | string): void => { stdout += chunk.toString(); inspect() }
      const onError = (error: Error): void => done(error)
      const onExit = (code: number | null): void =>
        done(new Error(`Open Design headless launcher exited before ready (${code ?? 'signal'})${tail.value ? `: ${tail.value.trim()}` : ''}`))
      child.stdout.on('data', onStdout)
      child.once('error', onError)
      child.once('exit', onExit)
    })
  }

  private async pollApiReady(origin: string, timeoutMs: number, adapter: ChildAdapter, signal: AbortSignal): Promise<void> {
    const controller = new AbortController()
    let lastError = 'not ready'
    const cancel = () => controller.abort(new Error('Open Design headless launcher exited before ready or cancelled'))
    signal.addEventListener('abort', cancel, { once: true })
    if (signal.aborted || adapter.exited) cancel()
    const timer = setTimeout(() => controller.abort(new Error(`Open Design /api/ready timed out: ${lastError}`)), timeoutMs)
    let removeAbort = () => {}
    const aborted = new Promise<never>((_resolve, reject) => {
      const fail = () => reject(controller.signal.reason)
      controller.signal.addEventListener('abort', fail, { once: true })
      removeAbort = () => controller.signal.removeEventListener('abort', fail)
      if (controller.signal.aborted) fail()
    })
    try {
      for (;;) {
        try {
          const response = await Promise.race([this.fetchFn(`${origin}/api/ready`, { signal: controller.signal }), aborted])
          void response.body?.cancel().catch(() => {})
          if (response.ok) return
          lastError = `HTTP ${response.status}`
        } catch (error) {
          if (controller.signal.aborted) throw controller.signal.reason
          lastError = error instanceof Error ? error.message : String(error)
        }
        let delay: ReturnType<typeof setTimeout> | undefined
        try {
          await Promise.race([new Promise<void>((resolve) => { delay = setTimeout(resolve, 100) }), aborted])
        } finally { clearTimeout(delay) }
      }
    } finally {
      clearTimeout(timer)
      removeAbort()
      signal.removeEventListener('abort', cancel)
    }
  }

}

/** Presents a `ChildProcess` as the manager's `SidecarChild`; also guarantees an `error` listener exists. */
class ChildAdapter implements SidecarChild {
  exited = false

  constructor(private readonly child: ChildProcess) {
    // A spawn failure (ENOENT/EACCES) is emitted as 'error'; with no listener Node would throw it
    // out of the event loop. The failure itself reaches the manager as a missing pid.
    child.on('error', () => {})
    child.once('exit', () => { this.exited = true })
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
