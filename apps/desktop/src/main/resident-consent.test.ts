// Resident service auto-install and `agent24 service install`.
import { afterEach, describe, expect, it } from 'vitest'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import {
  CONSENT_VERSION, consentPath, installService, publicInstallReason, readConsent, autoInstallService, serviceInstallDeps, shouldAutoInstall, writeConsent,
  type ConsentAnswer, type InstallDeps, type InstallOutcome, type InstallSystem,
} from './resident-service'

let root = ''
afterEach(() => {
  if (root) fs.rmSync(root, { recursive: true, force: true })
  root = ''
})
const dataDir = () => (root = fs.mkdtempSync(path.join(os.tmpdir(), 'a24b4b-')))

describe('the consent file', () => {
  it('is <data_dir>/desktop/resident-consent.json', () => {
    expect(consentPath('/d')).toBe(path.join('/d', 'desktop', 'resident-consent.json'))
  })

  it('reads back what was written: owner-only, in a directory it creates, with no temp file left and nothing but asked/answer/time (N4)', () => {
    const file = consentPath(dataDir())
    writeConsent(file, 'declined', new Date('2026-11-10T08:00:00Z'))
    expect(readConsent(file)).toEqual({ version: CONSENT_VERSION, answer: 'declined', at: '2026-11-10T08:00:00.000Z' })
    expect(fs.statSync(file).mode & 0o777).toBe(0o600)
    expect(fs.readdirSync(path.dirname(file))).toEqual(['resident-consent.json'])
    expect(Object.keys(JSON.parse(fs.readFileSync(file, 'utf8'))).sort()).toEqual(['answer', 'at', 'version'])
  })

  it('a later answer replaces the kept one', () => {
    const file = consentPath(dataDir())
    writeConsent(file, 'declined', new Date('2026-11-10T08:00:00Z'))
    writeConsent(file, 'allow', new Date('2026-11-10T09:00:00Z'))
    expect(readConsent(file)).toEqual({ version: CONSENT_VERSION, answer: 'allow', at: '2026-11-10T09:00:00.000Z' })
  })

  it.each([
    ['not JSON', '{'],
    ['JSON null', 'null'],
    ['an array', '[]'],
    ['another version', JSON.stringify({ version: 2, answer: 'allow', at: '2026-11-10T08:00:00.000Z' })],
    ['an unknown answer', JSON.stringify({ version: 1, answer: 'maybe', at: '2026-11-10T08:00:00.000Z' })],
    ['no time', JSON.stringify({ version: 1, answer: 'allow' })],
    ['a time that is not a string', JSON.stringify({ version: 1, answer: 'allow', at: 5 })],
  ])('%s is no answer: asked again', (_name, text) => {
    const file = consentPath(dataDir())
    fs.mkdirSync(path.dirname(file), { recursive: true })
    fs.writeFileSync(file, text)
    expect(readConsent(file)).toBeNull()
  })

  it('a missing file is no answer', () => {
    expect(readConsent(consentPath(dataDir()))).toBeNull()
  })
})

describe('shouldAutoInstall', () => {
  const ask = { platform: 'darwin' as NodeJS.Platform, fromBundle: true, installed: false, consent: null }
  const kept = (answer: ConsentAnswer) => ({ version: CONSENT_VERSION, answer, at: '2026-11-10T08:00:00.000Z' })
  it.each([['no previous answer', null], ['previous allow', { version: CONSENT_VERSION, answer: 'allow' as const, at: '2026-11-10T08:00:00.000Z' }]])('%s installs automatically', (_name, consent) => {
    expect(shouldAutoInstall({ ...ask, consent })).toBe(true)
  })
  it.each([
    ['Linux', { ...ask, platform: 'linux' as NodeJS.Platform }],
    ['Windows', { ...ask, platform: 'win32' as NodeJS.Platform }],
    ['a dev build or an AGENT24D_BIN override', { ...ask, fromBundle: false }],
    ['a LaunchAgent already there', { ...ask, installed: true }],
    ['declined before (CLI service uninstall)', { ...ask, consent: kept('declined') }],
  ])('%s does not ask', (_name, o) => {
    expect(shouldAutoInstall(o)).toBe(false)
  })
})

