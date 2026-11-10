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
} = {}) {
  const calls: string[] = []
  let held = false
  const deps: StopDeps = {
    shutdown: o.enrolled === false ? null : async () => {
      calls.push(`shutdown held=${held}`)
      return o.shutdown ? o.shutdown() : { status: 202 }
    },
    exited: async () => { calls.push(`exited held=${held}`); return o.exited ?? true },
    uninstall: async () => { calls.push(`uninstall held=${held}`); await o.uninstall?.() },
    hold: async (work) => {
      held = true
      calls.push('hold')
      try { return await work() } finally { held = false; calls.push('release') }
    },
    record: () => { calls.push(`record held=${held}`); o.record?.() },
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
})

describe('serviceStopDeps (the real steps over a fake system)', () => {
  function system(pid: number | null, o: Partial<StopSystem> = {}) {
    const runs: { bin: string; args: string[] }[] = []
    let shutdowns = 0
    const sys: StopSystem = {
      run: async (bin, args) => { runs.push({ bin, args }) },
      alive: () => false,
      shutdown: async () => { shutdowns++; return { status: 202 } },
      exitWaitMs: 0,
      ...o,
    }
    const hold: StopDeps['hold'] = (work) => work()
    const record = () => {}
    const deps = serviceStopDeps({ cli: '/App/backend/agent24', pid, hold, record }, sys)
    return { deps, runs, hold, record, shutdowns: () => shutdowns }
  }

  it('not enrolled: no shutdown to send', () => {
    expect(system(null).deps.shutdown).toBeNull()
  })

  it('enrolled: the shutdown is the elevated one', async () => {
    const s = system(4242)
    expect(await s.deps.shutdown!()).toEqual({ status: 202 })
    expect(s.shutdowns()).toBe(1)
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
  })
})
