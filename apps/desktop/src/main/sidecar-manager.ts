import { spawn } from 'node:child_process'
import {
  createOwnership,
  MemoryEndpointHandoff,
  type SidecarEndpointHandoff,
  type SidecarOwnership,
  type SidecarState,
  type SidecarStatus,
} from './sidecar-contract'

export interface SidecarChild { readonly pid?: number; readonly exitCode: number | null }
export interface SidecarReady { readonly endpoint: { readonly origin: string; readonly secret: string } }
export interface SidecarLaunch {
  readonly child: SidecarChild
  readonly processGroupId?: number
  readonly dedicatedProcessGroup?: boolean
  readonly ready: Promise<SidecarReady>
}
export interface SidecarLauncher { launch(): Promise<SidecarLaunch> }
export interface SidecarHealth { check(endpoint: SidecarReady['endpoint'], timeoutMs: number): Promise<boolean> }
export interface SidecarLogger {
  info(event: string, fields?: Record<string, string | number>): void
  warn(event: string, fields?: Record<string, string | number>): void
}
export interface SidecarStopper {
  stop(owner: SidecarOwnership, child: SidecarChild, gracefulMs: number, killAfterMs: number): Promise<void>
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

export const exactTreeStopper: SidecarStopper = {
  async stop(owner, child, gracefulMs, killAfterMs) {
    await signalOwnedTree(owner, 'SIGTERM')
    await wait(gracefulMs)
    if (child.exitCode === null) {
      await signalOwnedTree(owner, 'SIGKILL')
      await wait(killAfterMs)
    }
  },
}

export function validateReady(ready: SidecarReady): void {
  if (!ready.endpoint.secret) throw new Error('sidecar readiness secret is empty')
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
  private active: { owner: SidecarOwnership; child: SidecarChild; ready?: SidecarReady } | null = null
  private timer: NodeJS.Timeout | null = null
  private failures = 0
  private probing = false
  private lifecycle: Promise<SidecarStatus>

  constructor(
    private readonly spec: SidecarSpec,
    private readonly launcher: SidecarLauncher,
    private readonly health: SidecarHealth,
    private readonly stopper: SidecarStopper = exactTreeStopper,
    private readonly handoff: SidecarEndpointHandoff = new MemoryEndpointHandoff(),
    private readonly logger: SidecarLogger = silentLogger,
  ) {
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
      const launch = await this.launcher.launch()
      const pid = launch.child.pid
      if (typeof pid !== 'number' || !Number.isInteger(pid) || pid <= 0) throw new Error('sidecar did not provide a valid pid')
      const group = launch.processGroupId
      let owner = createOwnership(this.spec.sidecarId, pid)
      this.active = { owner, child: launch.child }
      if (process.platform !== 'win32' && launch.dedicatedProcessGroup === true && (typeof group !== 'number' || !Number.isInteger(group) || group !== pid || group <= 0)) {
        throw new Error('sidecar process group is not an owned child group')
      }
      if (process.platform !== 'win32' && launch.dedicatedProcessGroup !== true && group !== undefined) {
        throw new Error('sidecar process group evidence is missing')
      }
      owner = createOwnership(this.spec.sidecarId, pid, process.platform === 'win32' ? null : group ?? null)
      this.active = { owner, child: launch.child }
      const ready = await Promise.race([launch.ready, wait(this.spec.readyTimeoutMs).then(() => { throw new Error('sidecar readiness timeout') })])
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

  private async stopActive(active: { owner: SidecarOwnership; child: SidecarChild }): Promise<void> {
    if (this.timer) clearInterval(this.timer)
    this.timer = null
    this.handoff.clear(active.owner.instanceId)
    try {
      await this.stopper.stop(active.owner, active.child, this.spec.shutdown.termGraceMs, this.spec.shutdown.killAfterMs)
    } catch (error) {
      if (this.active?.owner.instanceId === active.owner.instanceId) this.state = 'failed'
      throw error
    }
    if (this.active?.owner.instanceId === active.owner.instanceId) this.active = null
  }

  private async probe(): Promise<void> {
    const active = this.active
    if (!active?.ready || this.probing) return
    this.probing = true
    try {
      const ok = await this.health.check(active.ready.endpoint, this.spec.healthTimeoutMs)
      if (this.state === 'stopping' || this.active?.owner.instanceId !== active.owner.instanceId) return
      this.failures = ok ? 0 : this.failures + 1
      this.state = ok ? 'healthy' : (this.failures >= this.spec.maxHealthFailures ? 'degraded' : 'ready')
      if (this.state === 'degraded') this.logger.warn('sidecar.degraded', { sidecarId: this.spec.sidecarId, failures: this.failures })
    } finally { this.probing = false }
  }
}
