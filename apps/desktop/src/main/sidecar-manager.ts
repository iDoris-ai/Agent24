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
  readonly signalCode: NodeJS.Signals | null
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
export interface SidecarLauncher { launch(): Promise<SidecarLaunch> }
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
const treeOperations = new WeakMap<SidecarTreeRunner, Map<string, Promise<unknown>>>()

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
      taskkill.once('close', (code) => code === 0 ? resolve() : reject(new Error(`taskkill exited with ${String(code)}`)))
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
    // A malformed/missing group never gains group authority: inspect its PID only.
    const target = owner.processGroupId === null ? owner.pid : -owner.processGroupId
    try {
      process.kill(target, 0)
      return false
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ESRCH') return true
      throw error
    }
  },
}

function boundedTreeOperation<T>(
  tree: SidecarTreeRunner,
  owner: SidecarOwnership,
  timeoutMs: number,
  name: string,
  operation: () => Promise<T>,
): Promise<T> {
  let operations = treeOperations.get(tree)
  if (!operations) { operations = new Map(); treeOperations.set(tree, operations) }
  if (operations.has(owner.instanceId)) return Promise.reject(new Error('sidecar tree operation is still pending'))
  const pending = Promise.resolve().then(operation)
  operations.set(owner.instanceId, pending)
  const release = () => {
    if (operations?.get(owner.instanceId) === pending) operations.delete(owner.instanceId)
  }
  void pending.then(release, release)
  return Promise.race([pending, wait(Math.max(1, timeoutMs)).then(() => { throw new Error(`sidecar tree ${name} timed out`) })])
}

function boundedSignal(tree: SidecarTreeRunner, owner: SidecarOwnership, force: boolean, timeoutMs: number): Promise<void> {
  return boundedTreeOperation(tree, owner, timeoutMs, 'signal', () => tree.signal(owner, force))
}

function boundedEmpty(tree: SidecarTreeRunner, owner: SidecarOwnership, timeoutMs: number): Promise<boolean> {
  return boundedTreeOperation(tree, owner, timeoutMs, 'emptiness check', () => tree.isEmpty(owner))
}

async function waitForEmpty(tree: SidecarTreeRunner, owner: SidecarOwnership, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + Math.max(0, timeoutMs)
  do {
    const remaining = Math.max(1, deadline - Date.now())
    if (await boundedEmpty(tree, owner, remaining)) return true
    if (Date.now() >= deadline) return false
    await wait(Math.min(10, Math.max(1, deadline - Date.now())))
  } while (Date.now() < deadline)
  return false
}

async function forceTreeStop(owner: SidecarOwnership, tree: SidecarTreeRunner, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + Math.max(0, timeoutMs)
  await boundedSignal(tree, owner, true, Math.max(1, deadline - Date.now()))
  if (!await waitForEmpty(tree, owner, Math.max(0, deadline - Date.now()))) throw new Error('sidecar process tree did not exit')
}

export const exactTreeStopper: SidecarStopper = {
  async stop(owner, _child, gracefulMs, killAfterMs, tree = nativeTreeRunner) {
    const deadline = Date.now() + Math.max(0, gracefulMs)
    await boundedSignal(tree, owner, false, Math.max(1, deadline - Date.now()))
    if (await waitForEmpty(tree, owner, Math.max(0, deadline - Date.now()))) return
    await forceTreeStop(owner, tree, killAfterMs)
  },
}

/** Reject endpoint records that would expose a non-local or ambiguous target. */
export function validateReady(ready: SidecarReady): void {
  const endpoint = ready?.endpoint
  if (typeof endpoint?.secret !== 'string' || endpoint.secret.length === 0) {
    throw new Error('sidecar readiness secret is empty')
  }
  if (typeof endpoint.origin !== 'string') throw new Error('sidecar endpoint origin is invalid')
  const url = new URL(endpoint.origin)
  if (!['http:', 'https:'].includes(url.protocol) || !['127.0.0.1', '[::1]'].includes(url.hostname)) {
    throw new Error('sidecar endpoint must use a loopback HTTP(S) origin')
  }
  if (url.username || url.password || url.pathname !== '/' || url.search || url.hash) {
    throw new Error('sidecar endpoint must not include credentials, a path, query, or fragment')
  }
  const port = url.port === '' ? (url.protocol === 'http:' ? 80 : 443) : Number(url.port)
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('sidecar endpoint port is invalid')
}

