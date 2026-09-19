import { describe, expect, it, vi } from 'vitest'
import { exactTreeStopper, SidecarManager, validateReady, type SidecarHealth, type SidecarLauncher, type SidecarStopper, type SidecarTreeController } from './sidecar-manager'
import { MemoryEndpointHandoff } from './sidecar-contract'

const spec = { sidecarId: 'creative', readyTimeoutMs: 50, healthIntervalMs: 100, healthTimeoutMs: 10, maxHealthFailures: 2, shutdown: { termGraceMs: 1, killAfterMs: 1 } }
const child = { pid: 42, exitCode: null, onExit: vi.fn().mockReturnValue(() => {}) }
const ready = Promise.resolve({ endpoint: { origin: 'http://127.0.0.1:4312', secret: 's'.repeat(32) } })
const tree: SidecarTreeController = { signal: vi.fn().mockResolvedValue(undefined), isEmpty: vi.fn().mockResolvedValue(true) }

describe('SidecarManager', () => {
  it('rejects non-positive or non-finite lifecycle bounds', () => {
    expect(() => new SidecarManager({ ...spec, healthTimeoutMs: 0 }, { launch: () => ({ child, tree, ready }) }, { check: vi.fn() })).toThrow('timer-safe')
    expect(() => new SidecarManager({ ...spec, healthIntervalMs: 2_147_483_648 }, { launch: () => ({ child, tree, ready }) }, { check: vi.fn() })).toThrow('timer-safe')
    expect(() => new SidecarManager({ ...spec, maxHealthFailures: Number.NaN }, { launch: () => ({ child, tree, ready }) }, { check: vi.fn() })).toThrow('failure bound')
  })

  it('rejects non-loopback or incomplete ready records before handoff', () => {
    expect(() => validateReady({ endpoint: { origin: 'http://example.com:4312', secret: 's'.repeat(32) } })).toThrow('loopback')
    expect(() => validateReady({ endpoint: { origin: 'http://127.0.0.1:4312', secret: '' } })).toThrow('secret')
    expect(() => validateReady({ endpoint: { origin: 'http://127.0.0.1:4312', secret: 'short' } })).toThrow('weak')
    expect(() => validateReady({ endpoint: { origin: 'http://127.0.0.1:4312', secret: `s${' '.repeat(31)}` } })).toThrow('weak')
  })

  it('rejects invalid child ownership and retains ownership when shutdown fails', async () => {
    const invalid = new SidecarManager(spec, { launch: () => ({ child: { pid: -1, exitCode: null } as typeof child, tree, ready }) }, { check: vi.fn() })
    await expect(invalid.start()).rejects.toThrow('valid pid')
    const missingTree = new SidecarManager(spec, { launch: () => ({ child, processGroupId: 42, dedicatedProcessGroup: true, ready } as never) }, { check: vi.fn() })
    await expect(missingTree.start()).rejects.toThrow('verified tree')
    const badGroupStop = { stop: vi.fn().mockResolvedValue(undefined) }
    const badGroup = new SidecarManager(spec, { launch: () => ({ child, processGroupId: 99, dedicatedProcessGroup: true, tree, ready }) }, { check: vi.fn() }, badGroupStop)
    await expect(badGroup.start()).rejects.toThrow('owned child group')
    expect(badGroupStop.stop).toHaveBeenCalledOnce()

    const stopper = { stop: vi.fn().mockRejectedValue(new Error('kill failed')) }
    const manager = new SidecarManager(spec, { launch: () => ({ child, dedicatedProcessGroup: true, processGroupId: 42, tree, ready }) }, { check: vi.fn().mockResolvedValue(true) }, stopper)
    await manager.start()
    await expect(manager.stop()).rejects.toThrow('kill failed')
    expect(manager.status().state).toBe('failed')
    await expect(manager.stop()).rejects.toThrow('kill failed')
    expect(stopper.stop).toHaveBeenCalledTimes(2)
  })

  it('converts health rejection into bounded degradation', async () => {
    const launcher: SidecarLauncher = { launch: () => ({ child, processGroupId: 42, dedicatedProcessGroup: true, tree, ready }) }
    const manager = new SidecarManager({ ...spec, maxHealthFailures: 1 }, launcher, { check: () => Promise.reject(new Error('probe failed')) }, { stop: vi.fn().mockResolvedValue(undefined) })
    await expect(manager.start()).resolves.toMatchObject({ state: 'degraded' })
    await manager.stop()
  })

  it('does not let a late probe rejection overwrite an exited generation', async () => {
    let exit!: (code: number | null) => void
    let rejectProbe!: (error: Error) => void
    const exitingChild = { pid: 45, exitCode: null, onExit: (listener: (code: number | null) => void) => { exit = listener; return () => {} } }
    const pendingProbe = new Promise<boolean>((_resolve, reject) => { rejectProbe = reject })
    const health: SidecarHealth = { check: vi.fn().mockResolvedValueOnce(true).mockReturnValueOnce(pendingProbe) }
    const retainedTree: SidecarTreeController = { signal: vi.fn().mockResolvedValue(undefined), isEmpty: vi.fn().mockResolvedValue(false) }
    const manager = new SidecarManager({ ...spec, healthIntervalMs: 5 }, { launch: () => ({ child: exitingChild, processGroupId: 45, dedicatedProcessGroup: true, tree: retainedTree, ready }) }, health, { stop: vi.fn().mockResolvedValue(undefined) })
    await manager.start()
    await new Promise((resolve) => setTimeout(resolve, 10))
    exit(1)
    rejectProbe(new Error('late failure'))
    await new Promise((resolve) => setTimeout(resolve, 5))
    expect(manager.status().state).toBe('failed')
  })

  it('does not install an interval when the child exits during initial health', async () => {
    let exit!: (code: number | null) => void
    let resolveHealth!: (value: boolean) => void
    const exitingChild = { pid: 46, exitCode: null, onExit: (listener: (code: number | null) => void) => { exit = listener; return () => {} } }
    const initialHealth = new Promise<boolean>((resolve) => { resolveHealth = resolve })
    const health: SidecarHealth = { check: vi.fn().mockReturnValue(initialHealth) }
    const manager = new SidecarManager({ ...spec, healthIntervalMs: 2 }, { launch: () => ({ child: exitingChild, processGroupId: 46, dedicatedProcessGroup: true, tree, ready }) }, health, { stop: vi.fn().mockResolvedValue(undefined) })
    const started = manager.start()
    await new Promise((resolve) => setTimeout(resolve, 0))
    exit(0)
    resolveHealth(true)
    await expect(started).rejects.toThrow('initial health')
    await new Promise((resolve) => setTimeout(resolve, 8))
    expect(health.check).toHaveBeenCalledTimes(1)
  })

  it('uses graceful then forced signals for an owned process group', async () => {
    let forced = false
    const nonEmpty: SidecarTreeController = { signal: vi.fn().mockImplementation((force) => { forced = force; return Promise.resolve() }), isEmpty: vi.fn().mockImplementation(() => Promise.resolve(forced)) }
    await exactTreeStopper.stop(nonEmpty, 1, 1)
    expect(nonEmpty.signal).toHaveBeenNthCalledWith(1, false)
    expect(nonEmpty.signal).toHaveBeenNthCalledWith(2, true)
  })

  it('requires readiness and health before reporting healthy, then stops its exact owner', async () => {
    const launcher: SidecarLauncher = { launch: () => ({ child, processGroupId: 42, dedicatedProcessGroup: true, tree, ready }) }
    const health: SidecarHealth = { check: vi.fn().mockResolvedValue(true) }
    const stopper: SidecarStopper = { stop: vi.fn().mockResolvedValue(undefined) }
    const logger = { info: vi.fn(), warn: vi.fn() }
    const manager = new SidecarManager(spec, launcher, health, stopper, undefined, logger)

    await expect(manager.start()).resolves.toMatchObject({ state: 'healthy', sidecarId: 'creative' })
    expect(logger.info).toHaveBeenCalledWith('sidecar.ready', expect.objectContaining({ sidecarId: 'creative' }))
    expect(JSON.stringify(logger.info.mock.calls)).not.toContain('secret')
    await manager.stop()
    expect(stopper.stop).toHaveBeenCalledWith(tree, 1, 1)
    expect(manager.status().state).toBe('stopped')
  })

  it('clears the generation on natural child exit so stop cannot target it later', async () => {
    let exit!: (code: number | null) => void
    const exitingChild = { pid: 43, exitCode: null, onExit: (listener: (code: number | null) => void) => { exit = listener; return () => {} } }
    const stopper = { stop: vi.fn().mockResolvedValue(undefined) }
    const exitedTree: SidecarTreeController = { signal: vi.fn().mockResolvedValue(undefined), isEmpty: vi.fn().mockResolvedValue(true) }
    const manager = new SidecarManager(spec, { launch: () => ({ child: exitingChild, processGroupId: 43, dedicatedProcessGroup: true, tree: exitedTree, ready }) }, { check: vi.fn().mockResolvedValue(true) }, stopper)
    await manager.start()
    exit(0)
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(manager.status()).toMatchObject({ state: 'failed', instanceId: undefined })
    await manager.stop()
    expect(stopper.stop).not.toHaveBeenCalled()
  })

  it('clears handoff immediately when descendant reap needs retry', async () => {
    let exit!: (code: number | null) => void
    const exitingChild = { pid: 44, exitCode: null, onExit: (listener: (code: number | null) => void) => { exit = listener; return () => {} } }
    const exitedTree: SidecarTreeController = { signal: vi.fn().mockResolvedValue(undefined), isEmpty: vi.fn().mockResolvedValue(false) }
    const handoff = new MemoryEndpointHandoff()
    const manager = new SidecarManager(spec, { launch: () => ({ child: exitingChild, processGroupId: 44, dedicatedProcessGroup: true, tree: exitedTree, ready }) }, { check: vi.fn().mockResolvedValue(true) }, { stop: vi.fn().mockResolvedValue(undefined) }, handoff)
    await manager.start()
    exit(0)
    await new Promise((resolve) => setTimeout(resolve, 5))
    expect(handoff.current()).toBeNull()
    expect(manager.status().state).toBe('failed')
  })

  it('bounds health failures without replacing the owned child', async () => {
    const launcher: SidecarLauncher = { launch: () => ({ child, processGroupId: 42, dedicatedProcessGroup: true, tree, ready }) }
    const health: SidecarHealth = { check: vi.fn().mockResolvedValue(false) }
    const manager = new SidecarManager(spec, launcher, health, { stop: vi.fn().mockResolvedValue(undefined) })

    await expect(manager.start()).resolves.toMatchObject({ state: 'ready' })
    await new Promise((resolve) => setTimeout(resolve, 120))
    expect(manager.status().state).toBe('degraded')
    await manager.stop()
  })

  it('serializes stop behind a pending start', async () => {
    let resolveReady!: (value: Awaited<typeof ready>) => void
    const pendingReady = new Promise<Awaited<typeof ready>>((resolve) => { resolveReady = resolve })
    const launcher: SidecarLauncher = { launch: () => ({ child, dedicatedProcessGroup: true, processGroupId: 42, tree, ready: pendingReady }) }
    const manager = new SidecarManager(spec, launcher, { check: vi.fn().mockResolvedValue(true) }, { stop: vi.fn().mockResolvedValue(undefined) })
    const started = manager.start()
    const stopped = manager.stop()
    resolveReady(await ready)
    await expect(started).resolves.toMatchObject({ state: 'healthy' })
    await expect(stopped).resolves.toMatchObject({ state: 'stopped' })
  })

  it('ignores a health result that resolves after stop begins', async () => {
    let resolveHealth!: (value: boolean) => void
    const pendingHealth = new Promise<boolean>((resolve) => { resolveHealth = resolve })
    const health: SidecarHealth = { check: vi.fn().mockResolvedValueOnce(true).mockReturnValueOnce(pendingHealth) }
    const launcher: SidecarLauncher = { launch: () => ({ child, dedicatedProcessGroup: true, processGroupId: 42, tree, ready }) }
    const manager = new SidecarManager({ ...spec, healthIntervalMs: 5 }, launcher, health, { stop: vi.fn().mockResolvedValue(undefined) })
    await manager.start()
    await new Promise((resolve) => setTimeout(resolve, 20))
    const stopping = manager.stop()
    resolveHealth(false)
    await stopping
    expect(manager.status().state).toBe('stopped')
  })
})
