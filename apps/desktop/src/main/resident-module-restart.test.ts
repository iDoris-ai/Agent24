import { describe, expect, it } from 'vitest'
import { whileServiceStopped, type UpgradeDeps } from './resident-service'

function deps(shutdownStatus = 202) {
  const calls: string[] = []
  const d: UpgradeDeps = {
    shutdown: async () => { calls.push('shutdown'); return { status: shutdownStatus, code: shutdownStatus === 202 ? undefined : 'presence_denied' } },
    exited: async () => { calls.push('exited'); return true },
    copy: () => 'same',
    kickstart: async () => { calls.push('kickstart') },
    started: async () => { calls.push('started'); return true },
    hold: async (work) => { calls.push('hold'); return work() },
  }
  return { d, calls }
}

describe('whileServiceStopped (documents module replaced in service mode)', () => {
  it('stops the service, runs the work, starts it again', async () => {
    const { d, calls } = deps()
    const out = await whileServiceStopped(d, async () => { calls.push('work'); return 'upgraded' })
    expect(out).toEqual({ ok: true, value: 'upgraded' })
    expect(calls).toEqual(['hold', 'shutdown', 'exited', 'work', 'kickstart', 'started'])
  })

  it('does nothing when the presence box is refused', async () => {
    const { d, calls } = deps(403)
    expect(await whileServiceStopped(d, async () => { calls.push('work') })).toEqual({ ok: false, code: 'presence_denied' })
    expect(calls).toEqual(['hold', 'shutdown'])
  })

  it('starts the service again even when the work throws', async () => {
    const { d, calls } = deps()
    await expect(whileServiceStopped(d, async () => { throw new Error('boom') })).rejects.toThrow('boom')
    expect(calls).toContain('kickstart')
  })
})
