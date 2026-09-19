import { spawn } from 'node:child_process'
import {
  createOwnership,
  MemoryEndpointHandoff,
  type SidecarEndpointHandoff,
  type SidecarOwnership,
  type SidecarState,
  type SidecarStatus,
} from './sidecar-contract'

export interface SidecarChild {
  readonly pid?: number
  readonly exitCode: number | null
  onExit(listener: (code: number | null) => void): () => void
}
export interface SidecarReady { readonly endpoint: { readonly origin: string; readonly secret: string } }
export interface SidecarLaunch {
  readonly child: SidecarChild
  readonly processGroupId?: number
  readonly dedicatedProcessGroup?: boolean
  readonly tree?: SidecarTreeRunner
  readonly ready: Promise<SidecarReady>
}
/** Launch is synchronous: spawn and ownership setup must complete before returning. */
export interface SidecarLauncher { launch(): SidecarLaunch }
export interface SidecarHealth { check(endpoint: SidecarReady['endpoint'], timeoutMs: number): Promise<boolean> }
export interface SidecarTreeRunner {
  signal(owner: SidecarOwnership, force: boolean): Promise<void>
  isEmpty(owner: SidecarOwnership): Promise<boolean>
}
export interface SidecarLogger {
  info(event: string, fields?: Record<string, string | number>): void
  warn(event: string, fields?: Record<string, string | number>): void
}
export interface SidecarStopper {
  stop(owner: SidecarOwnership, child: SidecarChild, gracefulMs: number, killAfterMs: number, tree?: SidecarTreeRunner): Promise<void>
}
export interface SidecarSpec {
  readonly sidecarId: string
  readonly readyTimeoutMs: number
  readonly healthIntervalMs: number
  readonly healthTimeoutMs: number
  readonly maxHealthFailures: number
  readonly shutdown: { readonly termGraceMs: number; readonly killAfterMs: number }
}

const wait = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms))
const silentLogger: SidecarLogger = { info: () => {}, warn: () => {} }

function withTimeout<T>(promise: Promise<T>, timeoutMs: number, onTimeout: () => void, message: string): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { onTimeout(); reject(new Error(message)) }, timeoutMs)
    promise.then((value) => { clearTimeout(timer); resolve(value) }, (error) => { clearTimeout(timer); reject(error) })
  })
}

function validateSpec(spec: SidecarSpec): void {
  const timeouts = [spec.readyTimeoutMs, spec.healthIntervalMs, spec.healthTimeoutMs, spec.shutdown.termGraceMs, spec.shutdown.killAfterMs]
  if (timeouts.some((value) => !Number.isFinite(value) || value <= 0)) throw new Error('sidecar timeouts must be finite and positive')
  if (!Number.isInteger(spec.maxHealthFailures) || spec.maxHealthFailures <= 0) throw new Error('sidecar health failure bound is invalid')
}

/** Signals only the manager-owned PID or the launcher-provided dedicated group. */
export function signalOwnedTree(owner: SidecarOwnership, signal: NodeJS.Signals): Promise<void> {
  if (!Number.isInteger(owner.pid) || owner.pid <= 0) throw new Error('sidecar pid is invalid')
  if (owner.processGroupId !== null && (!Number.isInteger(owner.processGroupId) || owner.processGroupId <= 0)) {
    throw new Error('sidecar process group is invalid')
  }
  if (process.platform === 'win32') {
    return new Promise((resolve, reject) => {
      const taskkill = spawn('taskkill.exe', ['/PID', String(owner.pid), '/T', ...(signal === 'SIGKILL' ? ['/F'] : [])], { stdio: 'ignore', windowsHide: true })
      taskkill.once('error', reject)
      taskkill.once('exit', (code) => code === 0 ? resolve() : reject(new Error(`taskkill exited with ${String(code)}`)))
    })
  }
  const target = owner.processGroupId === null ? owner.pid : -owner.processGroupId
  try { process.kill(target, signal) } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ESRCH') throw error
  }
  return Promise.resolve()
}

const nativeTreeRunner: SidecarTreeRunner = {
  signal: (owner, force) => signalOwnedTree(owner, force ? 'SIGKILL' : 'SIGTERM'),
  async isEmpty(owner) {
    if (process.platform === 'win32') throw new Error('verified Windows tree control is required')
    if (owner.processGroupId === null) return false
    try { process.kill(-owner.processGroupId, 0); return false } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ESRCH') return true
      throw error
    }
  },
}

