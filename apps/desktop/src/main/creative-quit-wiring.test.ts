import { afterEach, describe, expect, it, vi } from 'vitest'
import { createCreativeShutdown } from './creative-quit'
import { CREATIVE_QUIT_STOP_BUDGET_MS } from './creative-serve-web'
import { OPEN_DESIGN_KILL_AFTER_MS, OPEN_DESIGN_TERM_GRACE_MS } from './open-design-sidecar'

afterEach(() => { vi.useRealTimers(); vi.restoreAllMocks() })
function fixture(stop = vi.fn(async () => {})) {
  const deps = { prepare: vi.fn(), stop, cleanup: vi.fn(), quit: vi.fn(), exit: vi.fn() }
  return { deps, shutdown: createCreativeShutdown(deps) }
}
describe('N4 shared shutdown', () => {
  it('keeps the 22 second budget above the process tree shutdown budget', () => {
    expect(CREATIVE_QUIT_STOP_BUDGET_MS).toBe(22_000)
    expect(CREATIVE_QUIT_STOP_BUDGET_MS).toBeGreaterThan(OPEN_DESIGN_TERM_GRACE_MS + OPEN_DESIGN_KILL_AFTER_MS)
  })
  it.each([['SIGINT', 130], ['SIGTERM', 143], ['SIGHUP', 129]] as const)('cleans up before exiting on %s', async (signal, code) => {
    const { deps, shutdown } = fixture()
    shutdown.signal(signal)
    await vi.waitFor(() => expect(deps.exit).toHaveBeenCalledExactlyOnceWith(code))
    expect(deps.prepare).toHaveBeenCalledOnce()
    expect(deps.stop).toHaveBeenCalledOnce()
    expect(deps.cleanup).toHaveBeenCalledOnce()
    expect(deps.prepare.mock.invocationCallOrder[0]).toBeLessThan(deps.stop.mock.invocationCallOrder[0])
    expect(deps.stop.mock.invocationCallOrder[0]).toBeLessThan(deps.cleanup.mock.invocationCallOrder[0])
    expect(deps.cleanup.mock.invocationCallOrder[0]).toBeLessThan(deps.exit.mock.invocationCallOrder[0])
  })
  it('bounds a hanging stop and then runs cleanup', async () => {
    vi.useFakeTimers()
    const { deps, shutdown } = fixture(vi.fn(() => new Promise<void>(() => {})))
    shutdown.signal('SIGTERM')
    await vi.advanceTimersByTimeAsync(21_999)
    expect(deps.cleanup).not.toHaveBeenCalled()
    expect(deps.exit).not.toHaveBeenCalled()
    await vi.advanceTimersByTimeAsync(1)
    expect(deps.cleanup).toHaveBeenCalledOnce()
    expect(deps.exit).toHaveBeenCalledWith(143)
  })
  it('a second signal exits immediately with the first signal code', () => {
    const { deps, shutdown } = fixture(vi.fn(() => new Promise<void>(() => {})))
    shutdown.signal('SIGHUP')
    shutdown.signal('SIGINT')
    expect(deps.exit).toHaveBeenCalledExactlyOnceWith(129)
    expect(deps.cleanup).not.toHaveBeenCalled()
  })
  it.each(['beforeQuit', 'willQuit'] as const)('%s shares a single stop and cleanup with signals', async (entry) => {
    const { deps, shutdown } = fixture()
    const event = { preventDefault: vi.fn() }
    shutdown[entry](event)
    shutdown.beforeQuit(event)
    shutdown.signal('SIGINT')
    await vi.waitFor(() => expect(deps.exit).toHaveBeenCalledWith(130))
    shutdown.willQuit(event)
    expect(event.preventDefault).toHaveBeenCalledTimes(2)
    expect(deps.prepare).toHaveBeenCalledOnce()
    expect(deps.stop).toHaveBeenCalledOnce()
    expect(deps.cleanup).toHaveBeenCalledOnce()
    expect(deps.quit).not.toHaveBeenCalled()
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
    shutdown.signal('SIGTERM')
    await vi.waitFor(() => expect(deps.exit).toHaveBeenCalledWith(143))
    expect(deps.cleanup).toHaveBeenCalledOnce()
  })
  it('still stops, cleans up and exits when prepare throws', async () => {
    const { deps, shutdown } = fixture()
    deps.prepare.mockImplementation(() => { throw new Error('prepare failed') })
    vi.spyOn(console, 'error').mockImplementation(() => {})
    shutdown.signal('SIGTERM')
    await vi.waitFor(() => expect(deps.exit).toHaveBeenCalledWith(143))
    expect(deps.stop).toHaveBeenCalledOnce()
    expect(deps.cleanup).toHaveBeenCalledOnce()
  })
  it('still exits when cleanup throws', async () => {
    const { deps, shutdown } = fixture()
    deps.cleanup.mockImplementation(() => { throw new Error('cleanup failed') })
    shutdown.signal('SIGINT')
    await vi.waitFor(() => expect(deps.exit).toHaveBeenCalledWith(130))
  })
  it('warns only when stop does not finish cleanly', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const ok = fixture()
    ok.shutdown.signal('SIGTERM')
    await vi.waitFor(() => expect(ok.deps.exit).toHaveBeenCalledWith(143))
    expect(warn).not.toHaveBeenCalled()
    const failed = fixture(vi.fn(async () => { throw new Error('stop failed') }))
    failed.shutdown.signal('SIGTERM')
    await vi.waitFor(() => expect(failed.deps.exit).toHaveBeenCalledWith(143))
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
