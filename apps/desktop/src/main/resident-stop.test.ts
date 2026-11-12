// M5 B4c: the tray's 「完全退出」: stopping the service for good (resident-service.ts).
import { afterEach, describe, expect, it } from 'vitest'
import { serviceStopDeps, stopService, type StopDeps, type StopSystem } from './resident-service'
import { buildTrayView, type TrayInput } from './tray-menu'
import { mt, resetMainLocaleForTest, setMainLocale } from './main-i18n'

afterEach(() => { resetMainLocaleForTest() })

function fakeStop(o: {
  enrolled?: boolean
  shutdown?: () => Promise<{ status: number; code?: string }>
  exited?: boolean
  uninstall?: () => Promise<void>
  record?: () => void
  unreachable?: boolean
  afterUninstall?: () => Promise<void>
} = {}) {
  const calls: string[] = []
  let held = false
  const deps: StopDeps = {
    shutdown: o.enrolled === false ? null : async () => {
      calls.push(`shutdown held=${held}`)
      return o.shutdown ? o.shutdown() : { status: 202 }
    },
    unreachable: () => o.unreachable ?? false,
    exited: async () => { calls.push(`exited held=${held}`); return o.exited ?? true },
    uninstall: async () => { calls.push(`uninstall held=${held}`); await o.uninstall?.() },
    hold: async (work) => {
      held = true
      calls.push('hold')
      try { return await work() } finally { held = false; calls.push('release') }
    },
    record: () => { calls.push(`record held=${held}`); o.record?.() },
    ...(o.afterUninstall ? {
      afterUninstall: async () => { calls.push(`afterUninstall held=${held}`); await o.afterUninstall!() },
    } : {}),
  }
  return { deps, calls }
}

