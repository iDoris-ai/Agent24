import { describe, expect, it, vi } from 'vitest'
import { exactTreeStopper, SidecarManager, validateReady, type SidecarHealth, type SidecarLauncher, type SidecarStopper } from './sidecar-manager'

const spec = { sidecarId: 'creative', readyTimeoutMs: 50, healthIntervalMs: 100, healthTimeoutMs: 10, maxHealthFailures: 2, shutdown: { termGraceMs: 1, killAfterMs: 1 } }
const child = { pid: 42, exitCode: null }
const ready = Promise.resolve({ endpoint: { origin: 'http://127.0.0.1:4312', secret: 'secret' } })

describe('SidecarManager', () => {
  it('rejects non-loopback or incomplete ready records before handoff', () => {
    expect(() => validateReady({ endpoint: { origin: 'http://example.com:4312', secret: 'x' } })).toThrow('loopback')
    expect(() => validateReady({ endpoint: { origin: 'http://127.0.0.1:4312', secret: '' } })).toThrow('secret')
  })

  it('rejects invalid child ownership and retains ownership when shutdown fails', async () => {
    const invalid = new SidecarManager(spec, { launch: () => Promise.resolve({ child: { pid: -1, exitCode: null }, ready }) }, { check: vi.fn() })
    await expect(invalid.start()).rejects.toThrow('valid pid')
    const badGroupStop = { stop: vi.fn().mockResolvedValue(undefined) }
    const badGroup = new SidecarManager(spec, { launch: () => Promise.resolve({ child, processGroupId: 99, dedicatedProcessGroup: true, ready }) }, { check: vi.fn() }, badGroupStop)
    await expect(badGroup.start()).rejects.toThrow('owned child group')
    expect(badGroupStop.stop).toHaveBeenCalledOnce()

    const stopper = { stop: vi.fn().mockRejectedValue(new Error('kill failed')) }
    const manager = new SidecarManager(spec, { launch: () => Promise.resolve({ child, dedicatedProcessGroup: true, processGroupId: 42, ready }) }, { check: vi.fn().mockResolvedValue(true) }, stopper)
    await manager.start()
    await expect(manager.stop()).rejects.toThrow('kill failed')
    expect(manager.status().state).toBe('failed')
    await expect(manager.stop()).rejects.toThrow('kill failed')
    expect(stopper.stop).toHaveBeenCalledTimes(2)
  })

  it('uses graceful then forced signals for an owned process group', async () => {
    const kill = vi.spyOn(process, 'kill').mockImplementation(() => true)
    await exactTreeStopper.stop({ sidecarId: 'creative', instanceId: 'a', pid: 42, processGroupId: 42, startedAt: 0 }, child, 1, 1)
    expect(kill.mock.calls.map(([pid, signal]) => [pid, signal])).toEqual([[-42, 'SIGTERM'], [-42, 'SIGKILL']])
    kill.mockRestore()
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

  it('serializes stop behind a pending start', async () => {
    let resolveReady!: (value: Awaited<typeof ready>) => void
    const pendingReady = new Promise<Awaited<typeof ready>>((resolve) => { resolveReady = resolve })
    const launcher: SidecarLauncher = { launch: () => Promise.resolve({ child, dedicatedProcessGroup: true, processGroupId: 42, ready: pendingReady }) }
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
    const launcher: SidecarLauncher = { launch: () => Promise.resolve({ child, dedicatedProcessGroup: true, processGroupId: 42, ready }) }
    const manager = new SidecarManager({ ...spec, healthIntervalMs: 5 }, launcher, health, { stop: vi.fn().mockResolvedValue(undefined) })
    await manager.start()
    await new Promise((resolve) => setTimeout(resolve, 20))
    const stopping = manager.stop()
    resolveHealth(false)
    await stopping
    expect(manager.status().state).toBe('stopped')
  })
})