async function waitForEmpty(tree: SidecarTreeRunner, owner: SidecarOwnership, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs
  do {
    if (await tree.isEmpty(owner)) return true
    await wait(Math.min(10, Math.max(1, deadline - Date.now())))
  } while (Date.now() < deadline)
  return tree.isEmpty(owner)
}

export const exactTreeStopper: SidecarStopper = {
  async stop(owner, _child, gracefulMs, killAfterMs, tree = nativeTreeRunner) {
    await tree.signal(owner, false)
    if (await waitForEmpty(tree, owner, gracefulMs)) return
    await tree.signal(owner, true)
    if (!await waitForEmpty(tree, owner, killAfterMs)) throw new Error('sidecar process tree did not exit')
  },
}

export function validateReady(ready: SidecarReady): void {
  if (!ready || !ready.endpoint || typeof ready.endpoint.origin !== 'string') throw new Error('sidecar endpoint is invalid')
  if (typeof ready.endpoint.secret !== 'string' || !ready.endpoint.secret) throw new Error('sidecar readiness secret is empty')
  if (Buffer.byteLength(ready.endpoint.secret, 'utf8') < 32 || /[\s\p{Cc}]/u.test(ready.endpoint.secret)) throw new Error('sidecar readiness secret is weak')
  const url = new URL(ready.endpoint.origin)
  if (!['http:', 'https:'].includes(url.protocol) || !['127.0.0.1', '[::1]'].includes(url.hostname)) {
    throw new Error('sidecar endpoint must use a loopback HTTP(S) origin')
  }
  if (!url.port || url.username || url.password || url.pathname !== '/' || url.search || url.hash) {
    throw new Error('sidecar endpoint must include an explicit loopback port')
  }
  const port = Number(url.port)
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('sidecar endpoint port is invalid')
}

export class SidecarManager {
  private state: SidecarState = 'stopped'
  private active: {
    owner: SidecarOwnership
    child: SidecarChild
    tree?: SidecarTreeRunner
    ready?: SidecarReady
    removeExit?: () => void
    failures: number
    probing: boolean
    reaping?: Promise<void>
  } | null = null
  private timer: NodeJS.Timeout | null = null
  private lifecycle: Promise<SidecarStatus>

  constructor(
    private readonly spec: SidecarSpec,
    private readonly launcher: SidecarLauncher,
    private readonly health: SidecarHealth,
    private readonly stopper: SidecarStopper = exactTreeStopper,
    private readonly handoff: SidecarEndpointHandoff = new MemoryEndpointHandoff(),
    private readonly logger: SidecarLogger = silentLogger,
  ) {
    validateSpec(spec)
    this.lifecycle = Promise.resolve(this.status())
  }

  status(): SidecarStatus {
    return { state: this.state, sidecarId: this.spec.sidecarId, instanceId: this.active?.owner.instanceId }
  }

  start(): Promise<SidecarStatus> {
    const operation = this.lifecycle.then(() => this.startInternal())
    this.lifecycle = operation.catch(() => this.status())
    return operation
  }

  private async startInternal(): Promise<SidecarStatus> {
    if (this.state !== 'stopped') return this.status()
    this.state = 'starting'
      this.logger.info('sidecar.starting', { sidecarId: this.spec.sidecarId })
    try {
      const launch = this.launcher.launch()
      const pid = launch.child.pid
      if (typeof pid !== 'number' || !Number.isInteger(pid) || pid <= 0) throw new Error('sidecar did not provide a valid pid')
      const group = launch.processGroupId
      let owner = createOwnership(this.spec.sidecarId, pid)
      this.active = { owner, child: launch.child, tree: launch.tree, failures: 0, probing: false }
      if (process.platform !== 'win32' && launch.dedicatedProcessGroup !== true) {
        throw new Error('sidecar dedicated process group is required')
      }
      if (process.platform === 'win32' && !launch.tree) throw new Error('verified Windows tree control is required')
      if (process.platform !== 'win32' && (typeof group !== 'number' || !Number.isInteger(group) || group !== pid || group <= 0)) {
        throw new Error('sidecar process group is not an owned child group')
      }
      if (process.platform !== 'win32' && launch.dedicatedProcessGroup !== true && group !== undefined) {
        throw new Error('sidecar process group evidence is missing')
      }
      owner = createOwnership(this.spec.sidecarId, pid, process.platform === 'win32' ? null : group ?? null)
      this.active = { owner, child: launch.child, tree: launch.tree, failures: 0, probing: false }
      const generation = owner.instanceId
      this.active.removeExit = launch.child.onExit(() => this.handleExit(generation))
      const ready = await withTimeout(Promise.resolve(launch.ready), this.spec.readyTimeoutMs, () => {}, 'sidecar readiness timeout')
      if (this.active?.owner.instanceId !== generation || this.state !== 'starting') throw new Error('sidecar exited before readiness')
      validateReady(ready)
      this.active.ready = ready
      this.handoff.publish(owner.instanceId, ready.endpoint)
      this.state = 'ready'
      this.logger.info('sidecar.ready', { sidecarId: this.spec.sidecarId, instanceId: owner.instanceId })
      await this.probe()
      this.timer = setInterval(() => { void this.probe() }, this.spec.healthIntervalMs)
    } catch (error) {
      this.state = 'failed'
      this.logger.warn('sidecar.failed', { sidecarId: this.spec.sidecarId })
      const active = this.active
      if (active) await this.stopActive(active)
      throw error
    }
    return this.status()
  }

