// M5 B4c2: the Settings switch (resident-service.ts setResident) and the
// tray's message for a failed 「完全退出」 (stopFailureKey).
import { afterEach, describe, expect, it, vi } from 'vitest'
import { installedAndEnrolled, installService, residentOps, serviceInstallDeps, setResident, stopFailureKey, type ConsentAnswer, type InstallDeps, type InstallOutcome, type InstallSystem, type StopOutcome } from './resident-service'
import { mt, resetMainLocaleForTest, setMainLocale } from './main-i18n'

afterEach(() => { resetMainLocaleForTest() })

function deps(o: { installed?: boolean; install?: InstallOutcome | Error; stop?: StopOutcome | Error; record?: () => void } = {}) {
  const calls: string[] = []
  const recorded: ConsentAnswer[] = []
  return {
    calls, recorded,
    deps: {
      installed: () => o.installed ?? false,
      install: async () => { calls.push('install'); if (o.install instanceof Error) throw o.install; return o.install ?? { ok: true } },
      stop: async () => { calls.push('stop'); if (o.stop instanceof Error) throw o.stop; return o.stop ?? { ok: true } },
      record: (a: ConsentAnswer) => { recorded.push(a); o.record?.() },
    },
  }
}

describe('setResident', () => {
  it('on: installs and keeps allow', async () => {
    const d = deps()
    expect(await setResident(true, d.deps)).toEqual({ ok: true })
    expect(d.calls).toEqual(['install'])
    expect(d.recorded).toEqual(['allow'])
  })

  it('on, but the install failed: its stage and code, nothing kept', async () => {
    const d = deps({ install: { ok: false, stage: 'start', code: 'not_started' } })
    expect(await setResident(true, d.deps)).toEqual({ ok: false, stage: 'start', code: 'not_started' })
    expect(d.recorded).toEqual([])
  })

  it('on, but the install threw: named, nothing kept', async () => {
    const d = deps({ install: new Error('boom') })
    expect(await setResident(true, d.deps)).toEqual({ ok: false, stage: 'install', code: 'install_failed' })
    expect(d.recorded).toEqual([])
  })

  it('on while already installed: nothing to do', async () => {
    const d = deps({ installed: true })
    expect(await setResident(true, d.deps)).toEqual({ ok: true })
    expect(d.calls).toEqual([])
    expect(d.recorded).toEqual([])
  })

  it('off: stops (which keeps declined itself), nothing recorded here', async () => {
    const d = deps({ installed: true })
    expect(await setResident(false, d.deps)).toEqual({ ok: true })
    expect(d.calls).toEqual(['stop'])
    expect(d.recorded).toEqual([])
  })

  it.each([
    [{ ok: false, stage: 'shutdown', code: 'presence_denied' }],
    [{ ok: false, stage: 'shutdown', code: 'not_connected' }],
    [{ ok: false, stage: 'uninstall', code: 'EACCES' }],
  ] as const)('off, but the stop failed: its stage and code (%o)', async (outcome) => {
    const d = deps({ installed: true, stop: outcome })
    expect(await setResident(false, d.deps)).toEqual(outcome)
  })

  it('off, but the stop threw: named as a failed shutdown', async () => {
    const d = deps({ installed: true, stop: new Error('boom') })
    expect(await setResident(false, d.deps)).toEqual({ ok: false, stage: 'shutdown', code: 'shutdown_failed' })
  })

  it('off while not installed: nothing to do', async () => {
    const d = deps({ installed: false })
    expect(await setResident(false, d.deps)).toEqual({ ok: true })
    expect(d.calls).toEqual([])
  })

  it('a failed record after a good install is ignored: the plist says it is on', async () => {
    const d = deps({ record: () => { throw new Error('disk full') } })
    expect(await setResident(true, d.deps)).toEqual({ ok: true })
  })
})

describe('stopFailureKey (QA on B4c1)', () => {
  it.each([
    [{ stage: 'shutdown', code: 'not_connected' }, 'tray.serviceNotConnected'],
    [{ stage: 'uninstall', code: 'EACCES' }, 'tray.serviceStoppedNotRemoved'],
    [{ stage: 'shutdown', code: 'presence_denied' }, 'tray.serviceNotStopped'],
    [{ stage: 'shutdown', code: 'shutdown_failed' }, 'tray.serviceNotStopped'],
  ] as const)('%o → %s', (outcome, key) => {
    expect(stopFailureKey(outcome)).toBe(key)
  })

  it.each(['en', 'zh', 'th'] as const)('%s: stopped but not removed says so, names its code and the way out', (locale) => {
    setMainLocale(locale)
    const text = mt('tray.serviceStoppedNotRemoved', { code: 'EACCES' })
    expect(text).toContain('EACCES')
    expect(text).toContain('agent24 service uninstall')
    expect(text).not.toBe(mt('tray.serviceNotStopped', { code: 'EACCES' }))
  })
})

