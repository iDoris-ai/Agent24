import { afterEach, describe, expect, it, vi } from 'vitest'
import crypto from 'node:crypto'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import {
  copyDaemonIfChanged, jobPid, parseDaemonVersion, ServiceUpgrader, serviceDaemonCopyPath, serviceUpgradeDeps, upgradeNeeded, upgradeService, upgradesFromBundle,
  type UpgradeDeps, type UpgradeOutcome, type UpgradeSystem,
} from './resident-service'
import { buildTrayView } from './tray-menu'
import { resetMainLocaleForTest, setMainLocale } from './main-i18n'

const sha = (file: string) => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')

let root = ''
afterEach(() => {
  vi.restoreAllMocks()
  resetMainLocaleForTest()
  if (root) fs.rmSync(root, { recursive: true, force: true })
  root = ''
})

function files(oldBytes: string | null, newBytes: string) {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'a24b5-'))
  const src = path.join(root, 'app', 'agent24d')
  fs.mkdirSync(path.dirname(src))
  fs.writeFileSync(src, newBytes)
  const dest = serviceDaemonCopyPath(path.join(root, 'data'))
  if (oldBytes !== null) {
    fs.mkdirSync(path.dirname(dest), { recursive: true })
    fs.writeFileSync(dest, oldBytes, { mode: 0o755 })
  }
  return { src, dest, dir: path.dirname(dest) }
}

describe('versions', () => {
  it('reads `agent24d --version`, and only two known versions that differ ask for an upgrade', () => {
    expect(parseDaemonVersion('agent24d 0.6.0\n')).toBe('0.6.0')
    expect(parseDaemonVersion('something else')).toBeNull()
    expect(upgradeNeeded('0.6.1', '0.6.0')).toBe(true)
    expect(upgradeNeeded('0.6.0', '0.6.0')).toBe(false)
    expect(upgradeNeeded(null, '0.6.0')).toBe(false)
    expect(upgradeNeeded('0.6.1', null)).toBe(false)
  })

  it('an older app never downgrades the service daemon; numbers compare as numbers (PR-Daemon B5a B1)', () => {
    expect(upgradeNeeded('0.6.0', '0.6.1')).toBe(false)
    expect(upgradeNeeded('0.5.9', '0.6.0')).toBe(false)
    expect(upgradeNeeded('0.10.0', '0.9.9')).toBe(true)
    expect(upgradeNeeded('1.0.0', '0.99.0')).toBe(true)
    // A release is newer than its pre-release; two pre-releases are not ordered.
    expect(upgradeNeeded('0.7.0', '0.7.0-rc.1')).toBe(true)
    expect(upgradeNeeded('0.7.0-rc.1', '0.7.0')).toBe(false)
    expect(upgradeNeeded('0.7.0-rc.2', '0.7.0-rc.1')).toBe(false)
    expect(upgradeNeeded('0.6.1+abc', '0.6.0')).toBe(true)
    expect(upgradeNeeded('v0.6.1', '0.6.0')).toBe(false)
  })

  it('only a packaged app with no AGENT24D_BIN upgrades from its bundle (PR-Daemon B5a N3)', () => {
    expect(upgradesFromBundle(true, {})).toBe(true)
    expect(upgradesFromBundle(false, {})).toBe(false)
    expect(upgradesFromBundle(true, { AGENT24D_BIN: '/x/agent24d' })).toBe(false)
  })

  it("launchd's pid for the job is its own line, not a nested one, and none between runs", () => {
    expect(jobPid('gui/501/ai.auraai.agent24 = {\n\tstate = running\n\tpid = 4242\n\tendpoints = {\n\t\tpid = 7\n\t}\n}')).toBe(4242)
    expect(jobPid('gui/501/ai.auraai.agent24 = {\n\tstate = not running\n\tendpoints = {\n\t\tpid = 7\n\t}\n}')).toBeNull()
  })
})

