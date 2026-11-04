import { afterEach, describe, expect, it, vi } from 'vitest'
import { createCreativeShutdown } from './creative-quit'
import { CREATIVE_QUIT_STOP_BUDGET_MS } from './creative-serve-web'
import { OPEN_DESIGN_KILL_AFTER_MS, OPEN_DESIGN_TERM_GRACE_MS } from './open-design-sidecar'

afterEach(() => { vi.useRealTimers(); vi.restoreAllMocks() })
function fixture(stop = vi.fn(async () => {})) {
  const deps = { prepare: vi.fn(), stop, cleanup: vi.fn(), quit: vi.fn() }
  return { deps, shutdown: createCreativeShutdown(deps) }
}
describe('N4 shared shutdown', () => {
  it('keeps the 22 second budget above the process tree shutdown budget', () => {
    expect(CREATIVE_QUIT_STOP_BUDGET_MS).toBe(22_000)
    expect(CREATIVE_QUIT_STOP_BUDGET_MS).toBeGreaterThan(OPEN_DESIGN_TERM_GRACE_MS + OPEN_DESIGN_KILL_AFTER_MS)
  })
  it.each(['beforeQuit', 'willQuit'] as const)('cleans up before quitting through %s', async (entry) => {
    const { deps, shutdown } = fixture()
    shutdown[entry]({ preventDefault: vi.fn() })
    await vi.waitFor(() => expect(deps.quit).toHaveBeenCalledOnce())
    expect(deps.prepare).toHaveBeenCalledOnce()
    expect(deps.stop).toHaveBeenCalledOnce()
    expect(deps.cleanup).toHaveBeenCalledOnce()
    expect(deps.prepare.mock.invocationCallOrder[0]).toBeLessThan(deps.stop.mock.invocationCallOrder[0])
    expect(deps.stop.mock.invocationCallOrder[0]).toBeLessThan(deps.cleanup.mock.invocationCallOrder[0])
    expect(deps.cleanup.mock.invocationCallOrder[0]).toBeLessThan(deps.quit.mock.invocationCallOrder[0])
  })
  it('bounds a hanging stop and then runs cleanup', async () => {
    vi.useFakeTimers()
    const { deps, shutdown } = fixture(vi.fn(() => new Promise<void>(() => {})))
    shutdown.beforeQuit({ preventDefault: vi.fn() })
    await vi.advanceTimersByTimeAsync(21_999)
    expect(deps.cleanup).not.toHaveBeenCalled()
    expect(deps.quit).not.toHaveBeenCalled()
    await vi.advanceTimersByTimeAsync(1)
    expect(deps.cleanup).toHaveBeenCalledOnce()
    expect(deps.quit).toHaveBeenCalledOnce()
  })
  it.each(['beforeQuit', 'willQuit'] as const)('%s shares a single stop and cleanup with repeated lifecycle events', async (entry) => {
    const { deps, shutdown } = fixture()
    const event = { preventDefault: vi.fn() }
    shutdown[entry](event)
    shutdown.beforeQuit(event)
    shutdown.willQuit(event)
    await vi.waitFor(() => expect(deps.quit).toHaveBeenCalledOnce())
    shutdown.willQuit(event)
    expect(event.preventDefault).toHaveBeenCalledTimes(3)
    expect(deps.prepare).toHaveBeenCalledOnce()
    expect(deps.stop).toHaveBeenCalledOnce()
    expect(deps.cleanup).toHaveBeenCalledOnce()
    expect(deps.quit).toHaveBeenCalledOnce()
  })
  it('normal quit resumes once and completed lifecycle events do not block', async () => {
    const { deps, shutdown } = fixture()
    const event = { preventDefault: vi.fn() }
    shutdown.beforeQuit(event)
    await vi.waitFor(() => expect(deps.quit).toHaveBeenCalledOnce())
    shutdown.beforeQuit(event)
    shutdown.willQuit(event)
    expect(event.preventDefault).toHaveBeenCalledOnce()
    expect(deps.cleanup).toHaveBeenCalledOnce()
  })
  it('still cleans up and exits when stop rejects', async () => {
    const { deps, shutdown } = fixture(vi.fn(async () => { throw new Error('stop failed') }))
    shutdown.beforeQuit({ preventDefault: vi.fn() })
    await vi.waitFor(() => expect(deps.quit).toHaveBeenCalledOnce())
    expect(deps.cleanup).toHaveBeenCalledOnce()
  })
  it('still stops, cleans up and exits when prepare throws', async () => {
    const { deps, shutdown } = fixture()
    deps.prepare.mockImplementation(() => { throw new Error('prepare failed') })
    vi.spyOn(console, 'error').mockImplementation(() => {})
    shutdown.beforeQuit({ preventDefault: vi.fn() })
    await vi.waitFor(() => expect(deps.quit).toHaveBeenCalledOnce())
    expect(deps.stop).toHaveBeenCalledOnce()
    expect(deps.cleanup).toHaveBeenCalledOnce()
  })
  it('still exits when cleanup throws', async () => {
    const { deps, shutdown } = fixture()
    deps.cleanup.mockImplementation(() => { throw new Error('cleanup failed') })
    shutdown.beforeQuit({ preventDefault: vi.fn() })
    await vi.waitFor(() => expect(deps.quit).toHaveBeenCalledOnce())
  })
  it('warns only when stop does not finish cleanly', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const ok = fixture()
    ok.shutdown.beforeQuit({ preventDefault: vi.fn() })
    await vi.waitFor(() => expect(ok.deps.quit).toHaveBeenCalledOnce())
    expect(warn).not.toHaveBeenCalled()
    const failed = fixture(vi.fn(async () => { throw new Error('stop failed') }))
    failed.shutdown.beforeQuit({ preventDefault: vi.fn() })
    await vi.waitFor(() => expect(failed.deps.quit).toHaveBeenCalledOnce())
    expect(warn).toHaveBeenCalledOnce()
  })
  it('stops blocking quit after cleanup throws', async () => {
    const { deps, shutdown } = fixture()
    deps.cleanup.mockImplementation(() => { throw new Error('cleanup failed') })
    const event = { preventDefault: vi.fn() }
    shutdown.beforeQuit(event)
    await vi.waitFor(() => expect(deps.cleanup).toHaveBeenCalledOnce())
    await vi.waitFor(() => expect(deps.quit).toHaveBeenCalledOnce())
    shutdown.willQuit(event)
    expect(event.preventDefault).toHaveBeenCalledOnce()
  })
})
