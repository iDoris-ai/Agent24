import { afterEach, describe, expect, it, vi } from 'vitest'
import { exactTreeStopper, SidecarManager, type SidecarHealth, type SidecarLaunch, type SidecarLauncher, type SidecarReady, type SidecarStopper } from './sidecar-manager'
import { MemoryEndpointHandoff } from './sidecar-contract'

const spec = { sidecarId: 'creative', readyTimeoutMs: 50, healthIntervalMs: 100, healthTimeoutMs: 10, maxHealthFailures: 2, shutdown: { termGraceMs: 1, killAfterMs: 1 } }
const child = { pid: 42, exitCode: null }
const ready = Promise.resolve({ endpoint: { origin: 'http://127.0.0.1:4312', secret: 'secret' } })

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason?: unknown) => void
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise
    reject = rejectPromise
  })
  return { promise, resolve, reject }
}

describe('SidecarManager', () => {
  afterEach(() => vi.restoreAllMocks())

  it('uses graceful then forced signals for an owned process group', async () => {
    const kill = vi.spyOn(process, 'kill').mockImplementation(() => true)
    await exactTreeStopper.stop({ sidecarId: 'creative', instanceId: 'a', pid: 42, processGroupId: 42, startedAt: 0 }, child, 1, 1)
    expect(kill.mock.calls.map(([pid, signal]) => [pid, signal])).toEqual([[-42, 'SIGTERM'], [-42, 'SIGKILL']])
    kill.mockRestore()
  })

  it('requires readiness and health before reporting healthy, then stops its exact owner', async () => {
    const launcher: SidecarLauncher = { launch: () => Promise.resolve({ child, processGroupId: 42, ready }) }
    const health: SidecarHealth = { check: vi.fn().mockResolvedValue(true) }
    const stopper: SidecarStopper = { stop: vi.fn().mockResolvedValue(undefined) }
    const logger = { info: vi.fn(), warn: vi.fn() }
    const manager = new SidecarManager(spec, launcher, health, stopper, undefined, logger)

    await expect(manager.start()).resolves.toMatchObject({ state: 'healthy', sidecarId: 'creative' })
    expect(logger.info).toHaveBeenCalledWith('sidecar.ready', expect.objectContaining({ sidecarId: 'creative' }))
    expect(JSON.stringify(logger.info.mock.calls)).not.toContain('secret')
    await manager.stop()
    expect(stopper.stop).toHaveBeenCalledWith(expect.objectContaining({ pid: 42, processGroupId: 42 }), child, 1, 1)
    expect(manager.status().state).toBe('stopped')
  })

  it('bounds health failures without replacing the owned child', async () => {
    const launcher: SidecarLauncher = { launch: () => Promise.resolve({ child, processGroupId: 42, ready }) }
    const health: SidecarHealth = { check: vi.fn().mockResolvedValue(false) }
    const manager = new SidecarManager(spec, launcher, health, { stop: vi.fn().mockResolvedValue(undefined) })

    await expect(manager.start()).resolves.toMatchObject({ state: 'ready' })
    await new Promise((resolve) => setTimeout(resolve, 120))
    expect(manager.status().state).toBe('degraded')
    await manager.stop()
  })

  it('cancels a start waiting for readiness without publishing or installing a timer', async () => {
    const interval = vi.spyOn(globalThis, 'setInterval')
    const readyGate = deferred<SidecarReady>()
    const launcher: SidecarLauncher = { launch: () => Promise.resolve({ child, processGroupId: 42, ready: readyGate.promise }) }
    const stopper: SidecarStopper = { stop: vi.fn().mockResolvedValue(undefined) }
    const handoff = new MemoryEndpointHandoff()
    const manager = new SidecarManager(spec, launcher, { check: vi.fn().mockResolvedValue(true) }, stopper, handoff)
    const starting = manager.start()
    await vi.waitFor(() => expect(manager.status().instanceId).toBeDefined())
    const owner = manager.status().instanceId

    await manager.stop()
    readyGate.resolve({ endpoint: { origin: 'http://127.0.0.1:4312', secret: 'secret' } })
    await expect(starting).resolves.toMatchObject({ state: 'stopped', sidecarId: 'creative' })

    expect(manager.status()).toMatchObject({ state: 'stopped', sidecarId: 'creative' })
    expect(handoff.current()).toBeNull()
    expect(stopper.stop).toHaveBeenCalledTimes(1)
    expect(stopper.stop).toHaveBeenCalledWith(expect.objectContaining({ instanceId: owner }), child, 1, 1)
    expect(interval).not.toHaveBeenCalled()
    interval.mockRestore()
  })

  it('stops a child returned after its pending launch was cancelled', async () => {
    const interval = vi.spyOn(globalThis, 'setInterval')
    const launchGate = deferred<SidecarLaunch>()
    const readyGate = deferred<SidecarReady>()
    const lateChild = { pid: 84, exitCode: null }
    const launcher: SidecarLauncher = { launch: vi.fn(() => launchGate.promise) }
    const stopper: SidecarStopper = { stop: vi.fn().mockResolvedValue(undefined) }
    const handoff = new MemoryEndpointHandoff()
    const manager = new SidecarManager(spec, launcher, { check: vi.fn().mockResolvedValue(true) }, stopper, handoff)
    const unhandled: unknown[] = []
    const onUnhandled = (reason: unknown) => unhandled.push(reason)
    process.on('unhandledRejection', onUnhandled)
    try {
      const starting = manager.start()
      await vi.waitFor(() => expect(launcher.launch).toHaveBeenCalledOnce())
      await manager.stop()
      launchGate.resolve({ child: lateChild, processGroupId: 84, ready: readyGate.promise })

      await expect(starting).resolves.toMatchObject({ state: 'stopped', sidecarId: 'creative' })
      readyGate.reject(new Error('secret late readiness failure'))
      await new Promise((resolve) => setTimeout(resolve, 0))

      expect(manager.status()).toMatchObject({ state: 'stopped', sidecarId: 'creative' })
      expect(handoff.current()).toBeNull()
      expect(stopper.stop).toHaveBeenCalledOnce()
      expect(stopper.stop).toHaveBeenCalledWith(expect.objectContaining({ pid: 84, processGroupId: 84 }), lateChild, 1, 1)
      expect(interval).not.toHaveBeenCalled()
      expect(unhandled).toEqual([])
    } finally {
      process.off('unhandledRejection', onUnhandled)
      interval.mockRestore()
    }
  })

  it('ignores a deferred health result after stop', async () => {
    const interval = vi.spyOn(globalThis, 'setInterval')
    const healthGate = deferred<boolean>()
    const health: SidecarHealth = { check: vi.fn(() => healthGate.promise) }
    const stopper: SidecarStopper = { stop: vi.fn().mockResolvedValue(undefined) }
    const handoff = new MemoryEndpointHandoff()
    const manager = new SidecarManager(spec, { launch: () => Promise.resolve({ child, processGroupId: 42, ready }) }, health, stopper, handoff)
    const starting = manager.start()
    await vi.waitFor(() => expect(health.check).toHaveBeenCalledOnce())
    const owner = manager.status().instanceId

    await manager.stop()
    healthGate.resolve(true)
    await expect(starting).resolves.toMatchObject({ state: 'stopped' })

    expect(manager.status()).toMatchObject({ state: 'stopped' })
    expect(manager.status().instanceId).toBeUndefined()
    expect(handoff.current()).toBeNull()
    expect(stopper.stop).toHaveBeenCalledWith(expect.objectContaining({ instanceId: owner }), child, 1, 1)
    expect(interval).not.toHaveBeenCalled()
    interval.mockRestore()
  })

  it('counts initial and interval health rejections without unhandled errors or owner changes', async () => {
    const health: SidecarHealth = { check: vi.fn().mockRejectedValue(new Error('secret health endpoint')) }
    const stopper: SidecarStopper = { stop: vi.fn().mockResolvedValue(undefined) }
    const logger = { info: vi.fn(), warn: vi.fn() }
    const handoff = new MemoryEndpointHandoff()
    const manager = new SidecarManager({ ...spec, healthIntervalMs: 2 }, { launch: () => Promise.resolve({ child, processGroupId: 42, ready }) }, health, stopper, handoff, logger)
    const unhandled: unknown[] = []
    const onUnhandled = (reason: unknown) => unhandled.push(reason)
    process.on('unhandledRejection', onUnhandled)
    try {
      await expect(manager.start()).resolves.toMatchObject({ state: 'ready' })
      const owner = manager.status().instanceId
      await vi.waitFor(() => expect(manager.status().state).toBe('degraded'))
      await new Promise((resolve) => setTimeout(resolve, 10))
      expect(health.check.mock.calls.length).toBeGreaterThanOrEqual(2)
      expect(manager.status().instanceId).toBe(owner)
      expect(stopper.stop).not.toHaveBeenCalled()
      expect(JSON.stringify(logger.warn.mock.calls)).not.toContain('secret')
      expect(unhandled).toEqual([])
    } finally {
      process.off('unhandledRejection', onUnhandled)
      await manager.stop()
    }
  })
})