describe('copyDaemonIfChanged (as service.rs copy_daemon)', () => {
  it('replaces a different copy whole: 0755 in an owner-only bin/, nothing left beside it, whatever the umask', () => {
    const { src, dest, dir } = files('old', 'new')
    const umask = process.umask(0o077)
    try {
      expect(copyDaemonIfChanged(src, dest)).toBe('copied')
    } finally {
      process.umask(umask)
    }
    expect(sha(dest)).toBe(sha(src))
    expect(fs.statSync(dest).mode & 0o777).toBe(0o755)
    expect(fs.statSync(dir).mode & 0o777).toBe(0o700)
    expect(fs.readdirSync(dir)).toEqual(['agent24d'])
  })

  it('the same bytes in a file that is not runnable: still "same", made 0755 as copy_daemon does (PR-Daemon B5a N2)', () => {
    const { src, dest } = files('same', 'same')
    fs.chmodSync(dest, 0o644)
    expect(copyDaemonIfChanged(src, dest)).toBe('same')
    expect(fs.statSync(dest).mode & 0o777).toBe(0o755)
  })

  it('makes bin/ when there is none', () => {
    const { src, dest } = files(null, 'new')
    expect(copyDaemonIfChanged(src, dest)).toBe('copied')
    expect(sha(dest)).toBe(sha(src))
  })

  it('a bin/ that is a link is refused, and nothing is written through it', () => {
    const { src, dest } = files(null, 'new')
    const elsewhere = path.join(root, 'elsewhere')
    fs.mkdirSync(elsewhere)
    fs.mkdirSync(path.dirname(path.dirname(dest)), { recursive: true })
    fs.symlinkSync(elsewhere, path.dirname(dest))
    expect(() => copyDaemonIfChanged(src, dest)).toThrow(/link is refused/)
    expect(fs.readdirSync(elsewhere)).toEqual([])
  })

  it('the same bytes behind a link are not "the same": the link is replaced by a plain file (Codex B5a C1)', () => {
    const { src, dest } = files(null, 'same')
    const target = path.join(root, 'target')
    fs.writeFileSync(target, 'same')
    fs.mkdirSync(path.dirname(dest), { recursive: true })
    fs.symlinkSync(target, dest)
    expect(copyDaemonIfChanged(src, dest)).toBe('copied')
    expect(fs.lstatSync(dest).isFile()).toBe(true)
    expect(fs.readFileSync(target, 'utf8')).toBe('same')
  })

  it('a bin/ that is a link is refused even when the bytes behind it are the same (Codex B5a C1)', () => {
    const { src, dest } = files(null, 'same')
    const elsewhere = path.join(root, 'elsewhere')
    fs.mkdirSync(elsewhere)
    fs.writeFileSync(path.join(elsewhere, 'agent24d'), 'same')
    fs.mkdirSync(path.dirname(path.dirname(dest)), { recursive: true })
    fs.symlinkSync(elsewhere, path.dirname(dest))
    expect(() => copyDaemonIfChanged(src, dest)).toThrow(/link is refused/)
  })

  it('a copy that fails part way leaves the old file\'s sha and no temp file (UN3)', () => {
    const { src, dest, dir } = files('old', 'new')
    const before = sha(dest)
    vi.spyOn(fs, 'renameSync').mockImplementation(() => { throw Object.assign(new Error('EIO'), { code: 'EIO' }) })
    expect(() => copyDaemonIfChanged(src, dest)).toThrow('EIO')
    expect(sha(dest)).toBe(before)
    expect(fs.readdirSync(dir)).toEqual(['agent24d'])
  })
})

function steps(over: Partial<UpgradeDeps> = {}) {
  const calls: string[] = []
  const deps: UpgradeDeps = {
    shutdown: async () => { calls.push('shutdown'); return { status: 202 } },
    exited: async () => { calls.push('exited'); return true },
    copy: () => { calls.push('copy'); return 'copied' },
    kickstart: async () => { calls.push('kickstart') },
    started: async () => { calls.push('started'); return true },
    hold: async (work) => {
      calls.push('hold')
      try {
        return await work()
      } finally {
        calls.push('release')
      }
    },
    ...over,
  }
  return { calls, deps }
}