type ActiveSidecar = {
  readonly token: number
  readonly owner: SidecarOwnership
  readonly child: SidecarChild
  readonly tree?: SidecarTreeRunner
  ready?: SidecarReady
  probing: boolean
  exited?: boolean
  removeExit?: () => void
  stopPromise?: Promise<void>
}

export class SidecarManager {
  private state: SidecarState = 'stopped'
  private active: ActiveSidecar | null = null
  private timer: NodeJS.Timeout | null = null
  private failures = 0
  private generation = 0
  private stopping: Promise<void> | null = null
  private readonly detached = new Set<ActiveSidecar>()

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
    if (this.state !== 'stopped' || this.detached.size !== 0) return this.status()
    const token = ++this.generation
    this.state = 'starting'
    this.failures = 0
    this.logger.info('sidecar.starting', { sidecarId: this.spec.sidecarId })
    let attempt: ActiveSidecar | null = null
    let cleaningCancelledAttempt = false
    try {
      const launch = await this.launcher.launch()
      void launch.ready.catch(() => {})
      if (!Number.isInteger(launch.child.pid) || !launch.child.pid || launch.child.pid < 1) {
        if (!this.isStarting(token)) return this.cancelled()
        throw new Error('sidecar did not provide a pid')
      }
      attempt = { token, owner: createOwnership(this.spec.sidecarId, launch.child.pid), child: launch.child, tree: launch.tree, probing: false }
      const processGroupId = launch.processGroupId
      if (process.platform !== 'win32') {
        if (launch.dedicatedProcessGroup !== true) throw new Error('sidecar dedicated process group is required')
        if (typeof processGroupId !== 'number' || !Number.isInteger(processGroupId) || processGroupId !== launch.child.pid) {
          throw new Error('sidecar process group is not an owned child group')
        }
        attempt = { ...attempt, owner: createOwnership(this.spec.sidecarId, launch.child.pid, processGroupId) }
      } else if (!launch.tree) throw new Error('verified Windows tree control is required')
      if (!this.isStarting(token)) {
        cleaningCancelledAttempt = true
        await this.stopActive(attempt)
        return this.cancelled()
      }
      this.active = attempt
      attempt.removeExit = launch.child.onExit(() => this.handleExit(attempt!))
      // Some adapters do not replay an exit which occurred before subscription.
      if (launch.child.exitCode !== null || launch.child.signalCode !== null) this.handleExit(attempt)
      if (attempt.exited) throw new Error('sidecar exited before readiness')
      let readyTimeout: NodeJS.Timeout | undefined
      let ready: SidecarReady
      try {
        ready = await Promise.race([
          launch.ready,
          new Promise<SidecarReady>((_, reject) => {
            readyTimeout = setTimeout(() => reject(new Error('sidecar readiness timeout')), this.spec.readyTimeoutMs)
          }),
        ])
      } finally {
        if (readyTimeout) clearTimeout(readyTimeout)
      }
      if (attempt.exited) throw new Error('sidecar exited before readiness')
      if (!this.isCurrent(token, attempt)) return this.cancelled()
      validateReady(ready)
      attempt.ready = ready
      this.handoff.publish(attempt.owner.instanceId, ready.endpoint)
      this.state = 'ready'
      this.logger.info('sidecar.ready', { sidecarId: this.spec.sidecarId, instanceId: attempt.owner.instanceId })
      await this.probe(attempt)
      if (attempt.exited) throw new Error('sidecar exited during health check')
      if (!this.isCurrent(token, attempt)) return this.cancelled()
      this.timer = setInterval(() => { void this.probe(attempt!).catch(() => {}) }, this.spec.healthIntervalMs)
    } catch (error) {
      if (cleaningCancelledAttempt) throw error
      if (attempt?.exited) {
        await this.stopActive(attempt)
        throw error
      }
      const cancelled = attempt
        ? !this.isCurrent(token, attempt) && !this.isStarting(token)
        : !this.isStarting(token)
      if (cancelled) {
        if (attempt) await this.stopActive(attempt)
        return this.cancelled()
      }
      this.state = 'failed'
      this.logger.warn('sidecar.failed', { sidecarId: this.spec.sidecarId })
      if (attempt) await this.stopActive(attempt)
      throw error
    }
    return this.status()
  }

  async stop(): Promise<SidecarStatus> {
    const token = ++this.generation
    const active = this.active
    const attempts = [...new Set(active ? [active, ...this.detached] : this.detached)]
    if (attempts.length === 0) {
      const stopping = this.stopping
      if (stopping) await stopping
      if (this.generation === token && !this.active && this.detached.size === 0) this.state = 'stopped'
      return this.status()
    }
    if (this.stopping) {
      try {
        await this.stopping
      } finally {
        if (this.generation === token && !this.active && this.detached.size === 0) this.state = 'stopped'
      }
      return this.status()
    }
    this.state = 'stopping'
    this.logger.info('sidecar.stopping', { sidecarId: this.spec.sidecarId })
    const stopping = Promise.all(attempts.map((attempt) => this.stopActive(attempt))).then(() => {})
    this.stopping = stopping
    try {
      await stopping
    } finally {
      if (this.stopping === stopping) this.stopping = null
      if (this.generation === token && !this.active && this.detached.size === 0) this.state = 'stopped'
    }
    return this.status()
  }

  private isStarting(token: number): boolean {
    return this.generation === token && this.state === 'starting' && this.active === null
  }

  private isCurrent(token: number, active: ActiveSidecar | null): active is ActiveSidecar {
    return active !== null && this.generation === token && active.token === token && this.active === active
  }

  private cancelled(): SidecarStatus {
    return { state: 'stopped', sidecarId: this.spec.sidecarId }
  }

  private stopActive(active: ActiveSidecar, force = false): Promise<void> {
    if (active.stopPromise) return active.stopPromise
    const detached = this.active !== active
    if (detached) {
      this.detached.add(active)
      if (!this.active && this.state === 'stopped') this.state = 'stopping'
    }
    if (this.active === active) {
      if (this.timer) clearInterval(this.timer)
      this.timer = null
      this.handoff.clear(active.owner.instanceId)
    }
    const stopping = Promise.resolve().then(() =>
      force
        ? forceTreeStop(active.owner, active.tree ?? nativeTreeRunner, this.spec.shutdown.killAfterMs)
        : active.tree
          ? this.stopper.stop(active.owner, active.child, this.spec.shutdown.termGraceMs, this.spec.shutdown.killAfterMs, active.tree)
          : this.stopper.stop(active.owner, active.child, this.spec.shutdown.termGraceMs, this.spec.shutdown.killAfterMs),
    ).then(
      () => {
        if (this.active === active) this.active = null
        this.detached.delete(active)
        active.removeExit?.()
        active.removeExit = undefined
        if (!this.active && this.detached.size === 0 && this.state === 'stopping' && this.generation > active.token) this.state = 'stopped'
      },
      (error) => {
        if (this.active === active) this.state = 'failed'
        else if (!this.active && this.state === 'stopping') this.state = 'failed'
        if (active.stopPromise === stopping) active.stopPromise = undefined
        throw error
      },
    )
    active.stopPromise = stopping
    return stopping
  }

  private async probe(active: ActiveSidecar): Promise<void> {
    const token = active.token
    if (!this.isCurrent(token, active) || active.exited || !active.ready || active.probing) return
    active.probing = true
    let ok = false
    try {
      ok = await this.health.check(active.ready.endpoint, this.spec.healthTimeoutMs)
    } catch { ok = false } finally {
      active.probing = false
    }
    if (!this.isCurrent(token, active) || active.exited) return
    this.failures = ok ? 0 : this.failures + 1
    this.state = ok ? 'healthy' : (this.failures >= this.spec.maxHealthFailures ? 'degraded' : 'ready')
    if (this.state === 'degraded') this.logger.warn('sidecar.degraded', { sidecarId: this.spec.sidecarId, failures: this.failures })
  }

  private handleExit(active: ActiveSidecar): void {
    if (this.active !== active || active.exited) return
    active.exited = true
    if (this.timer) clearInterval(this.timer)
    this.timer = null
    this.handoff.clear(active.owner.instanceId)
    this.state = 'failed'
    this.logger.warn('sidecar.exited', { sidecarId: this.spec.sidecarId })
    void this.stopActive(active, true).catch(() => this.logger.warn('sidecar.tree_cleanup_failed', { sidecarId: this.spec.sidecarId }))
  }
}