describe('installService looks at the plist again once the hold has begun (Codex B4c2 C1)', () => {
  function fakeInstall() {
    const calls: string[] = []
    let held = false
    const deps: InstallDeps = {
      install: async () => { calls.push(`install held=${held}`) },
      started: async () => { calls.push(`started held=${held}`); return true },
      uninstall: async () => { calls.push(`uninstall held=${held}`) },
      hold: async (work) => {
        held = true
        calls.push('hold')
        try { return await work() } finally { held = false; calls.push('release') }
      },
    }
    return { deps, calls }
  }

  it('a LaunchAgent there once the hold has begun (another install queued ahead): nothing run, nothing undone', async () => {
    const { deps, calls } = fakeInstall()
    let plist = false
    const queued: InstallDeps = { ...deps, installed: () => plist, hold: async (work) => { plist = true; return deps.hold(work) } }
    expect(await installService(queued)).toEqual({ ok: true })
    expect(calls).toEqual(['hold', 'release'])
  })

  it('no LaunchAgent once the hold has begun: installs as before', async () => {
    const { deps, calls } = fakeInstall()
    expect(await installService({ ...deps, installed: () => false })).toEqual({ ok: true })
    expect(calls).toEqual(['hold', 'install held=true', 'started held=true', 'release'])
  })

  it('the installed look is the one given', () => {
    const installed = () => true
    const sys: InstallSystem = { run: async () => {}, record: () => null, alive: () => true, spawned: () => false, startWaitMs: 0 }
    const deps = serviceInstallDeps({ cli: '/a', daemonBin: '/b', ready: Promise.resolve(), hold: (w) => w(), spawned: () => false, installed }, sys)
    expect(deps.installed).toBe(installed)
  })
})

describe('residentOps: the install and the stop one at a time (PR-Daemon #1783 N1/N2)', () => {
  function gate<T>() {
    let open: (v: T) => void = () => {}
    let fail: (e: unknown) => void = () => {}
    const p = new Promise<T>((resolve, reject) => { open = resolve; fail = reject })
    return { p, open, fail }
  }
  const stopped: StopOutcome = { ok: true }
  const installed: InstallOutcome = { ok: true }

  it('a stop asked while one runs gets that one\'s outcome: one presence box, one uninstall', async () => {
    const op = residentOps()
    const g = gate<StopOutcome>()
    const work = vi.fn(() => g.p)
    const a = op('stop', work)
    const b = op('stop', work)
    await Promise.resolve()
    expect(work).toHaveBeenCalledTimes(1)
    g.open(stopped)
    expect(await a).toBe(stopped)
    expect(await b).toBe(stopped)
    expect(work).toHaveBeenCalledTimes(1)
  })

  it('the other kind waits its turn, and its work is only called then', async () => {
    const op = residentOps()
    const calls: string[] = []
    const g = gate<InstallOutcome>()
    const a = op('install', () => { calls.push('install'); return g.p })
    const b = op('stop', async () => { calls.push('stop'); return stopped })
    await new Promise((r) => setTimeout(r, 0))
    expect(calls).toEqual(['install'])
    g.open(installed)
    await a
    expect(await b).toBe(stopped)
    expect(calls).toEqual(['install', 'stop'])
  })

  it('once one has ended, the same kind runs again', async () => {
    const op = residentOps()
    const work = vi.fn(async () => stopped)
    await op('stop', work)
    await op('stop', work)
    expect(work).toHaveBeenCalledTimes(2)
  })

  it('one that rejects frees its kind and does not hold up the next', async () => {
    const op = residentOps()
    const g = gate<InstallOutcome>()
    const a = op('install', () => g.p)
    const b = op('stop', async () => stopped)
    g.fail(new Error('boom'))
    await expect(a).rejects.toThrow('boom')
    expect(await b).toBe(stopped)
    expect(await op('install', async () => installed)).toBe(installed)
  })
})

describe('installedAndEnrolled (Codex B4c3 C2)', () => {
  it('an install is done once this app is enrolled with the new service daemon', async () => {
    let enrolled = false
    let done = false
    const run = installedAndEnrolled(async () => ({ ok: true }), () => enrolled, 5_000).then((o) => { done = true; return o })
    await new Promise((r) => setTimeout(r, 300))
    expect(done).toBe(false)
    enrolled = true
    expect(await run).toEqual({ ok: true })
  })

  it('not enrolled in time: done anyway, as installed', async () => {
    expect(await installedAndEnrolled(async () => ({ ok: true }), () => false, 50)).toEqual({ ok: true })
  })

  it('a failed install is not waited on', async () => {
    const enrolled = vi.fn(() => false)
    const failed: InstallOutcome = { ok: false, stage: 'install', code: 'ENOENT' }
    expect(await installedAndEnrolled(async () => failed, enrolled, 60_000)).toBe(failed)
    expect(enrolled).not.toHaveBeenCalled()
  })
})