describe('stopService', () => {
  it('enrolled: shutdown, wait for the exit, uninstall, then record, all inside the hold', async () => {
    const { deps, calls } = fakeStop()
    expect(await stopService(deps)).toEqual({ ok: true })
    expect(calls).toEqual(['hold', 'shutdown held=true', 'exited held=true', 'uninstall held=true', 'record held=true', 'release'])
  })

  it('a refused presence box changes nothing: no uninstall, no record (PM r1 ①)', async () => {
    const { deps, calls } = fakeStop({ shutdown: async () => ({ status: 403, code: 'presence_denied' }) })
    expect(await stopService(deps)).toEqual({ ok: false, stage: 'shutdown', code: 'presence_denied' })
    expect(calls).toEqual(['hold', 'shutdown held=true', 'release'])
  })

  it('a refusal without a code is named by its status', async () => {
    const { deps } = fakeStop({ shutdown: async () => ({ status: 504 }) })
    expect(await stopService(deps)).toEqual({ ok: false, stage: 'shutdown', code: 'http_504' })
  })

  it('only a 2xx is a yes: a redirect or an informational status changes nothing', async () => {
    for (const status of [101, 302]) {
      const { deps, calls } = fakeStop({ shutdown: async () => ({ status }) })
      expect(await stopService(deps)).toEqual({ ok: false, stage: 'shutdown', code: `http_${status}` })
      expect(calls).toEqual(['hold', 'shutdown held=true', 'release'])
    }
  })

  it('a shutdown that throws (a timed-out box, no connection) changes nothing', async () => {
    const { deps, calls } = fakeStop({ shutdown: () => Promise.reject(new Error('timeout')) })
    expect(await stopService(deps)).toEqual({ ok: false, stage: 'shutdown', code: 'shutdown_failed' })
    expect(calls).toEqual(['hold', 'shutdown held=true', 'release'])
  })

  it('a daemon that did not exit in time is still uninstalled: the bootout stops it', async () => {
    const { deps, calls } = fakeStop({ exited: false })
    expect(await stopService(deps)).toEqual({ ok: true })
    expect(calls).toEqual(['hold', 'shutdown held=true', 'exited held=true', 'uninstall held=true', 'record held=true', 'release'])
  })

  it('not enrolled: nothing to ask, so straight to uninstall and record', async () => {
    const { deps, calls } = fakeStop({ enrolled: false })
    expect(await stopService(deps)).toEqual({ ok: true })
    expect(calls).toEqual(['hold', 'uninstall held=true', 'record held=true', 'release'])
  })

  it('not enrolled, yet a live service daemon on record (the tray\'s disconnect): not stopped without the presence box (Codex B4c1 C1)', async () => {
    const { deps, calls } = fakeStop({ enrolled: false, unreachable: true })
    expect(await stopService(deps)).toEqual({ ok: false, stage: 'shutdown', code: 'not_connected' })
    expect(calls).toEqual(['hold', 'release'])
  })

  it('enrolled, the presence box is asked whatever the record says', async () => {
    const { deps, calls } = fakeStop({ unreachable: true })
    expect(await stopService(deps)).toEqual({ ok: true })
    expect(calls[1]).toBe('shutdown held=true')
  })

  it('a failed uninstall is reported and nothing is recorded', async () => {
    const { deps, calls } = fakeStop({ uninstall: () => Promise.reject(Object.assign(new Error('rm'), { code: 'EACCES' })) })
    expect(await stopService(deps)).toEqual({ ok: false, stage: 'uninstall', code: 'EACCES' })
    expect(calls).not.toContain('record held=true')
    expect(calls.at(-1)).toBe('release')
  })

  it('an uninstall failing with an exit status is named by it', async () => {
    const { deps } = fakeStop({ uninstall: () => Promise.reject(Object.assign(new Error('exit'), { code: 2 })) })
    expect(await stopService(deps)).toEqual({ ok: false, stage: 'uninstall', code: 'exit_2' })
  })

  it('a failed record is ignored: the service is stopped all the same', async () => {
    const { deps } = fakeStop({ record: () => { throw new Error('disk full') } })
    expect(await stopService(deps)).toEqual({ ok: true })
  })

  // PR#1 F1 follow-up (ruling 2026-11-13-pr1-omlx-orphan-ownership, 乙): the
  // oMLX a quit left running is stopped only once the LaunchAgent is removed.
  it('stops what a quit left running only after the uninstall, inside the hold', async () => {
    const { deps, calls } = fakeStop({ afterUninstall: async () => {} })
    expect(await stopService(deps)).toEqual({ ok: true })
    expect(calls).toEqual(['hold', 'shutdown held=true', 'exited held=true', 'uninstall held=true', 'afterUninstall held=true', 'record held=true', 'release'])
  })

  it('stops nothing that a quit left running when the uninstall fails (the plist stays)', async () => {
    const { deps, calls } = fakeStop({ afterUninstall: async () => {}, uninstall: () => Promise.reject(Object.assign(new Error('rm'), { code: 'EACCES' })) })
    expect(await stopService(deps)).toEqual({ ok: false, stage: 'uninstall', code: 'EACCES' })
    expect(calls).not.toContain('afterUninstall held=true')
  })

  it('stops nothing that a quit left running when the shutdown is refused or the daemon is unreachable', async () => {
    for (const o of [{ shutdown: async () => ({ status: 403, code: 'presence_denied' }) }, { enrolled: false, unreachable: true }]) {
      const { deps, calls } = fakeStop({ afterUninstall: async () => {}, ...o })
      expect((await stopService(deps)).ok).toBe(false)
      expect(calls).not.toContain('afterUninstall held=true')
    }
  })

  it('a failure stopping what a quit left running is ignored: the service is stopped all the same', async () => {
    const { deps, calls } = fakeStop({ afterUninstall: () => Promise.reject(new Error('ps failed')) })
    expect(await stopService(deps)).toEqual({ ok: true })
    expect(calls).toContain('record held=true')
  })
})

