// M5 B5 (#1741): bringing the LaunchAgent's daemon up to the one this app
// ships. launchd runs the copy at `<data_dir>/bin/agent24d` (B1, agent24-cli
// `service.rs`), and that file may change only while no daemon runs from it:
// the daemon runs its modules from its own path. So: an elevated
// `POST /shutdown` (the presence box, once), wait for that daemon to exit,
// copy when the bytes differ, `launchctl kickstart -k`. The entry to it is B4's.

import { execFile } from 'node:child_process'
import crypto from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { daemonConnection, daemonElevation, DAEMON_EXIT_WAIT_MS, processAlive, readExternalDaemonState } from './backend-manager'
import { sendElevating } from './elevated-request'

export const SERVICE_LABEL = 'ai.auraai.agent24'

/** `agent24d --version` prints `agent24d 0.6.0`. */
export function parseDaemonVersion(stdout: string): string | null {
  return /^agent24d (\S+)\s*$/.exec(stdout)?.[1] ?? null
}

export function bundledDaemonVersion(bin: string): Promise<string | null> {
  return new Promise((resolve) => {
    execFile(bin, ['--version'], { timeout: 10_000 }, (err, stdout) => resolve(err ? null : parseDaemonVersion(stdout)))
  })
}

/** `a.b.c`, optionally `-pre` and `+build`; null when it is not one. */
function semver(v: string | null): [number, number, number, string | null] | null {
  const m = v === null ? null : /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/.exec(v)
  return m ? [Number(m[1]), Number(m[2]), Number(m[3]), m[4] ?? null] : null
}

/** Only a bundled version newer than the running one: an older app never puts
 * its older daemon over a store a newer one migrated (PR-Daemon B5a B1). An
 * unknown version, or two pre-releases of one version, is never a reason. */
export function upgradeNeeded(bundled: string | null, running: string | null): boolean {
  const b = semver(bundled)
  const r = semver(running)
  if (!b || !r) return false
  for (const i of [0, 1, 2] as const) if (b[i] !== r[i]) return b[i] > r[i]
  return b[3] === null && r[3] !== null
}

/** Only a packaged app with its own agent24d: never a dev build or an
 * `AGENT24D_BIN` override (PR-Daemon B5a N3). */
export function upgradesFromBundle(packaged: boolean, env: NodeJS.ProcessEnv): boolean {
  return packaged && !env['AGENT24D_BIN']
}

export function serviceDaemonCopyPath(dataDir: string): string {
  return path.join(dataDir, 'bin', 'agent24d')
}

function sha256(file: string): string | null {
  try {
    return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')
  } catch {
    return null
  }
}

/** As `service.rs` `copy_daemon`: bytes only (no extended attributes, so no
 * quarantine flag), into an owner-only directory that is not a link, written
 * next to the target and renamed over it. Left as it was when it fails. */
export function copyDaemonIfChanged(src: string, dest: string): 'same' | 'copied' {
  const bytes = fs.readFileSync(src)
  const dir = path.dirname(dest)
  fs.mkdirSync(dir, { recursive: true })
  if (!fs.lstatSync(dir).isDirectory()) throw new Error(`${dir} is not a directory (a link is refused)`)
  fs.chmodSync(dir, 0o700)
  // Same bytes in a plain file only: a link there is replaced (Codex B5a C1).
  const there = fs.lstatSync(dest, { throwIfNoEntry: false })
  if (there?.isFile() && crypto.createHash('sha256').update(bytes).digest('hex') === sha256(dest)) {
    // As copy_daemon, which always rewrites: runnable whatever it was (PR-Daemon B5a N2).
    fs.chmodSync(dest, 0o755)
    return 'same'
  }
  const temp = path.join(dir, `.agent24d.${process.pid}.tmp`)
  fs.rmSync(temp, { force: true })
  try {
    const fd = fs.openSync(temp, 'wx', 0o755)
    try {
      fs.writeFileSync(fd, bytes)
      fs.fchmodSync(fd, 0o755)
      fs.fsyncSync(fd)
    } finally {
      fs.closeSync(fd)
    }
    fs.renameSync(temp, dest)
  } catch (err) {
    fs.rmSync(temp, { force: true })
    throw err
  }
  return 'copied'
}

/** The job's own `pid = N`, one tab in; a nested block's is not it (`service.rs` `job_pid`). */
export function jobPid(print: string): number | null {
  const pid = /^\tpid = (\d+)$/m.exec(print)?.[1]
  return pid ? Number(pid) : null
}

/** launchd's process for the job; null when it runs none. */
export function servicePid(uid: number): Promise<number | null> {
  return new Promise((resolve) => {
    execFile('launchctl', ['print', `gui/${uid}/${SERVICE_LABEL}`], { timeout: 10_000 }, (err, stdout) => resolve(err ? null : jobPid(stdout)))
  })
}

export function kickstartService(uid: number): Promise<void> {
  return new Promise((resolve, reject) => {
    execFile('launchctl', ['kickstart', '-k', `gui/${uid}/${SERVICE_LABEL}`], { timeout: 30_000 }, (err) => (err ? reject(err) : resolve()))
  })
}

export type UpgradeOutcome =
  | { ok: true; copied: boolean }
  | { ok: false; stage: 'shutdown' | 'exit' | 'copy' | 'kickstart'; code: string }