function fakeInstall(o: { install?: () => Promise<void>; started?: boolean; uninstall?: () => Promise<void> } = {}) {
  const calls: string[] = []
  let held = false
  const deps: InstallDeps = {
    install: async () => { calls.push(`install held=${held}`); await o.install?.() },
    started: async () => { calls.push(`started held=${held}`); return o.started ?? true },
    uninstall: async () => { calls.push(`uninstall held=${held}`); await o.uninstall?.() },
    hold: async (work) => {
      held = true
      calls.push('hold')
      try { return await work() } finally { held = false; calls.push('release') }
    },
  }
  return { deps, calls }
}

describe('installService', () => {
  it('installs and waits for the service daemon, all inside the hold; nothing is uninstalled', async () => {
    const { deps, calls } = fakeInstall()
    expect(await installService(deps)).toEqual({ ok: true })
    expect(calls).toEqual(['hold', 'install held=true', 'started held=true', 'release'])
  })

  it('a failed install is uninstalled once, inside the hold, and not waited for', async () => {
    const { deps, calls } = fakeInstall({ install: () => Promise.reject(Object.assign(new Error('no cli'), { code: 'ENOENT' })) })
    expect(await installService(deps)).toEqual({ ok: false, stage: 'install', code: 'ENOENT' })
    expect(calls).toEqual(['hold', 'install held=true', 'uninstall held=true', 'release'])
  })

  it.each([
    ['an exit status', Object.assign(new Error('exit'), { code: 1 }), 'exit_1'],
    ['no code (a timeout kill)', Object.assign(new Error('killed'), { code: null, killed: true }), 'install_failed'],
    ['not an Error', 'boom', 'install_failed'],
    ['stderr first line', Object.assign(new Error('exit'), { code: 1, stderr: 'Bootstrap failed: 5: Input/output error\nTry again: 1\n' }), 'exit_1: Bootstrap failed: 5: Input/output error'],
    ['stderr as a buffer', Object.assign(new Error('exit'), { code: 1, stderr: Buffer.from('/Users/tester/Library/LaunchAgents/ai.auraai.agent24.plist: Operation not permitted\n') }), 'exit_1: /Users/tester/Library/LaunchAgents/ai.auraai.agent24.plist: Operation not permitted'],
  ])('an install failing with %s is named by it', async (_name, err, code) => {
    const { deps } = fakeInstall({ install: () => Promise.reject(err) })
    expect(await installService(deps)).toEqual({ ok: false, stage: 'install', code })
  })

  it('a service daemon not on record in time is uninstalled once: no half-installed plist is left (PM r1 ②)', async () => {
    const { deps, calls } = fakeInstall({ started: false })
    expect(await installService(deps)).toEqual({ ok: false, stage: 'start', code: 'not_started' })
    expect(calls).toEqual(['hold', 'install held=true', 'started held=true', 'uninstall held=true', 'release'])
  })

  it('an uninstall that fails as well still reports the first failure, and the hold still ends', async () => {
    const { deps, calls } = fakeInstall({ started: false, uninstall: () => Promise.reject(new Error('bootout')) })
    expect(await installService(deps)).toEqual({ ok: false, stage: 'start', code: 'not_started' })
    expect(calls.at(-1)).toBe('release')
  })
})

describe('autoInstallService', () => {
  function consent(install: InstallOutcome | Error = { ok: true }) {
    const recorded: ConsentAnswer[] = []
    let installs = 0
    const run = autoInstallService({
      install: () => { installs++; return install instanceof Error ? Promise.reject(install) : Promise.resolve(install) },
      record: () => { recorded.push('allow') },
    })
    return { run, recorded, installs: () => installs }
  }

  it('installs automatically and keeps allow on success', async () => {
    const c = consent()
    expect(await c.run).toBe('installed')
    expect(c.recorded).toEqual(['allow'])
    expect(c.installs()).toBe(1)
  })

  it('a failed install is not kept, so the next launch retries', async () => {
    const c = consent({ ok: false, stage: 'start', code: 'not_started' })
    expect(await c.run).toEqual({ failed: 'not_started' })
    expect(c.recorded).toEqual([])
  })

  it('a thrown install failure is not kept', async () => {
    const c = consent(new Error('unexpected'))
    expect(await c.run).toEqual({ failed: 'install_failed' })
    expect(c.recorded).toEqual([])
  })

})

