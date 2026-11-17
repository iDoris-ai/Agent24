import { describe, expect, it, vi } from 'vitest'
import { runWhileServiceStopped, type ServicePauseDeps } from './os-package-service'

function deps(over: Partial<ServicePauseDeps> = {}): ServicePauseDeps {
  return {
    shutdown: async () => ({ status: 200 }),
    exited: async () => true,
    kickstart: async () => {},
    started: async () => true,
    ...over,
  }
}

describe('runWhileServiceStopped', () => {
  it('does not install or kickstart when the shutdown is refused', async () => {
    const work = vi.fn(async () => 'installed')
    const kickstart = vi.fn(async () => {})
    const result = await runWhileServiceStopped(deps({
      shutdown: async () => ({ status: 403, code: 'presence_denied' }),
      kickstart,
    }), work)
    expect(result).toEqual({ ok: false, stage: 'shutdown', code: 'presence_denied' })
    expect(work).not.toHaveBeenCalled()
    expect(kickstart).not.toHaveBeenCalled()
  })

  it('kickstarts without installing when the daemon does not exit', async () => {
    const work = vi.fn()
    const kickstart = vi.fn(async () => {})
    const result = await runWhileServiceStopped(deps({ exited: async () => false, kickstart }), work)
    expect(result).toEqual({ ok: false, stage: 'exit', code: 'still_running' })
    expect(work).not.toHaveBeenCalled()
    expect(kickstart).toHaveBeenCalledOnce()
  })

  it('runs the install between shutdown and kickstart', async () => {
    const order: string[] = []
    const result = await runWhileServiceStopped(deps({
      shutdown: async () => { order.push('down'); return { status: 200 } },
      kickstart: async () => { order.push('up') },
    }), async () => { order.push('install'); return 'ok' })
    expect(result).toEqual({ ok: true, value: 'ok' })
    expect(order).toEqual(['down', 'install', 'up'])
  })
})