  stop(): Promise<SidecarStatus> {
    const operation = this.lifecycle.then(() => this.stopInternal())
    this.lifecycle = operation.catch(() => this.status())
    return operation
  }

  private async stopInternal(): Promise<SidecarStatus> {
    if (!this.active) { this.state = 'stopped'; return this.status() }
    this.state = 'stopping'
    this.logger.info('sidecar.stopping', { sidecarId: this.spec.sidecarId })
    await this.stopActive(this.active)
    this.state = 'stopped'
    return this.status()
  }

  private async stopActive(active: { owner: SidecarOwnership; child: SidecarChild; tree?: SidecarTreeRunner; removeExit?: () => void; reaping?: Promise<void> }): Promise<void> {
    if (this.active?.reaping) await this.active.reaping
    if (this.active?.owner.instanceId !== active.owner.instanceId) return
    if (this.timer) clearInterval(this.timer)
    this.timer = null
    this.handoff.clear(active.owner.instanceId)
    try {
      await this.stopper.stop(active.owner, active.child, this.spec.shutdown.termGraceMs, this.spec.shutdown.killAfterMs, this.active.tree)
    } catch (error) {
      if (this.active?.owner.instanceId === active.owner.instanceId) this.state = 'failed'
      throw error
    }
    if (this.active?.owner.instanceId === active.owner.instanceId) {
      active.removeExit?.()
      active.removeExit = undefined
      this.active = null
    }
  }

  private async probe(): Promise<void> {
    const active = this.active
    if (!active?.ready || active.probing) return
    active.probing = true
    try {
      const ok = await withTimeout(
        Promise.resolve().then(() => this.health.check(active.ready!.endpoint, this.spec.healthTimeoutMs)),
        this.spec.healthTimeoutMs,
        () => {},
        'sidecar health timeout',
      )
      if (this.state === 'stopping' || this.state === 'failed' || this.active?.owner.instanceId !== active.owner.instanceId) return
      active.failures = ok ? 0 : active.failures + 1
      this.state = ok ? 'healthy' : (active.failures >= this.spec.maxHealthFailures ? 'degraded' : 'ready')
      if (this.state === 'degraded') this.logger.warn('sidecar.degraded', { sidecarId: this.spec.sidecarId, failures: active.failures })
    } catch (error) {
      if (this.active?.owner.instanceId === active.owner.instanceId && this.state !== 'stopping' && this.state !== 'failed') {
        active.failures += 1
        this.state = active.failures >= this.spec.maxHealthFailures ? 'degraded' : 'ready'
        this.logger.warn('sidecar.health_error', { sidecarId: this.spec.sidecarId, failures: active.failures })
      }
    } finally { active.probing = false }
  }

  private handleExit(instanceId: string): void {
    const active = this.active
    if (!active || active.owner.instanceId !== instanceId || active.reaping) return
    if (this.timer) clearInterval(this.timer)
    this.timer = null
    this.handoff.clear(instanceId)
    this.state = 'failed'
    this.logger.warn('sidecar.exited', { sidecarId: this.spec.sidecarId })
    active.reaping = this.reapExited(active)
  }

  private async reapExited(active: { owner: SidecarOwnership; tree?: SidecarTreeRunner; removeExit?: () => void }): Promise<void> {
    try {
      const tree = active.tree ?? nativeTreeRunner
      await tree.signal(active.owner, true)
      if (!await waitForEmpty(tree, active.owner, this.spec.shutdown.killAfterMs)) throw new Error('sidecar descendants remain')
      if (this.active?.owner.instanceId === active.owner.instanceId) {
        active.removeExit?.()
        active.removeExit = undefined
        this.active = null
      }
    } catch (error) {
      this.logger.warn('sidecar.tree_cleanup_failed', { sidecarId: this.spec.sidecarId })
    }
  }
}