describe('serviceInstallDeps (the real steps over a fake system)', () => {
  function system(o: Partial<InstallSystem> = {}, ready: Promise<unknown> = Promise.resolve()) {
    const runs: { bin: string; args: string[]; env: NodeJS.ProcessEnv }[] = []
    const sys: InstallSystem = {
      run: async (bin, args, env) => { runs.push({ bin, args, env }) },
      record: () => ({ pid: 4242, enrollEndpoint: '/tmp/enroll.sock' }),
      alive: () => true,
      spawned: () => false,
      startWaitMs: 0,
      ...o,
    }
    const hold: InstallDeps['hold'] = (work) => work()
    const deps = serviceInstallDeps({ cli: '/App/backend/agent24', daemonBin: '/App/backend/agent24d', ready, hold, spawned: sys.spawned, installed: () => false }, sys)
    return { deps, runs, hold }
  }

  it('install runs the bundled CLI with AGENT24D_BIN set to the bundled daemon, over this process environment', async () => {
    const { deps, runs } = system()
    await deps.install()
    expect(runs).toHaveLength(1)
    expect(runs[0]!.bin).toBe('/App/backend/agent24')
    expect(runs[0]!.args).toEqual(['service', 'install'])
    expect(runs[0]!.env['AGENT24D_BIN']).toBe('/App/backend/agent24d')
    expect(runs[0]!.env['PATH']).toBe(process.env['PATH'])
  })

  it('uninstall runs the bundled CLI `service uninstall`', async () => {
    const { deps, runs } = system()
    await deps.uninstall()
    expect(runs.map((r) => [r.bin, ...r.args])).toEqual([['/App/backend/agent24', 'service', 'uninstall']])
    expect(runs[0]!.env['AGENT24_SERVICE_KEEP_CONSENT']).toBe('1')
  })

  it('a failing run rejects', async () => {
    const { deps } = system({ run: () => Promise.reject(new Error('exit 1')) })
    await expect(deps.install()).rejects.toThrow('exit 1')
  })

  it('started: a live daemon on record with an enrolment socket that this app did not spawn', async () => {
    expect(await system().deps.started()).toBe(true)
  })

  it.each([
    ['no record', { record: () => null }],
    ['a record without an enrolment socket (a legacy daemon)', { record: () => ({ pid: 4242 }) }],
    ['a dead pid on record', { alive: () => false }],
    ['our own child on record', { spawned: () => true }],
  ])('not started: %s', async (_name, o) => {
    expect(await system(o).deps.started()).toBe(false)
  })

  it('started waits for the record to appear, within startWaitMs', async () => {
    let looks = 0
    const { deps } = system({ record: () => (++looks < 3 ? null : { pid: 4242, enrollEndpoint: '/tmp/enroll.sock' }), startWaitMs: 5_000 })
    expect(await deps.started()).toBe(true)
    expect(looks).toBe(3)
  })

  it('install waits for the documents module launch install first (Codex B4b C2), and runs when that failed too', async () => {
    let open!: () => void
    const { deps, runs } = system({}, new Promise<void>((r) => { open = r }))
    const installing = deps.install()
    await new Promise((r) => setTimeout(r, 10))
    expect(runs).toEqual([])
    open()
    await installing
    expect(runs).toHaveLength(1)
    const failed = system({}, Promise.reject(new Error('codesign')))
    await failed.deps.install()
    expect(failed.runs).toHaveLength(1)
  })

  it('not started: the daemon that was on record before the install, even alive with a socket (Codex B4b C3)', async () => {
    const { deps } = system({ record: () => ({ pid: 4242, enrollEndpoint: '/tmp/old.sock' }) })
    await deps.install()
    expect(await deps.started()).toBe(false)
  })

  it('started: a new daemon on record after one that was there before', async () => {
    let pid = 4242
    const { deps } = system({ record: () => ({ pid, enrollEndpoint: '/tmp/enroll.sock' }) })
    await deps.install()
    pid = 5151
    expect(await deps.started()).toBe(true)
  })

  it('the hold is the one given', () => {
    const { deps, hold } = system()
    expect(deps.hold).toBe(hold)
  })
})

describe('public install failure details', () => {
  it('limits output and redacts common secrets', () => {
    const reason = publicInstallReason(`${'x'.repeat(250)}\nsecond line`)
    expect(reason).toHaveLength(200)
    expect(reason).not.toContain('second')
    const secret = publicInstallReason('launchctl: OMLX_API_KEY=supersecret Bearer abcdefghijklmnop')
    expect(secret).not.toContain('supersecret')
    expect(secret).not.toContain('abcdefghijklmnop')
    expect(secret).toContain('OMLX_API_KEY=[redacted]')
  })
})
