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
export interface SidecarLaunch { readonly child: SidecarChild; readonly processGroupId?: number; readonly ready: Promise<SidecarReady> }
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
export function signalOwnedTree(owner: SidecarOwnership, signal: NodeJS.Signals): void {
  if (!Number.isInteger(owner.pid) || owner.pid <= 0) return
  if (process.platform === 'win32') {
    spawn('taskkill.exe', ['/PID', String(owner.pid), '/T', ...(signal === 'SIGKILL' ? ['/F'] : [])], { stdio: 'ignore', windowsHide: true })
    return
  }
  const target = owner.processGroupId === null ? owner.pid : -owner.processGroupId
  try { process.kill(target, signal) } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ESRCH') throw error
  }
}

export const exactTreeStopper: SidecarStopper = {
  async stop(owner, child, gracefulMs, killAfterMs) {
    signalOwnedTree(owner, 'SIGTERM')
    await wait(gracefulMs)
    if (child.exitCode === null) {
      signalOwnedTree(owner, 'SIGKILL')
      await wait(killAfterMs)
    }
  },
}

export class SidecarManager {
  private state: SidecarState = 'stopped'
  private active: { owner: SidecarOwnership; child: SidecarChild; ready?: SidecarReady } | null = null
  private timer: NodeJS.Timeout | null = null
  private failures = 0
  private probing = false

  constructor(
    private readonly spec: SidecarSpec,
    private readonly launcher: SidecarLauncher,
    private readonly health: SidecarHealth,
    private readonly stopper: SidecarStopper = exactTreeStopper,
    private readonly handoff: SidecarEndpointHandoff = new MemoryEndpointHandoff(),
    private readonly logger: SidecarLogger = silentLogger,
  ) {}

  status(): SidecarStatus {
    return { state: this.state, sidecarId: this.spec.sidecarId, instanceId: this.active?.owner.instanceId }
  }

  async start(): Promise<SidecarStatus> {
    if (this.state !== 'stopped') return this.status()
    this.state = 'starting'
    this.logger.info('sidecar.starting', { sidecarId: this.spec.sidecarId })
    try {
      const launch = await this.launcher.launch()
      if (!launch.child.pid) throw new Error('sidecar did not provide a pid')
      const owner = createOwnership(this.spec.sidecarId, launch.child.pid, launch.processGroupId ?? null)
      this.active = { owner, child: launch.child }
      const ready = await Promise.race([launch.ready, wait(this.spec.readyTimeoutMs).then(() => { throw new Error('sidecar readiness timeout') })])
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

  async stop(): Promise<SidecarStatus> {
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
    this.active = null
    await this.stopper.stop(active.owner, active.child, this.spec.shutdown.termGraceMs, this.spec.shutdown.killAfterMs)
  }

  private async probe(): Promise<void> {
    const active = this.active
    if (!active?.ready || this.probing) return
    this.probing = true
    try {
      const ok = await this.health.check(active.ready.endpoint, this.spec.healthTimeoutMs)
      this.failures = ok ? 0 : this.failures + 1
      this.state = ok ? 'healthy' : (this.failures >= this.spec.maxHealthFailures ? 'degraded' : 'ready')
      if (this.state === 'degraded') this.logger.warn('sidecar.degraded', { sidecarId: this.spec.sidecarId, failures: this.failures })
    } finally { this.probing = false }
  }
}