describe('serviceStopDeps (the real steps over a fake system)', () => {
  function system(pid: number | null, o: Partial<StopSystem> = {}, spawned: (pid: number) => boolean = () => false) {
    const runs: { bin: string; args: string[] }[] = []
    let shutdowns = 0
    const sys: StopSystem = {
      run: async (bin, args) => { runs.push({ bin, args }) },
      record: () => null,
      alive: () => false,
      shutdown: async () => { shutdowns++; return { status: 202 } },
      exitWaitMs: 0,
      ...o,
    }
    const hold: StopDeps['hold'] = (work) => work()
    const record = () => {}
    const deps = serviceStopDeps({ cli: '/App/backend/agent24', pid, spawned, hold, record }, sys)
    return { deps, runs, hold, record, shutdowns: () => shutdowns }
  }

  it('passes the stop of what a quit left running through as afterUninstall', async () => {
    let stops = 0
    const afterUninstall = async () => { stops++ }
    const deps = serviceStopDeps({ cli: '/App/backend/agent24', pid: null, spawned: () => false, hold: (work) => work(), record: () => {}, afterUninstall }, {
      run: async () => {}, record: () => null, alive: () => false, shutdown: async () => ({ status: 202 }), exitWaitMs: 0,
    })
    await deps.afterUninstall!()
    expect(stops).toBe(1)
    expect(serviceStopDeps({ cli: 'x', pid: null, spawned: () => false, hold: (work) => work(), record: () => {} }, {
      run: async () => {}, record: () => null, alive: () => false, shutdown: async () => ({ status: 202 }), exitWaitMs: 0,
    }).afterUninstall).toBeUndefined()
  })

  it('not enrolled: no shutdown to send', () => {
    expect(system(null).deps.shutdown).toBeNull()
  })

  it('enrolled: the shutdown is the elevated one', async () => {
    const s = system(4242)
    expect(await s.deps.shutdown!()).toEqual({ status: 202 })
    expect(s.shutdowns()).toBe(1)
  })

  it('unreachable: not enrolled, and a live daemon with an enrolment socket that this app did not spawn is on record', () => {
    const live = { record: () => ({ pid: 5151, enrollEndpoint: '/tmp/enroll.sock' }), alive: () => true }
    expect(system(null, live).deps.unreachable()).toBe(true)
  })

  it.each([
    ['enrolled (the box is asked)', 4242, {}, () => false],
    ['nothing on record (a crash loop, waiting)', null, { record: () => null, alive: () => true }, () => false],
    ['a dead daemon on record', null, { record: () => ({ pid: 5151, enrollEndpoint: '/tmp/e.sock' }), alive: () => false }, () => false],
    ['a legacy record (no enrolment socket)', null, { record: () => ({ pid: 5151 }), alive: () => true }, () => false],
    ['our own child on record', null, { record: () => ({ pid: 5151, enrollEndpoint: '/tmp/e.sock' }), alive: () => true }, () => true],
  ] as const)('not unreachable: %s', (_name, pid, o, spawned) => {
    const live = { record: () => ({ pid: 5151, enrollEndpoint: '/tmp/e.sock' }), alive: () => true }
    expect(system(pid, { ...live, ...o }, spawned).deps.unreachable()).toBe(false)
  })

  it('exited: true once the pid is gone', async () => {
    expect(await system(4242).deps.exited()).toBe(true)
  })

  it('exited: false when the pid is still alive after the wait', async () => {
    expect(await system(4242, { alive: () => true }).deps.exited()).toBe(false)
  })

  it('exited waits for the pid to go, within exitWaitMs', async () => {
    let looks = 0
    const { deps } = system(4242, { alive: () => ++looks < 3, exitWaitMs: 5_000 })
    expect(await deps.exited()).toBe(true)
    expect(looks).toBe(3)
  })

  it('exited with no pid: nothing to wait for', async () => {
    expect(await system(null, { alive: () => true }).deps.exited()).toBe(true)
  })

  it('uninstall runs the bundled CLI `service uninstall`', async () => {
    const { deps, runs } = system(4242)
    await deps.uninstall()
    expect(runs).toEqual([{ bin: '/App/backend/agent24', args: ['service', 'uninstall'] }])
  })

  it('the hold and the record are the ones given', () => {
    const s = system(4242)
    expect(s.deps.hold).toBe(s.hold)
    expect(s.deps.record).toBe(s.record)
  })
})

describe('the tray\'s 「完全退出」', () => {
  const base: TrayInput = { status: 'running', external: true, enrolled: true, externalPid: 4242, foreignPid: null, backendKind: 'rust' }
  const actions = (input: TrayInput) => buildTrayView(input).items.flatMap((item) => ('action' in item ? [item.action] : []))

  it('is offered right below 「退出 Agent24」 while the plist is there', () => {
    const list = actions({ ...base, serviceInstalled: true })
    expect(list.slice(-2)).toEqual(['quit', 'quit-service'])
  })

  it.each([['no plist', false], ['not said', undefined]])('is not offered with %s', (_name, installed) => {
    expect(actions({ ...base, serviceInstalled: installed })).not.toContain('quit-service')
  })

  it('is offered while the service daemon is not up too (a crash loop, waiting): the way out', () => {
    expect(actions({ ...base, status: 'starting', enrolled: false, external: false, crashLoop: true, serviceInstalled: true })).toContain('quit-service')
  })

  it.each([
    ['en', 'Quit Completely (Stop Background Service)'],
    ['zh', '完全退出（停止后台服务）'],
    ['th', 'ออกทั้งหมด (หยุดบริการเบื้องหลัง)'],
  ] as const)('reads %s', (locale, label) => {
    setMainLocale(locale)
    const item = buildTrayView({ ...base, serviceInstalled: true }).items.find((i) => 'action' in i && i.action === 'quit-service')
    expect(item && 'label' in item ? item.label : null).toBe(label)
    expect(mt('tray.serviceNotStopped', { code: 'presence_denied' })).toContain('presence_denied')
    // The way to connect is named as the tray names it.
    expect(mt('tray.serviceNotConnected')).toContain(mt('tray.start'))
    // And the way out when it never connects (Codex B4c1 R2 C2: a reused pid, a refused enrolment).
    expect(mt('tray.serviceNotConnected')).toContain('agent24 service uninstall')
  })
})
