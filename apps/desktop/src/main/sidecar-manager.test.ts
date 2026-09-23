import { spawn } from 'node:child_process'
import { EventEmitter } from 'node:events'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { MemoryEndpointHandoff } from './sidecar-contract'
import { exactTreeStopper, SidecarManager, signalOwnedTree, validateReady, type SidecarHealth, type SidecarLaunch, type SidecarLauncher, type SidecarReady, type SidecarStopper } from './sidecar-manager'
vi.mock('node:child_process', () => ({ spawn: vi.fn() }))

const spec = { sidecarId: 'creative', readyTimeoutMs: 50, healthIntervalMs: 100, healthTimeoutMs: 10, maxHealthFailures: 2, shutdown: { termGraceMs: 1, killAfterMs: 1 } }
const child = { pid: 42, exitCode: null }
const ready = Promise.resolve({ endpoint: { origin: 'http://127.0.0.1:4312', secret: 'secret' } })
const platform = process.platform
function deferred<T>() {
  let resolve!: (value: T) => void; let reject!: (reason?: unknown) => void
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}
function useWindowsPlatform() {
  Object.defineProperty(process, 'platform', { configurable: true, value: 'win32' })
}

describe('SidecarManager', () => {
  afterEach(() => { Object.defineProperty(process, 'platform', { configurable: true, value: platform }); vi.restoreAllMocks() })
  it('validates endpoint records before endpoint handoff or health checks', () => {
    expect(() => validateReady({ endpoint: { origin: 'http://127.0.0.1:4312', secret: 'x' } })).not.toThrow(); expect(() => validateReady({ endpoint: { origin: 'https://[::1]', secret: 'x' } })).not.toThrow()
    expect(() => validateReady({ endpoint: { origin: 'http://example.com:4312', secret: 'x' } })).toThrow('loopback'); expect(() => validateReady({ endpoint: { origin: 'http://127.0.0.1:4312', secret: '' } })).toThrow('secret')
    expect(() => validateReady({ endpoint: { origin: 'http://127.0.0.1:4312', secret: 1 } as never })).toThrow('secret')
    for (const origin of ['ftp://127.0.0.1:21', 'http://x@127.0.0.1:4312', 'http://127.0.0.1:4312/path', 'http://127.0.0.1:4312?x=1', 'http://127.0.0.1:4312#x']) {
      expect(() => validateReady({ endpoint: { origin, secret: 'x' } })).toThrow()
    }
  })

  it('rejects invalid readiness before publishing an endpoint or probing health', async () => {
    const handoff = new MemoryEndpointHandoff(); const health: SidecarHealth = { check: vi.fn().mockResolvedValue(true) }; const stopper: SidecarStopper = { stop: vi.fn().mockResolvedValue(undefined) }
    const manager = new SidecarManager(
      spec,
      { launch: () => Promise.resolve({ child, processGroupId: 42, dedicatedProcessGroup: true, ready: Promise.resolve({ endpoint: { origin: 'http://127.0.0.1:4312/api', secret: 'secret' } }) }) },
      health,
      stopper,
      handoff,
    )

    await expect(manager.start()).rejects.toThrow('must not include'); expect(handoff.current()).toBeNull()
    expect(health.check).not.toHaveBeenCalled()
  })

  it('uses graceful then forced POSIX group signals and tolerates an absent process', async () => {
    const kill = vi.spyOn(process, 'kill').mockImplementation(() => true)
    await exactTreeStopper.stop({ sidecarId: 'creative', instanceId: 'a', pid: 42, processGroupId: 42, startedAt: 0 }, child, 1, 1)
    expect(kill.mock.calls.map(([pid, signal]) => [pid, signal])).toEqual([[-42, 'SIGTERM'], [-42, 'SIGKILL']])
    kill.mockImplementation(() => { throw Object.assign(new Error('gone'), { code: 'ESRCH' }) })
    await expect(signalOwnedTree({ sidecarId: 'creative', instanceId: 'a', pid: 42, processGroupId: null, startedAt: 0 }, 'SIGTERM')).resolves.toBeUndefined()
  })
  it('waits for Windows taskkill completion and surfaces its errors', async () => {
    useWindowsPlatform()
    const taskkill = new EventEmitter(); const taskkillSpawn = vi.mocked(spawn); taskkillSpawn.mockReturnValue(taskkill as never)
    const stopped = signalOwnedTree({ sidecarId: 'creative', instanceId: 'a', pid: 42, processGroupId: null, startedAt: 0 }, 'SIGTERM')
    let complete = false; void stopped.then(() => { complete = true })
    await Promise.resolve()
    expect(complete).toBe(false)
    expect(taskkillSpawn).toHaveBeenCalledWith('taskkill.exe', ['/PID', '42', '/T'], expect.any(Object)); taskkill.emit('close', 0)
    await expect(stopped).resolves.toBeUndefined()
    const failedTaskkill = new EventEmitter(); taskkillSpawn.mockReturnValueOnce(failedTaskkill as never)
    const failed = signalOwnedTree({ sidecarId: 'creative', instanceId: 'a', pid: 42, processGroupId: null, startedAt: 0 }, 'SIGKILL')
    failedTaskkill.emit('close', 1); await expect(failed).rejects.toThrow('taskkill exited with 1')
    const erroredTaskkill = new EventEmitter(); taskkillSpawn.mockReturnValueOnce(erroredTaskkill as never)
    const errored = signalOwnedTree({ sidecarId: 'creative', instanceId: 'a', pid: 42, processGroupId: null, startedAt: 0 }, 'SIGTERM')
    erroredTaskkill.emit('error', new Error('taskkill unavailable')); await expect(errored).rejects.toThrow('taskkill unavailable')
  })
  it('keeps malformed group cleanup PID-only', async () => {
    const lateReady = deferred<SidecarReady>(); const unhandled = vi.fn()
    process.on('unhandledRejection', unhandled)
    const stopper: SidecarStopper = { stop: vi.fn().mockResolvedValue(undefined) }
    const manager = new SidecarManager(
      spec,
      { launch: () => Promise.resolve({ child, processGroupId: 99, dedicatedProcessGroup: true, ready: lateReady.promise }) },
      { check: vi.fn() },
      stopper,
    )
    await expect(manager.start()).rejects.toThrow('owned child group'); lateReady.reject(new Error('late malformed readiness rejection'))
    await new Promise((resolve) => setTimeout(resolve, 0)); process.off('unhandledRejection', unhandled)
    expect(unhandled).not.toHaveBeenCalled()
    expect(stopper.stop).toHaveBeenCalledWith(expect.objectContaining({ pid: 42, processGroupId: null }), child, 1, 1)
  })

  it('requires readiness and health before reporting healthy, then stops its exact owner', async () => {
    const launcher: SidecarLauncher = { launch: () => Promise.resolve({ child, processGroupId: 42, dedicatedProcessGroup: true, ready }) }
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
    const launcher: SidecarLauncher = { launch: () => Promise.resolve({ child, processGroupId: 42, dedicatedProcessGroup: true, ready }) }
    const health: SidecarHealth = { check: vi.fn().mockResolvedValue(false) }
    const manager = new SidecarManager(spec, launcher, health, { stop: vi.fn().mockResolvedValue(undefined) })

    await expect(manager.start()).resolves.toMatchObject({ state: 'ready' })
    await new Promise((resolve) => setTimeout(resolve, 120))
    expect(manager.status().state).toBe('degraded')
    await manager.stop()
  })

  it('deduplicates concurrent stop cleanup and blocks restart while ownership is unresolved', async () => {
    const stopGate = deferred<void>()
    const stopper: SidecarStopper = { stop: vi.fn(() => stopGate.promise) }
    const launcher: SidecarLauncher = { launch: vi.fn(() => Promise.resolve({ child, processGroupId: 42, dedicatedProcessGroup: true, ready })) }
    const manager = new SidecarManager(spec, launcher, { check: vi.fn().mockResolvedValue(true) }, stopper)
    await manager.start()
    const first = manager.stop()
    const second = manager.stop()
    await vi.waitFor(() => expect(stopper.stop).toHaveBeenCalledOnce())
    expect(manager.status()).toMatchObject({ state: 'stopping', instanceId: expect.any(String) })
    await expect(manager.start()).resolves.toMatchObject({ state: 'stopping' })
    expect(launcher.launch).toHaveBeenCalledOnce()
    stopGate.resolve()
    await Promise.all([first, second])
    expect(manager.status().state).toBe('stopped')
  })
  it('retains failed ownership for a successful stop retry', async () => {
    const stopper: SidecarStopper = { stop: vi.fn().mockRejectedValueOnce(new Error('kill failed')).mockResolvedValueOnce(undefined) }
    const launcher: SidecarLauncher = { launch: vi.fn(() => Promise.resolve({ child, processGroupId: 42, dedicatedProcessGroup: true, ready })) }
    const manager = new SidecarManager(spec, launcher, { check: vi.fn().mockResolvedValue(true) }, stopper)
    await manager.start()
    const owner = manager.status().instanceId
    await expect(manager.stop()).rejects.toThrow('kill failed')
    expect(manager.status()).toMatchObject({ state: 'failed', instanceId: owner })
    await expect(manager.start()).resolves.toMatchObject({ state: 'failed', instanceId: owner })
    expect(launcher.launch).toHaveBeenCalledOnce()
    await expect(manager.stop()).resolves.toMatchObject({ state: 'stopped' })
    expect(stopper.stop).toHaveBeenCalledTimes(2)
  })
  it('cancels a start waiting for readiness without publishing or installing a timer', async () => {
    const interval = vi.spyOn(globalThis, 'setInterval')
    const readyGate = deferred<SidecarReady>()
    const launcher: SidecarLauncher = { launch: () => Promise.resolve({ child, processGroupId: 42, dedicatedProcessGroup: true, ready: readyGate.promise }) }
    const stopper: SidecarStopper = { stop: vi.fn().mockResolvedValue(undefined) }
    const handoff = new MemoryEndpointHandoff()
    const manager = new SidecarManager(spec, launcher, { check: vi.fn().mockResolvedValue(true) }, stopper, handoff)
    const starting = manager.start()
    await vi.waitFor(() => expect(manager.status().instanceId).toBeDefined())
    const owner = manager.status().instanceId
    await manager.stop()
    readyGate.reject(new Error('ready rejected after successful stop'))
    await expect(starting).resolves.toMatchObject({ state: 'stopped', sidecarId: 'creative' })
    expect(handoff.current()).toBeNull()
    expect(stopper.stop).toHaveBeenCalledWith(expect.objectContaining({ instanceId: owner }), child, 1, 1)
    expect(stopper.stop).toHaveBeenCalledOnce()
    expect(interval).not.toHaveBeenCalled()
  })
  it('cleans up a late child and handles its later readiness rejection', async () => {
    const launchGate = deferred<SidecarLaunch>()
    const readyGate = deferred<SidecarReady>()
    const lateChild = { pid: 84, exitCode: null }
    const freshChild = { pid: 85, exitCode: null }
    const launcher: SidecarLauncher = {
      launch: vi.fn().mockImplementationOnce(() => launchGate.promise).mockResolvedValueOnce({ child: freshChild, processGroupId: 85, dedicatedProcessGroup: true, ready }),
    }
    const stopper: SidecarStopper = { stop: vi.fn().mockResolvedValue(undefined) }
    const manager = new SidecarManager(spec, launcher, { check: vi.fn().mockResolvedValue(true) }, stopper)
    const starting = manager.start()
    await vi.waitFor(() => expect(launcher.launch).toHaveBeenCalledOnce())
    await manager.stop()
    launchGate.resolve({ child: lateChild, processGroupId: 84, dedicatedProcessGroup: true, ready: readyGate.promise })
    await expect(starting).resolves.toMatchObject({ state: 'stopped' })
    readyGate.reject(new Error('late readiness failure'))
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(stopper.stop).toHaveBeenCalledWith(expect.objectContaining({ pid: 84, processGroupId: 84 }), lateChild, 1, 1)
    expect(manager.status().state).toBe('stopped')
    await expect(manager.start()).resolves.toMatchObject({ state: 'healthy' })
    expect(launcher.launch).toHaveBeenCalledTimes(2)
    await manager.stop()
  })
  it('reports detached cancelled cleanup failure without overwriting a newer generation', async () => {
    const launchGate = deferred<SidecarLaunch>()
    const oldReady = deferred<SidecarReady>()
    const lateChild = { pid: 84, exitCode: null }
    const freshChild = { pid: 85, exitCode: null }
    const launcher: SidecarLauncher = {
      launch: vi.fn()
        .mockImplementationOnce(() => launchGate.promise)
        .mockResolvedValueOnce({ child: freshChild, processGroupId: 85, dedicatedProcessGroup: true, ready }),
    }
    const stopper: SidecarStopper = { stop: vi.fn().mockRejectedValueOnce(new Error('detached cleanup failed')).mockResolvedValue(undefined) }
    const manager = new SidecarManager(spec, launcher, { check: vi.fn().mockResolvedValue(true) }, stopper)
    const cancelled = manager.start()
    await vi.waitFor(() => expect(launcher.launch).toHaveBeenCalledOnce())
    await manager.stop()
    await expect(manager.start()).resolves.toMatchObject({ state: 'healthy' })
    launchGate.resolve({ child: lateChild, processGroupId: 84, dedicatedProcessGroup: true, ready: oldReady.promise })
    await expect(cancelled).rejects.toThrow('detached cleanup failed')
    oldReady.reject(new Error('late readiness failure'))
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(manager.status()).toMatchObject({ state: 'healthy' })
    expect(manager.status().instanceId).not.toBeUndefined()
    await expect(manager.stop()).resolves.toMatchObject({ state: 'stopped' })
    expect(stopper.stop.mock.calls.map(([owner]) => owner.pid)).toEqual([84, 85, 84])
  })
  it('gates stop and restart on a pending detached cleanup', async () => {
    const launchGate = deferred<SidecarLaunch>()
    const stopGate = deferred<void>()
    const lateChild = { pid: 84, exitCode: null }
    const freshChild = { pid: 85, exitCode: null }
    const launcher: SidecarLauncher = { launch: vi.fn().mockImplementationOnce(() => launchGate.promise).mockResolvedValueOnce({ child: freshChild, processGroupId: 85, dedicatedProcessGroup: true, ready }) }
    const stopper: SidecarStopper = { stop: vi.fn(() => stopGate.promise) }
    const manager = new SidecarManager(spec, launcher, { check: vi.fn().mockResolvedValue(true) }, stopper)
    const cancelled = manager.start()
    await manager.stop()
    launchGate.resolve({ child: lateChild, processGroupId: 84, dedicatedProcessGroup: true, ready })
    await vi.waitFor(() => expect(stopper.stop).toHaveBeenCalledOnce())
    const joining = manager.stop()
    await expect(manager.start()).resolves.toMatchObject({ state: 'stopping' })
    expect(launcher.launch).toHaveBeenCalledOnce()
    stopGate.resolve()
    await Promise.all([cancelled, joining])
    await expect(manager.start()).resolves.toMatchObject({ state: 'healthy' })
  })
  it('does not let an old detached failure fail a newer pending launch', async () => {
    const oldLaunch = deferred<SidecarLaunch>()
    const freshLaunch = deferred<SidecarLaunch>()
    const oldChild = { pid: 84, exitCode: null }
    const freshChild = { pid: 85, exitCode: null }
    const launcher: SidecarLauncher = { launch: vi.fn().mockImplementationOnce(() => oldLaunch.promise).mockImplementationOnce(() => freshLaunch.promise) }
    const stopper: SidecarStopper = { stop: vi.fn().mockRejectedValueOnce(new Error('old cleanup failed')).mockResolvedValue(undefined) }
    const manager = new SidecarManager(spec, launcher, { check: vi.fn().mockResolvedValue(true) }, stopper)
    const old = manager.start()
    await manager.stop()
    const fresh = manager.start()
    oldLaunch.resolve({ child: oldChild, processGroupId: 84, dedicatedProcessGroup: true, ready })
    await expect(old).rejects.toThrow('old cleanup failed')
    expect(manager.status().state).toBe('starting')
    freshLaunch.resolve({ child: freshChild, processGroupId: 85, dedicatedProcessGroup: true, ready })
    await expect(fresh).resolves.toMatchObject({ state: 'healthy' })
    await manager.stop()
  })
  it('ignores a deferred health result after stop', async () => {
    const interval = vi.spyOn(globalThis, 'setInterval')
    const healthGate = deferred<boolean>()
    const health: SidecarHealth = { check: vi.fn(() => healthGate.promise) }
    const stopper: SidecarStopper = { stop: vi.fn().mockResolvedValue(undefined) }
    const handoff = new MemoryEndpointHandoff()
    const manager = new SidecarManager(spec, { launch: () => Promise.resolve({ child, processGroupId: 42, dedicatedProcessGroup: true, ready }) }, health, stopper, handoff)
    const starting = manager.start()
    await vi.waitFor(() => expect(health.check).toHaveBeenCalledOnce())
    const owner = manager.status().instanceId
    await manager.stop()
    healthGate.resolve(true)
    await expect(starting).resolves.toMatchObject({ state: 'stopped' })
    expect(manager.status().instanceId).toBeUndefined()
    expect(handoff.current()).toBeNull()
    expect(stopper.stop).toHaveBeenCalledWith(expect.objectContaining({ instanceId: owner }), child, 1, 1)
    expect(interval).not.toHaveBeenCalled()
  })
  it('counts initial and interval health rejections without unhandled errors or owner changes', async () => {
    const health: SidecarHealth = { check: vi.fn().mockRejectedValue(new Error('secret health endpoint')) }
    const stopper: SidecarStopper = { stop: vi.fn().mockResolvedValue(undefined) }
    const logger = { info: vi.fn(), warn: vi.fn() }
    const handoff = new MemoryEndpointHandoff()
    const manager = new SidecarManager({ ...spec, healthIntervalMs: 2 }, { launch: () => Promise.resolve({ child, processGroupId: 42, dedicatedProcessGroup: true, ready }) }, health, stopper, handoff, logger)
    await expect(manager.start()).resolves.toMatchObject({ state: 'ready' })
    const owner = manager.status().instanceId
    await vi.waitFor(() => expect(manager.status().state).toBe('degraded'))
    await new Promise((resolve) => setTimeout(resolve, 10))
    expect(health.check.mock.calls.length).toBeGreaterThanOrEqual(2)
    expect(manager.status().instanceId).toBe(owner)
    expect(stopper.stop).not.toHaveBeenCalled()
    expect(JSON.stringify(logger.warn.mock.calls)).not.toContain('secret')
    await manager.stop()
  })
})