describe('upgradeService', () => {
  it('in a hold: shutdown, its exit, the copy, kickstart, the new daemon on record', async () => {
    const { calls, deps } = steps()
    await expect(upgradeService(deps)).resolves.toEqual({ ok: true, copied: true })
    expect(calls).toEqual(['hold', 'shutdown', 'exited', 'copy', 'kickstart', 'started', 'release'])
  })

  it('a refused presence for shutdown never kickstarts and never copies (UN6)', async () => {
    const { calls, deps } = steps({ shutdown: async () => ({ status: 403, code: 'presence_denied' }) })
    await expect(upgradeService(deps)).resolves.toEqual({ ok: false, stage: 'shutdown', code: 'presence_denied' })
    expect(calls).toEqual(['hold', 'release'])
  })

  it('shutdown done, kickstart failed: the tray says so, never silent (UN7)', async () => {
    const { deps } = steps({ kickstart: async () => { throw new Error('launchctl: 113') } })
    const outcome = await upgradeService(deps)
    expect(outcome).toEqual({ ok: false, stage: 'kickstart', code: 'kickstart_failed' })
    const upgrader = new ServiceUpgrader({ bundled: () => '0.6.1', running: () => '0.6.0', installed: () => true, pid: () => 42, run: async () => outcome })
    upgrader.tick()
    await vi.waitFor(() => expect(upgrader.hint()).toEqual({ kind: 'failed', code: 'kickstart_failed' }))
    setMainLocale('zh')
    const tray = { status: 'running', external: true, enrolled: false, externalPid: 42, foreignPid: null, backendKind: 'rust' } as const
    expect(buildTrayView({ ...tray, upgrade: upgrader.hint() }).tooltip).toContain('后台服务升级未完成（kickstart_failed）')
  })

  it('version differs but the stable copy already matches: no copy, still shutdown and kickstart (UN2)', async () => {
    const { src, dest } = files('same', 'same')
    const mtime = fs.statSync(dest).mtimeMs
    const { calls, deps } = steps({ copy: () => { calls.push('copy'); return copyDaemonIfChanged(src, dest) } })
    await expect(upgradeService(deps)).resolves.toEqual({ ok: true, copied: false })
    expect(calls).toEqual(['hold', 'shutdown', 'exited', 'copy', 'kickstart', 'started', 'release'])
    expect(fs.statSync(dest).mtimeMs).toBe(mtime)
  })

  it('the old daemon still running: nothing copied, and the service is started again from the old file (PR-Daemon B5a N1)', async () => {
    const { calls, deps } = steps({ exited: async () => { calls.push('exited'); return false } })
    await expect(upgradeService(deps)).resolves.toEqual({ ok: false, stage: 'exit', code: 'still_running' })
    expect(calls).toEqual(['hold', 'shutdown', 'exited', 'kickstart', 'started', 'release'])
  })

  it('a failed copy still starts the old file, which it left whole, and says the copy failed', async () => {
    const { calls, deps } = steps({ copy: () => { throw Object.assign(new Error('x'), { code: 'EACCES' }) } })
    await expect(upgradeService(deps)).resolves.toEqual({ ok: false, stage: 'copy', code: 'EACCES' })
    expect(calls).toContain('kickstart')
  })

  it('no new daemon on record after kickstart: not ok', async () => {
    const { deps } = steps({ started: async () => false })
    await expect(upgradeService(deps)).resolves.toEqual({ ok: false, stage: 'kickstart', code: 'not_started' })
  })
})

describe('serviceUpgradeDeps: the real steps on the system (QA #1744)', () => {
  function real(over: Partial<UpgradeSystem> = {}) {
    const sys: UpgradeSystem = {
      alive: vi.fn(() => false), jobPid: vi.fn(async () => null), record: vi.fn(() => null), exitWaitMs: 300, startWaitMs: 300, ...over,
    }
    const { src, dest } = files('old', 'new')
    const deps = serviceUpgradeDeps({ bin: src, dataDir: path.join(root, 'data'), pid: 42, hold: (w) => w() }, sys)
    return { sys, deps, src, dest }
  }

  it('the old pid still alive: not exited, and launchd is never asked in its place', async () => {
    const { sys, deps } = real({ alive: vi.fn((pid: number) => pid === 42) })
    await expect(deps.exited()).resolves.toBe(false)
    expect(sys.jobPid).not.toHaveBeenCalled()
  })

  it('the old pid gone: exited only when launchd runs no process for the job either', async () => {
    await expect(real().deps.exited()).resolves.toBe(true)
    await expect(real({ jobPid: vi.fn(async () => 777) }).deps.exited()).resolves.toBe(false)
  })

  it('started: a new live daemon on record, not the old one, not a dead one', async () => {
    const alive = (pid: number) => pid === 43
    await expect(real({ alive, record: () => ({ pid: 43 }) }).deps.started()).resolves.toBe(true)
    // The old pid, alive or not, is never the new daemon.
    await expect(real({ alive: () => true, record: () => ({ pid: 42 }) }).deps.started()).resolves.toBe(false)
    await expect(real({ alive, record: () => ({ pid: 44 }) }).deps.started()).resolves.toBe(false)
    await expect(real({ alive, record: () => null }).deps.started()).resolves.toBe(false)
  })

  it('copy: the bundled binary onto <data_dir>/bin/agent24d', () => {
    const { deps, src, dest } = real()
    expect(deps.copy()).toBe('copied')
    expect(sha(dest)).toBe(sha(src))
  })
})

