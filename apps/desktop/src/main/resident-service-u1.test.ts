import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { execFile } from 'node:child_process'
import { SERVICE_LABEL, copyDaemonIfChanged, upgradeServiceByLaunchd, type UpgradeDeps } from './resident-service'

const dirs: string[] = []
afterEach(() => { for (const d of dirs) fs.rmSync(d, { recursive: true, force: true }); dirs.length = 0 })

/** A fake `execFile` recording each launchctl call; `fail(args, n)` says which fail. */
function launchctl(fail: (args: string[], n: number) => boolean = () => false) {
  const calls: string[][] = []
  const run = ((_cmd: string, args: string[], _opts: unknown, cb: (err: Error | null) => void) => {
    calls.push(args)
    cb(fail(args, calls.filter((c) => c[0] === args[0]).length) ? new Error(`${args[0]} failed`) : null)
  }) as unknown as typeof execFile
  return { calls, run }
}

/** The installed old daemon and the bundled new one, as files. */
function files() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'u1-'))
  dirs.push(dir)
  const src = path.join(dir, 'bundled-agent24d')
  const dest = path.join(dir, 'bin', 'agent24d')
  fs.writeFileSync(src, 'NEW')
  fs.mkdirSync(path.dirname(dest))
  fs.writeFileSync(dest, 'OLD')
  return { src, dest }
}

function deps(f: { src: string; dest: string }) {
  const d: UpgradeDeps = {
    shutdown: vi.fn(async () => ({ status: 200 })),
    exited: vi.fn(async () => true),
    copy: vi.fn(() => copyDaemonIfChanged(f.src, f.dest)),
    kickstart: vi.fn(async () => {}),
    started: vi.fn(async () => true),
    hold: (work) => work(),
  }
  return d
}

const versionOf = (bin: string) => Promise.resolve(fs.readFileSync(bin, 'utf8') === 'NEW' ? '0.6.50' : '0.6.49')
const BOOTOUT = ['bootout', `gui/501/${SERVICE_LABEL}`]
const BOOTSTRAP = ['bootstrap', 'gui/501', '/p']

describe('U1 option C: the upgrade stops and starts the service through launchd', () => {
  it('boots the job out (no elevated /shutdown, no presence box), copies, checks the copy, bootstraps', async () => {
    const f = files()
    const { calls, run } = launchctl()
    const d = deps(f)
    const out = await upgradeServiceByLaunchd(d, { uid: 501, plist: '/p', ...f, versionOf, run })
    expect(out).toEqual({ ok: true, copied: true })
    expect(d.shutdown).not.toHaveBeenCalled()
    expect(calls).toEqual([BOOTOUT, BOOTSTRAP])
    expect(fs.readFileSync(f.dest, 'utf8')).toBe('NEW')
  })

  it('a failed bootout is reported, and nothing is replaced or started', async () => {
    const f = files()
    const { calls, run } = launchctl((args) => args[0] === 'bootout')
    const d = deps(f)
    const out = await upgradeServiceByLaunchd(d, { uid: 501, plist: '/p', ...f, versionOf, run })
    expect(out).toEqual({ ok: false, stage: 'shutdown', code: 'bootout_failed' })
    expect(d.copy).not.toHaveBeenCalled()
    expect(calls).toEqual([BOOTOUT])
    expect(fs.readFileSync(f.dest, 'utf8')).toBe('OLD')
  })

  it('a copy that does not say the bundled version is put back before anything starts (PM 11-19 ①)', async () => {
    const f = files()
    const { calls, run } = launchctl()
    const out = await upgradeServiceByLaunchd(deps(f), { uid: 501, plist: '/p', ...f, versionOf: async () => null, run })
    expect(out).toEqual({ ok: false, stage: 'verify', code: 'verify_failed' })
    expect(fs.readFileSync(f.dest, 'utf8')).toBe('OLD')
    expect(calls).toEqual([BOOTOUT, BOOTSTRAP])
  })

  it('a new daemon that does not start is rolled back to the old one, which is started (PM 11-19 ①)', async () => {
    const f = files()
    const { calls, run } = launchctl((args, n) => args[0] === 'bootstrap' && n === 1)
    const out = await upgradeServiceByLaunchd(deps(f), { uid: 501, plist: '/p', ...f, versionOf, run })
    expect(out).toEqual({ ok: false, stage: 'rollback', code: 'rolled_back' })
    expect(fs.readFileSync(f.dest, 'utf8')).toBe('OLD')
    expect(calls).toEqual([BOOTOUT, BOOTSTRAP, BOOTOUT, BOOTSTRAP])
  })

  it('says so when even the old one does not start again', async () => {
    const f = files()
    const { run } = launchctl((args) => args[0] === 'bootstrap')
    const out = await upgradeServiceByLaunchd(deps(f), { uid: 501, plist: '/p', ...f, versionOf, run })
    expect(out).toEqual({ ok: false, stage: 'rollback', code: 'rollback_failed' })
  })
})
