import { describe, expect, it, vi } from 'vitest'
import type { execFile } from 'node:child_process'
import { SERVICE_LABEL, upgradeByLaunchd, upgradeService, type UpgradeDeps } from './resident-service'

/** A fake `execFile` that records each launchctl call and fails `fail`'s verb. */
function launchctl(fail?: string) {
  const calls: string[][] = []
  const run = ((_cmd: string, args: string[], _opts: unknown, cb: (err: Error | null) => void) => {
    calls.push(args)
    cb(args[0] === fail ? new Error(`${fail} failed`) : null)
  }) as unknown as typeof execFile
  return { calls, run }
}

function base(): UpgradeDeps & { shutdown: ReturnType<typeof vi.fn>; copy: ReturnType<typeof vi.fn> } {
  return {
    shutdown: vi.fn(async () => ({ status: 200 })),
    exited: vi.fn(async () => true),
    copy: vi.fn(() => 'copied' as const),
    kickstart: vi.fn(async () => {}),
    started: vi.fn(async () => true),
    hold: (work) => work(),
  }
}

describe('U1 option C: the upgrade stops and starts the service through launchd', () => {
  it('boots the job out (no elevated /shutdown, so no presence box), copies, then bootstraps its plist', async () => {
    const { calls, run } = launchctl()
    const deps = base()
    const out = await upgradeService(upgradeByLaunchd(deps, { uid: 501, plist: '/Users/u/Library/LaunchAgents/x.plist', run }))
    expect(out).toEqual({ ok: true, copied: true })
    expect(deps.shutdown).not.toHaveBeenCalled()
    expect(calls).toEqual([
      ['bootout', `gui/501/${SERVICE_LABEL}`],
      ['bootstrap', 'gui/501', '/Users/u/Library/LaunchAgents/x.plist'],
    ])
  })

  it('a failed bootout is reported, and nothing is replaced or started', async () => {
    const { calls, run } = launchctl('bootout')
    const deps = base()
    const out = await upgradeService(upgradeByLaunchd(deps, { uid: 501, plist: '/p', run }))
    expect(out).toEqual({ ok: false, stage: 'shutdown', code: 'bootout_failed' })
    expect(deps.copy).not.toHaveBeenCalled()
    expect(calls).toEqual([['bootout', `gui/501/${SERVICE_LABEL}`]])
  })

  it('a failed bootstrap is reported as the start failing', async () => {
    const { run } = launchctl('bootstrap')
    const out = await upgradeService(upgradeByLaunchd(base(), { uid: 501, plist: '/p', run }))
    expect(out).toEqual({ ok: false, stage: 'kickstart', code: 'kickstart_failed' })
  })
})