describe('ServiceUpgrader (PM 11-09: asked once per run, after enrolment)', () => {
  function upgrader(o: { bundled?: string | null; running?: string | null; pid?: number | null; installed?: boolean; outcome?: UpgradeOutcome }) {
    const run = vi.fn(async () => o.outcome ?? ({ ok: true, copied: true } as const))
    const pick = <K extends 'bundled' | 'running' | 'pid'>(k: K, d: NonNullable<typeof o[K]>) => () => (k in o ? o[k] ?? null : d)
    const u = new ServiceUpgrader({ bundled: pick('bundled', '0.6.1'), running: pick('running', '0.6.0'), installed: () => o.installed ?? true, pid: pick('pid', 42), run })
    return { u, run }
  }

  it('versions that differ: one upgrade for that daemon, the hint until it is done', async () => {
    const { u, run } = upgrader({})
    expect(u.hint()).toEqual({ kind: 'needed' })
    u.tick()
    u.tick()
    expect(run).toHaveBeenCalledTimes(1)
    expect(run).toHaveBeenCalledWith(42)
  })

  it('the presence box refused: the hint stays, and nothing is asked again this run', async () => {
    const { u, run } = upgrader({ outcome: { ok: false, stage: 'shutdown', code: 'presence_denied' } })
    u.tick()
    await Promise.resolve()
    await Promise.resolve()
    u.tick()
    expect(run).toHaveBeenCalledTimes(1)
    expect(u.hint()).toEqual({ kind: 'needed' })
  })

  it.each([
    ['the same version', { running: '0.6.1' }],
    ['not enrolled (no version either)', { pid: null, running: null }],
    ['the bundled version unknown', { bundled: null }],
  ] as const)('%s: nothing run, no hint', (_name, o) => {
    const { u, run } = upgrader(o)
    u.tick()
    expect(run).not.toHaveBeenCalled()
    expect(u.hint()).toBeNull()
  })

  it('no LaunchAgent plist: nothing run, since kickstart restarts that job only (Codex B5a C2)', () => {
    const { u, run } = upgrader({ installed: false })
    u.tick()
    expect(run).not.toHaveBeenCalled()
  })
})

describe('tray text (three languages)', () => {
  const tray = { status: 'running', external: true, enrolled: false, externalPid: 42, foreignPid: null, backendKind: 'rust' } as const
  it.each([
    ['en', 'restart the background service to finish the upgrade'],
    ['zh', '重启后台服务以完成升级'],
    ['th', 'รีสตาร์ตบริการเบื้องหลังเพื่ออัปเกรดให้เสร็จ'],
  ] as const)('%s: an upgrade needed and one that did not finish', (lang, restart) => {
    setMainLocale(lang)
    const needed = buildTrayView({ ...tray, upgrade: { kind: 'needed' } }).tooltip
    const failed = buildTrayView({ ...tray, upgrade: { kind: 'failed', code: 'not_started' } }).tooltip
    expect(needed).toContain(restart)
    expect(buildTrayView(tray).tooltip).not.toContain(restart)
    expect(failed).toContain('not_started')
    expect(failed).toContain('`agent24 service install`')
    // Reopening cannot work: with no service daemon nothing asks again (PR-Daemon B5a N1).
    expect(failed.replace(buildTrayView(tray).tooltip, '')).not.toMatch(/Reopen|重新打开|เปิด Agent24 ใหม่/)
  })
})
