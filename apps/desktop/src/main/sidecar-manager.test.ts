import { spawn } from 'node:child_process'
import { EventEmitter } from 'node:events'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { MemoryEndpointHandoff } from './sidecar-contract'
import { exactTreeStopper, SidecarManager, signalOwnedTree, validateReady, type SidecarHealth, type SidecarLauncher, type SidecarReady, type SidecarStopper, type SidecarTreeRunner } from './sidecar-manager'
vi.mock('node:child_process', () => ({ spawn: vi.fn() }))

const spec = { sidecarId: 'creative', readyTimeoutMs: 50, healthIntervalMs: 100, healthTimeoutMs: 10, maxHealthFailures: 2, shutdown: { termGraceMs: 1, killAfterMs: 1 } }
const childFor = (pid: number) => ({ pid, exitCode: null, signalCode: null, onExit: () => () => {} })
const child = childFor(42)
const ready = Promise.resolve({ endpoint: { origin: 'http://127.0.0.1:4312', secret: 's'.repeat(32) } })
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
    expect(() => validateReady({ endpoint: { origin: 'http://127.0.0.1:4312', secret: 's'.repeat(32) } })).not.toThrow(); expect(() => validateReady({ endpoint: { origin: 'https://[::1]', secret: 's'.repeat(32) } })).not.toThrow()
    expect(() => validateReady({ endpoint: { origin: 'http://example.com:4312', secret: 's'.repeat(32) } })).toThrow('loopback'); expect(() => validateReady({ endpoint: { origin: 'http://127.0.0.1:4312', secret: '' } })).toThrow('secret')
    expect(() => validateReady({ endpoint: { origin: 'http://127.0.0.1:4312', secret: 1 } as never })).toThrow('secret')
    for (const origin of ['ftp://127.0.0.1:21', 'http://x@127.0.0.1:4312', 'http://127.0.0.1:4312/path', 'http://127.0.0.1:4312?x=1', 'http://127.0.0.1:4312#x']) {
      expect(() => validateReady({ endpoint: { origin, secret: 's'.repeat(32) } })).toThrow()
    }
  })
  it('rejects weak readiness secrets and invalid lifecycle bounds', () => {
    expect(() => validateReady({ endpoint: { origin: 'http://127.0.0.1:4312', secret: 'short' } })).toThrow('weak')
    expect(() => validateReady({ endpoint: { origin: 'http://127.0.0.1:4312', secret: `s${' '.repeat(31)}` } })).toThrow('weak')
    const launcher = { launch: () => ({ child, processGroupId: 42, dedicatedProcessGroup: true, ready }) }
    expect(() => new SidecarManager({ ...spec, healthTimeoutMs: 0 }, launcher, { check: vi.fn() })).toThrow('finite and positive')
    expect(() => new SidecarManager({ ...spec, maxHealthFailures: Number.NaN }, launcher, { check: vi.fn() })).toThrow('failure bound')
  })

  it('rejects invalid readiness before publishing an endpoint or probing health', async () => {
    const handoff = new MemoryEndpointHandoff(); const health: SidecarHealth = { check: vi.fn().mockResolvedValue(true) }; const stopper: SidecarStopper = { stop: vi.fn().mockResolvedValue(undefined) }
    const manager = new SidecarManager(
      spec,
      { launch: () => ({ child, processGroupId: 42, dedicatedProcessGroup: true, ready: Promise.resolve({ endpoint: { origin: 'http://127.0.0.1:4312/api', secret: 's'.repeat(32) } }) }) },
      health,
      stopper,
      handoff,
    )

    await expect(manager.start()).rejects.toThrow('must not include'); expect(handoff.current()).toBeNull()
    expect(health.check).not.toHaveBeenCalled()
  })

  it('signals a POSIX group and tolerates an absent process', async () => {
    const kill = vi.spyOn(process, 'kill').mockImplementation(() => true)
    await signalOwnedTree({ sidecarId: 'creative', instanceId: 'a', pid: 42, processGroupId: 42, startedAt: 0 }, 'SIGTERM')
    expect(kill).toHaveBeenCalledWith(-42, 'SIGTERM')
    kill.mockImplementation(() => { throw Object.assign(new Error('gone'), { code: 'ESRCH' }) })
    await expect(signalOwnedTree({ sidecarId: 'creative', instanceId: 'a', pid: 42, processGroupId: null, startedAt: 0 }, 'SIGTERM')).resolves.toBeUndefined()
  })
  it('escalates an injected tree and verifies forced emptiness', async () => {
    let forced = false
    const tree: SidecarTreeRunner = { signal: vi.fn((_owner, force) => { forced = force; return Promise.resolve() }), isEmpty: vi.fn(() => Promise.resolve(forced)) }
    await exactTreeStopper.stop({ sidecarId: 'creative', instanceId: 'a', pid: 42, processGroupId: 42, startedAt: 0 }, child, 5, 5, tree)
    expect(tree.signal.mock.calls.map(([, force]) => force)).toEqual([false, true])
  })
  it('fails when a tree remains nonempty or its check fails', async () => {
    const owner = { sidecarId: 'creative', instanceId: 'a', pid: 42, processGroupId: 42, startedAt: 0 }
    const nonempty: SidecarTreeRunner = { signal: vi.fn().mockResolvedValue(undefined), isEmpty: vi.fn().mockResolvedValue(false) }
    await expect(exactTreeStopper.stop(owner, child, 5, 5, nonempty)).rejects.toThrow('did not exit')
    const broken: SidecarTreeRunner = { signal: vi.fn().mockResolvedValue(undefined), isEmpty: vi.fn().mockRejectedValue(new Error('tree probe failed')) }
    await expect(exactTreeStopper.stop(owner, child, 5, 5, broken)).rejects.toThrow('tree probe failed')
  })
  it('bounds an unresolved tree check', async () => {
    const tree: SidecarTreeRunner = { signal: vi.fn().mockResolvedValue(undefined), isEmpty: vi.fn(() => new Promise<boolean>(() => {})) }
    await expect(exactTreeStopper.stop({ sidecarId: 'creative', instanceId: 'a', pid: 42, processGroupId: 42, startedAt: 0 }, child, 5, 5, tree)).rejects.toThrow('emptiness check timed out')
  })
  it('allows a single delayed emptiness probe to use the phase budget', async () => {
    const tree: SidecarTreeRunner = { signal: vi.fn().mockResolvedValue(undefined), isEmpty: vi.fn(() => new Promise((resolve) => setTimeout(() => resolve(true), 30))) }
    await expect(exactTreeStopper.stop({ sidecarId: 'creative', instanceId: 'delayed-empty', pid: 42, processGroupId: 42, startedAt: 0 }, child, 100, 5, tree)).resolves.toBeUndefined()
    expect(tree.signal.mock.calls.map(([, force]) => force)).toEqual([false])
  })
  it('bounds unresolved graceful and forced tree signals', async () => {
    const owner = { sidecarId: 'creative', instanceId: 'signal-timeout', pid: 42, processGroupId: 42, startedAt: 0 }
    const graceful: SidecarTreeRunner = { signal: vi.fn(() => new Promise<void>(() => {})), isEmpty: vi.fn() }
    await expect(exactTreeStopper.stop(owner, child, 5, 5, graceful)).rejects.toThrow('signal timed out')
    expect(graceful.isEmpty).not.toHaveBeenCalled()
    const forced: SidecarTreeRunner = {
      signal: vi.fn((_owner, force) => force ? new Promise<void>(() => {}) : Promise.resolve()),
      isEmpty: vi.fn().mockResolvedValue(false),
    }
    await expect(exactTreeStopper.stop({ ...owner, instanceId: 'force-timeout' }, child, 5, 5, forced)).rejects.toThrow('signal timed out')
    expect(forced.signal.mock.calls.map(([, force]) => force)).toEqual([false, true])
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
      { launch: () => ({ child, processGroupId: 99, dedicatedProcessGroup: true, ready: lateReady.promise }) },
      { check: vi.fn() },
      stopper,
    )
    await expect(manager.start()).rejects.toThrow('owned child group'); lateReady.reject(new Error('late malformed readiness rejection'))
    await new Promise((resolve) => setTimeout(resolve, 0)); process.off('unhandledRejection', unhandled)
    expect(unhandled).not.toHaveBeenCalled()
    expect(stopper.stop).toHaveBeenCalledWith(expect.objectContaining({ pid: 42, processGroupId: null }), child, 1, 1)
  })

  it('requires readiness and health before reporting healthy, then stops its exact owner', async () => {
    const launcher: SidecarLauncher = { launch: () => ({ child, processGroupId: 42, dedicatedProcessGroup: true, ready }) }
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
    const launcher: SidecarLauncher = { launch: () => ({ child, processGroupId: 42, dedicatedProcessGroup: true, ready }) }
    const health: SidecarHealth = { check: vi.fn().mockResolvedValue(false) }
    const manager = new SidecarManager(spec, launcher, health, { stop: vi.fn().mockResolvedValue(undefined) })

    await expect(manager.start()).resolves.toMatchObject({ state: 'ready' })
    await new Promise((resolve) => setTimeout(resolve, 120))
    expect(manager.status().state).toBe('degraded')
    await manager.stop()
  })
  it('bounds pending readiness and cleans up its exact owner', async () => {
    const stopper: SidecarStopper = { stop: vi.fn().mockResolvedValue(undefined) }
    const manager = new SidecarManager(
      { ...spec, readyTimeoutMs: 5 },
      { launch: () => ({ child, processGroupId: 42, dedicatedProcessGroup: true, ready: new Promise<SidecarReady>(() => {}) }) },
      { check: vi.fn() },
      stopper,
    )
    await expect(manager.start()).rejects.toThrow('readiness timeout')
    expect(stopper.stop).toHaveBeenCalledWith(expect.objectContaining({ pid: 42, processGroupId: 42 }), child, 1, 1)
  })
  it('bounds a permanently pending health probe and isolates its late result after exit', async () => {
    let exit!: (code: number | null) => void
    const lateHealth = deferred<boolean>()
    const tree: SidecarTreeRunner = { signal: vi.fn().mockResolvedValue(undefined), isEmpty: vi.fn().mockResolvedValue(true) }
    const exitingChild = { pid: 45, exitCode: null, signalCode: null, onExit: (listener: (code: number | null) => void) => { exit = listener; return () => {} } }
    const manager = new SidecarManager(
      { ...spec, healthTimeoutMs: 5, healthIntervalMs: 100 },
      { launch: () => ({ child: exitingChild, processGroupId: 45, dedicatedProcessGroup: true, tree, ready }) },
      { check: vi.fn(() => lateHealth.promise) },
    )
    await expect(manager.start()).resolves.toMatchObject({ state: 'ready' })
    exit(1)
    lateHealth.reject(new Error('late health failure'))
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(manager.status().state).toBe('failed')
  })

  it('deduplicates concurrent stop cleanup and blocks restart while ownership is unresolved', async () => {
    const stopGate = deferred<void>()
    const stopper: SidecarStopper = { stop: vi.fn(() => stopGate.promise) }
    const launcher: SidecarLauncher = { launch: vi.fn(() => ({ child, processGroupId: 42, dedicatedProcessGroup: true, ready })) }
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
    const launcher: SidecarLauncher = { launch: vi.fn(() => ({ child, processGroupId: 42, dedicatedProcessGroup: true, ready })) }
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
    const launcher: SidecarLauncher = { launch: () => ({ child, processGroupId: 42, dedicatedProcessGroup: true, ready: readyGate.promise }) }
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
  it('ignores a deferred health result after stop', async () => {
    const interval = vi.spyOn(globalThis, 'setInterval')
    const healthGate = deferred<boolean>()
    const health: SidecarHealth = { check: vi.fn(() => healthGate.promise) }
    const stopper: SidecarStopper = { stop: vi.fn().mockResolvedValue(undefined) }
    const handoff = new MemoryEndpointHandoff()
    const manager = new SidecarManager({ ...spec, healthTimeoutMs: 1000 }, { launch: () => ({ child, processGroupId: 42, dedicatedProcessGroup: true, ready }) }, health, stopper, handoff)
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
    const manager = new SidecarManager({ ...spec, healthIntervalMs: 2 }, { launch: () => ({ child, processGroupId: 42, dedicatedProcessGroup: true, ready }) }, health, stopper, handoff, logger)
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
  it('requires a verified Windows tree and keeps malformed POSIX groups PID-only', async () => {
    useWindowsPlatform()
    const stopper: SidecarStopper = { stop: vi.fn().mockResolvedValue(undefined) }
    const manager = new SidecarManager(spec, { launch: () => ({ child, ready }) }, { check: vi.fn() }, stopper)
    await expect(manager.start()).rejects.toThrow('verified Windows tree control')
    expect(stopper.stop).toHaveBeenCalledWith(expect.objectContaining({ pid: 42, processGroupId: null }), child, 1, 1)
  })
  it('requires a dedicated POSIX group before readiness', async () => {
    const stopper: SidecarStopper = { stop: vi.fn().mockResolvedValue(undefined) }
    const manager = new SidecarManager(spec, { launch: () => ({ child, ready }) }, { check: vi.fn() }, stopper)
    await expect(manager.start()).rejects.toThrow('dedicated process group is required')
    expect(stopper.stop).toHaveBeenCalledWith(expect.objectContaining({ pid: 42, processGroupId: null }), child, 1, 1)
  })
  it('handles synchronous duplicate exit callbacks before readiness', async () => {
    const tree: SidecarTreeRunner = { signal: vi.fn().mockResolvedValue(undefined), isEmpty: vi.fn().mockResolvedValue(true) }
    const remove = vi.fn()
    const syncChild = { pid: 43, exitCode: null, signalCode: null, onExit: (listener: (code: number | null) => void) => { listener(0); listener(0); return remove } }
    const manager = new SidecarManager(spec, { launch: () => ({ child: syncChild, processGroupId: 43, dedicatedProcessGroup: true, tree, ready }) }, { check: vi.fn() })
    await expect(manager.start()).rejects.toThrow('exited before readiness')
    await vi.waitFor(() => expect(remove).toHaveBeenCalledOnce())
    expect(tree.signal).toHaveBeenCalledTimes(1)
  })
  it('treats a non-replaying SIGTERM-exited child as a natural exit before readiness', async () => {
    const tree: SidecarTreeRunner = { signal: vi.fn().mockResolvedValue(undefined), isEmpty: vi.fn().mockResolvedValue(true) }
    const exitedChild = { pid: 50, exitCode: null, signalCode: 'SIGTERM' as NodeJS.Signals, onExit: vi.fn(() => () => {}) }
    const health: SidecarHealth = { check: vi.fn().mockResolvedValue(true) }
    const handoff = new MemoryEndpointHandoff()
    const manager = new SidecarManager(spec, { launch: () => ({ child: exitedChild, processGroupId: 50, dedicatedProcessGroup: true, tree, ready }) }, health, undefined, handoff)
    await expect(manager.start()).rejects.toThrow('exited before readiness')
    expect(handoff.current()).toBeNull()
    expect(health.check).not.toHaveBeenCalled()
    expect(tree.signal).toHaveBeenCalledWith(expect.anything(), true)
  })
  it('clears a ready endpoint when a child exits during its initial health check', async () => {
    let exit!: (code: number | null) => void
    const healthGate = deferred<boolean>()
    const tree: SidecarTreeRunner = { signal: vi.fn().mockResolvedValue(undefined), isEmpty: vi.fn().mockResolvedValue(true) }
    const exitingChild = { pid: 44, exitCode: null, signalCode: null, onExit: (listener: (code: number | null) => void) => { exit = listener; return () => {} } }
    const handoff = new MemoryEndpointHandoff()
    const manager = new SidecarManager({ ...spec, healthTimeoutMs: 1000 }, { launch: () => ({ child: exitingChild, processGroupId: 44, dedicatedProcessGroup: true, tree, ready }) }, { check: vi.fn(() => healthGate.promise) }, undefined, handoff)
    const starting = manager.start()
    await vi.waitFor(() => expect(handoff.current()).not.toBeNull())
    exit(0)
    expect(handoff.current()).toBeNull()
    healthGate.resolve(true)
    await expect(starting).rejects.toThrow('exited during health check')
    expect(manager.status()).toMatchObject({ state: 'failed', instanceId: undefined })
  })
  it('invalidates the endpoint and timer immediately after a ready child exits', async () => {
    let exit!: (code: number | null) => void
    const clear = vi.spyOn(globalThis, 'clearInterval')
    const tree: SidecarTreeRunner = { signal: vi.fn().mockResolvedValue(undefined), isEmpty: vi.fn().mockResolvedValue(true) }
    const exitingChild = { pid: 48, exitCode: null, signalCode: null, onExit: (listener: (code: number | null) => void) => { exit = listener; return () => {} } }
    const handoff = new MemoryEndpointHandoff()
    const manager = new SidecarManager(spec, { launch: () => ({ child: exitingChild, processGroupId: 48, dedicatedProcessGroup: true, tree, ready }) }, { check: vi.fn().mockResolvedValue(true) }, undefined, handoff)
    await manager.start()
    exit(0)
    expect(handoff.current()).toBeNull()
    expect(clear).toHaveBeenCalled()
  })
  it('joins an explicit stop to pending natural cleanup', async () => {
    let exit!: (code: number | null) => void
    const tree: SidecarTreeRunner = { signal: vi.fn().mockResolvedValue(undefined), isEmpty: vi.fn().mockResolvedValue(true) }
    const exitingChild = { pid: 49, exitCode: null, signalCode: null, onExit: (listener: (code: number | null) => void) => { exit = listener; return () => {} } }
    const manager = new SidecarManager({ ...spec, shutdown: { termGraceMs: 1000, killAfterMs: 1000 } }, { launch: () => ({ child: exitingChild, processGroupId: 49, dedicatedProcessGroup: true, tree, ready }) }, { check: vi.fn().mockResolvedValue(true) })
    await manager.start()
    exit(0)
    const stopping = manager.stop()
    await expect(stopping).resolves.toMatchObject({ state: 'stopped' })
    expect(tree.signal.mock.calls.map(([, force]) => force)).toEqual([true])
  })
  it('joins a concurrent stop to natural-exit cleanup and retries a failed cleanup', async () => {
    let exit!: (code: number | null) => void
    const tree: SidecarTreeRunner = { signal: vi.fn().mockResolvedValue(undefined), isEmpty: vi.fn().mockRejectedValueOnce(new Error('tree error')).mockResolvedValue(true) }
    const exitingChild = { pid: 45, exitCode: null, signalCode: null, onExit: (listener: (code: number | null) => void) => { exit = listener; return () => {} } }
    const manager = new SidecarManager(spec, { launch: () => ({ child: exitingChild, processGroupId: 45, dedicatedProcessGroup: true, tree, ready }) }, { check: vi.fn().mockResolvedValue(true) })
    await manager.start()
    exit(0)
    const joined = manager.stop()
    await expect(joined).rejects.toThrow('tree error')
    await expect(manager.stop()).resolves.toMatchObject({ state: 'stopped' })
    expect(tree.signal.mock.calls.map(([, force]) => force)).toEqual([true, false])
  })
  it('joins hung natural cleanup and suppresses restart until its runner settles', async () => {
    let exit!: (code: number | null) => void
    const tree: SidecarTreeRunner = { signal: vi.fn(() => new Promise<void>(() => {})), isEmpty: vi.fn() }
    const exitingChild = { pid: 51, exitCode: null, signalCode: null, onExit: (listener: (code: number | null) => void) => { exit = listener; return () => {} } }
    const launcher: SidecarLauncher = { launch: vi.fn(() => ({ child: exitingChild, processGroupId: 51, dedicatedProcessGroup: true, tree, ready })) }
    const manager = new SidecarManager({ ...spec, shutdown: { termGraceMs: 5, killAfterMs: 5 } }, launcher, { check: vi.fn().mockResolvedValue(true) })
    await manager.start()
    exit(0)
    const first = manager.stop()
    const second = manager.stop()
    await Promise.all([expect(first).rejects.toThrow('signal timed out'), expect(second).rejects.toThrow('signal timed out')])
    expect(tree.signal).toHaveBeenCalledOnce()
    await expect(manager.start()).resolves.toMatchObject({ state: 'failed', instanceId: expect.any(String) })
    expect(launcher.launch).toHaveBeenCalledOnce()
    await expect(manager.stop()).rejects.toThrow('operation is still pending')
  })
  it('isolates an old exit callback from a newer generation', async () => {
    let oldExit!: (code: number | null) => void
    const oldChild = { pid: 46, exitCode: null, signalCode: null, onExit: (listener: (code: number | null) => void) => { oldExit = listener; return () => {} } }
    const newChild = childFor(47)
    const handoff = new MemoryEndpointHandoff()
    const launcher: SidecarLauncher = { launch: vi.fn().mockReturnValueOnce({ child: oldChild, processGroupId: 46, dedicatedProcessGroup: true, ready }).mockReturnValueOnce({ child: newChild, processGroupId: 47, dedicatedProcessGroup: true, ready }) }
    const manager = new SidecarManager(spec, launcher, { check: vi.fn().mockResolvedValue(true) }, { stop: vi.fn().mockResolvedValue(undefined) }, handoff)
    await manager.start(); await manager.stop(); await manager.start()
    const current = handoff.current()
    oldExit(0)
    expect(handoff.current()).toEqual(current)
    expect(manager.status()).toMatchObject({ state: 'healthy', instanceId: current?.instanceId })
    await manager.stop()
  })
})