export interface UpgradeDeps {
  /** The elevated `POST /api/v1/shutdown`: its status and, on a refusal, the daemon's code. */
  shutdown: () => Promise<{ status: number; code?: string }>
  /** Resolves true once the daemon that was shut down has exited. */
  exited: () => Promise<boolean>
  copy: () => 'same' | 'copied'
  kickstart: () => Promise<void>
  /** Resolves true once a new service daemon is on record: only then may the hold end. */
  started: () => Promise<boolean>
  /** Runs `work` with this app spawning nothing of its own (the lock is the service's). */
  hold: <T>(work: () => Promise<T>) => Promise<T>
}

/** Nothing is started when the shutdown was refused. Once it was approved the
 * service is always started again, since a clean exit is not restarted by
 * launchd (PR-Daemon B5a N1): from the old file, left whole, when the old
 * daemon did not exit in time (no copy then) or the copy failed. */
export function upgradeService(deps: UpgradeDeps): Promise<UpgradeOutcome> {
  return deps.hold(async (): Promise<UpgradeOutcome> => {
    const down = await deps.shutdown()
    if (down.status < 200 || down.status >= 300) return { ok: false, stage: 'shutdown', code: down.code ?? `http_${down.status}` }
    const gone = await deps.exited()
    let copied = false
    let copyError: string | null = null
    if (gone) {
      try {
        copied = deps.copy() === 'copied'
      } catch (err) {
        copyError = (err as NodeJS.ErrnoException).code ?? 'copy_failed'
      }
    }
    try {
      await deps.kickstart()
    } catch {
      return { ok: false, stage: 'kickstart', code: 'kickstart_failed' }
    }
    if (!(await deps.started())) return { ok: false, stage: 'kickstart', code: 'not_started' }
    if (!gone) return { ok: false, stage: 'exit', code: 'still_running' }
    return copyError === null ? { ok: true, copied } : { ok: false, stage: 'copy', code: copyError }
  })
}

async function until(check: () => boolean, ms: number): Promise<boolean> {
  for (const end = Date.now() + ms; !check(); await new Promise((r) => setTimeout(r, 200))) {
    if (Date.now() >= end) return false
  }
  return true
}

/** What the real steps read of the system; tests put their own (QA #1744). */
export interface UpgradeSystem {
  alive: (pid: number) => boolean
  jobPid: () => Promise<number | null>
  record: () => { pid: number } | null
  exitWaitMs: number
  startWaitMs: number
}

const realSystem = (): UpgradeSystem => ({
  alive: processAlive,
  jobPid: () => servicePid(process.getuid!()),
  record: readExternalDaemonState,
  exitWaitMs: DAEMON_EXIT_WAIT_MS,
  startWaitMs: 30_000,
})

/** The real steps, for the enrolled service daemon `pid`. */
export function serviceUpgradeDeps(
  o: { bin: string; dataDir: string; pid: number; hold: UpgradeDeps['hold'] },
  sys: UpgradeSystem = realSystem(),
): UpgradeDeps {
  return {
    shutdown: async () => {
      const res = await sendElevating({ method: 'POST', path: '/api/v1/shutdown' }, {}, { connection: daemonConnection, elevation: daemonElevation })
      return { status: res.status, code: (res.data as { error?: { code?: string } } | null)?.error?.code }
    },
    // And launchd has not run it again from that file (an exit not 0; Codex B5a C3).
    exited: async () => (await until(() => !sys.alive(o.pid), sys.exitWaitMs)) && (await sys.jobPid()) === null,
    copy: () => copyDaemonIfChanged(o.bin, serviceDaemonCopyPath(o.dataDir)),
    kickstart: () => kickstartService(process.getuid!()),
    started: () => until(() => {
      const state = sys.record()
      return state !== null && state.pid !== o.pid && sys.alive(state.pid)
    }, sys.startWaitMs),
    hold: o.hold,
  }
}

export type UpgradeHint = null | { kind: 'needed' } | { kind: 'failed'; code: string }

/** One upgrade per run, asked once the enrolled service daemon's version is
 * not the bundled one: the presence box is the user's say (PM 11-09). A
 * refused box leaves the hint and is not asked again until the next launch. */
export class ServiceUpgrader {
  private tried = false
  private failed: string | null = null

  constructor(private readonly deps: {
    bundled: () => string | null
    running: () => string | null
    /** The LaunchAgent's plist is there: kickstart restarts that job only (Codex B5a C2). */
    installed: () => boolean
    pid: () => number | null
    run: (pid: number) => Promise<UpgradeOutcome>
  }) {}

  tick(): void {
    const pid = this.deps.pid()
    if (this.tried || pid === null || !this.deps.installed() || !upgradeNeeded(this.deps.bundled(), this.deps.running())) return
    this.tried = true
    void this.deps.run(pid).then(
      (o) => { if (!o.ok && o.stage !== 'shutdown') this.failed = o.code },
      () => { this.failed = 'upgrade_failed' },
    )
  }

  hint(): UpgradeHint {
    if (this.failed !== null) return { kind: 'failed', code: this.failed }
    return upgradeNeeded(this.deps.bundled(), this.deps.running()) ? { kind: 'needed' } : null
  }
}
