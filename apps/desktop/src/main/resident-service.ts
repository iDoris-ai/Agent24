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

/** Only a newer bundled version: never an older daemon over a store a newer
 * one migrated (PR-Daemon B5a B1); an unknown version is never a reason. */
export function upgradeNeeded(bundled: string | null, running: string | null): boolean {
  const b = semver(bundled)
  const r = semver(running)
  if (!b || !r) return false
  for (const i of [0, 1, 2] as const) if (b[i] !== r[i]) return b[i] > r[i]
  return b[3] === null && r[3] !== null
}

/** Never a dev build or an `AGENT24D_BIN` override (PR-Daemon B5a N3). */
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
    fs.chmodSync(dest, 0o755) // as copy_daemon, which always rewrites (PR-Daemon B5a N2)
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

/** The elevated `POST /api/v1/shutdown` (the presence box): its status and,
 * on a refusal, the daemon's code. */
export async function elevatedShutdown(): Promise<{ status: number; code?: string }> {
  const res = await sendElevating({ method: 'POST', path: '/api/v1/shutdown' }, {}, { connection: daemonConnection, elevation: daemonElevation })
  return { status: res.status, code: (res.data as { error?: { code?: string } } | null)?.error?.code }
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
    shutdown: elevatedShutdown,
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

/** `launchctl print`'s own `runs = N`: how many times launchd has started the
 * job. A job that runs on keeps it; one that keeps failing pushes it up even
 * when every look finds it running (PR-Daemon B5b B1; Codex on its first fix). */
export function jobRuns(print: string): number | null {
  const runs = /^\truns = (\d+)$/m.exec(print)?.[1]
  return runs === undefined ? null : Number(runs)
}

export function serviceRuns(uid: number): Promise<number | null> {
  return new Promise((resolve) => {
    execFile('launchctl', ['print', `gui/${uid}/${SERVICE_LABEL}`], { timeout: 10_000 }, (err, stdout) => resolve(err ? null : jobRuns(stdout)))
  })
}

/** The pid on record when it can be the service's daemon: one with an
 * enrolment socket (a legacy one has none) that this app did not spawn; our
 * own child writes the socket too (`server.rs`; Codex B5b C1, R3). */
export function serviceRecordPid(state: { pid: number; enrollEndpoint?: string } | null, spawned: (pid: number) => boolean): number | null {
  return state && state.enrollEndpoint !== undefined && !spawned(state.pid) ? state.pid : null
}

export const LOOP_WINDOW_MS = 60_000
export const LOOP_PIDS = 3
export const LOOP_RESTARTS = 2
const RUNS_CHECK_MS = 10_000

/** M5 B5 ②: the LaunchAgent's daemon keeps exiting and launchd keeps starting
 * it (a failed migration today: CORE-5 is off). Within a minute, either
 * several daemons on record, or launchd started the job at least twice more
 * (one that fails before it writes a record). One restart, such as an upgrade,
 * is not a loop. Only while the plist is there and this app is not enrolled. */
export class CrashLoopDetector {
  private pids: { pid: number; at: number }[] = []
  private runs: { runs: number; at: number }[] = []
  private runsAt = -Infinity
  private era = 0

  constructor(private readonly deps: {
    now: () => number
    installed: () => boolean
    enrolled: () => boolean
    recordPid: () => number | null
    runs: () => Promise<number | null>
  }) {}

  async sample(): Promise<void> {
    const now = this.deps.now()
    if (!this.deps.installed() || this.deps.enrolled()) {
      this.pids = []
      this.runs = []
      this.era++
      return
    }
    const pid = this.deps.recordPid()
    if (pid !== null && this.pids.at(-1)?.pid !== pid) this.pids.push({ pid, at: now })
    this.pids = this.pids.filter((p) => now - p.at < LOOP_WINDOW_MS)
    if (now - this.runsAt >= RUNS_CHECK_MS) {
      this.runsAt = now
      const era = this.era
      const runs = await this.deps.runs()
      // Enrolled meanwhile: what launchd said then is no longer this run (Codex on B1 R2 C2).
      if (era !== this.era) return
      if (runs !== null) {
        // Fewer than before: the job was loaded again, so count from here (Codex on B1 R2 C1).
        if (runs < (this.runs.at(-1)?.runs ?? runs)) this.runs = []
        this.runs.push({ runs, at: now })
      }
    }
    this.runs = this.runs.filter((r) => now - r.at < LOOP_WINDOW_MS)
  }

  looping(): boolean {
    if (new Set(this.pids.map((p) => p.pid)).size >= LOOP_PIDS) return true
    const seen = this.runs.map((r) => r.runs)
    return Math.max(...seen) - Math.min(...seen) >= LOOP_RESTARTS
  }
}

// ── M5 B4b: first-launch consent and `agent24 service install` ──────────────
// David 11-10 (a): asked once, installed only on yes. The answer is kept in
// `<data_dir>/desktop/resident-consent.json` (asked / answer / time, nothing
// else: N4). A yes whose install fails is not kept, so the next launch asks
// again (PM r1 ②).

export const CONSENT_VERSION = 1
export const INSTALL_START_WAIT_MS = 30_000

export type ConsentAnswer = 'allow' | 'declined'
export interface ConsentRecord { version: typeof CONSENT_VERSION; answer: ConsentAnswer; at: string }

export function consentPath(dataDir: string): string {
  return path.join(dataDir, 'desktop', 'resident-consent.json')
}

/** The kept answer; null when there is none, or it is not one this version
 * wrote (a broken or older file is asked again). */
export function readConsent(file: string): ConsentRecord | null {
  try {
    const value: unknown = JSON.parse(fs.readFileSync(file, 'utf8'))
    if (typeof value !== 'object' || value === null || Array.isArray(value)) return null
    const { version, answer, at } = value as { version: unknown; answer: unknown; at: unknown }
    if (version !== CONSENT_VERSION || (answer !== 'allow' && answer !== 'declined') || typeof at !== 'string') return null
    return { version: CONSENT_VERSION, answer, at }
  } catch {
    return null
  }
}

/** Written next to the target and renamed over it, owner-only (0600) in a
 * directory created when missing. */
export function writeConsent(file: string, answer: ConsentAnswer, now: Date): void {
  fs.mkdirSync(path.dirname(file), { recursive: true })
  const temp = path.join(path.dirname(file), `.resident-consent.${process.pid}.tmp`)
  fs.rmSync(temp, { force: true })
  try {
    const fd = fs.openSync(temp, 'wx', 0o600)
    try {
      fs.writeFileSync(fd, JSON.stringify({ version: CONSENT_VERSION, answer, at: now.toISOString() }))
      fs.fchmodSync(fd, 0o600)
      fs.fsyncSync(fd)
    } finally {
      fs.closeSync(fd)
    }
    fs.renameSync(temp, file)
  } catch (err) {
    fs.rmSync(temp, { force: true })
    throw err
  }
}

/** Only a packaged macOS app that ships its own daemon (`upgradesFromBundle`),
 * with no LaunchAgent yet and no kept answer. */
export function shouldAskConsent(o: { platform: NodeJS.Platform; fromBundle: boolean; installed: boolean; consent: ConsentRecord | null }): boolean {
  return o.platform === 'darwin' && o.fromBundle && !o.installed && o.consent === null
}

export type InstallOutcome = { ok: true } | { ok: false; stage: 'install' | 'start'; code: string }

export interface InstallDeps {
  /** `agent24 service install`; rejects when it fails. */
  install: () => Promise<void>
  /** Resolves true once the service daemon is on record (within INSTALL_START_WAIT_MS). */
  started: () => Promise<boolean>
  /** `agent24 service uninstall`; idempotent. */
  uninstall: () => Promise<void>
  /** Runs `work` with this app's own daemon stopped and nothing spawned (`whileStopped`). */
  hold: <T>(work: () => Promise<T>) => Promise<T>
  /** The plist is there already, looked at once the hold has begun: another
   * install queued ahead did it, and installing again would bootout that
   * service daemon with no presence box (Codex B4c2 C1). */
  installed?: () => boolean
}

function installErrorCode(err: unknown): string {
  if (typeof err === 'object' && err !== null && 'code' in err) {
    if (typeof err.code === 'string') return err.code
    if (typeof err.code === 'number') return `exit_${err.code}`
  }
  return 'install_failed'
}

/** Inside one hold: install, then wait for the service daemon. Any failure
 * uninstalls once, so that no half-installed plist is left; when that
 * uninstall fails too, its failure is ignored and the plist may stay (known
 * Low, Codex B4b C4). The hold's end brings this app's own daemon back. */
export function installService(deps: InstallDeps): Promise<InstallOutcome> {
  return deps.hold(async (): Promise<InstallOutcome> => {
    if (deps.installed?.()) return { ok: true }
    const failed = async (stage: 'install' | 'start', code: string): Promise<InstallOutcome> => {
      try { await deps.uninstall() } catch { /* its own failure is ignored */ }
      return { ok: false, stage, code }
    }
    try {
      await deps.install()
    } catch (err) {
      return failed('install', installErrorCode(err))
    }
    if (!(await deps.started())) return failed('start', 'not_started')
    return { ok: true }
  })
}

export type ConsentOutcome = 'declined' | 'installed' | { failed: string }

/** Ask, then: no → kept as declined; yes → install, kept as allow only when it
 * worked. */
export async function runConsent(deps: {
  ask: () => Promise<ConsentAnswer>
  install: () => Promise<InstallOutcome>
  record: (answer: ConsentAnswer) => void
}): Promise<ConsentOutcome> {
  let answer: ConsentAnswer
  try { answer = await deps.ask() } catch { return { failed: 'ask_failed' } }
  if (answer === 'declined') { deps.record('declined'); return 'declined' }
  let outcome: InstallOutcome
  try { outcome = await deps.install() } catch { return { failed: 'install_failed' } }
  if (!outcome.ok) return { failed: outcome.code }
  deps.record('allow')
  return 'installed'
}

/** What the real steps run and read; tests put their own. */
export interface InstallSystem {
  run: (bin: string, args: string[], env: NodeJS.ProcessEnv) => Promise<void>
  record: () => { pid: number; enrollEndpoint?: string } | null
  alive: (pid: number) => boolean
  spawned: (pid: number) => boolean
  startWaitMs: number
}

const realInstallSystem = (spawned: (pid: number) => boolean): InstallSystem => ({
  run: (bin, args, env) => new Promise((resolve, reject) => {
    execFile(bin, args, { env, timeout: 60_000 }, (err) => (err ? reject(err) : resolve()))
  }),
  record: readExternalDaemonState,
  alive: processAlive,
  spawned,
  startWaitMs: INSTALL_START_WAIT_MS,
})

/** The real steps: the bundled CLI with `AGENT24D_BIN` set to the bundled
 * daemon (so the copy launchd runs is this app's), once `ready` (the bundled
 * documents module's launch install) is done: the service daemon starts as
 * soon as it is bootstrapped (Codex B4b C2). Started means a live daemon on
 * record with an enrolment socket that this app did not spawn and that was
 * not the one on record before the install (Codex B4b C3). */
export function serviceInstallDeps(
  o: { cli: string; daemonBin: string; ready: Promise<unknown>; hold: InstallDeps['hold']; spawned: (pid: number) => boolean; installed: () => boolean },
  sys: InstallSystem = realInstallSystem(o.spawned),
): InstallDeps {
  const env = { ...process.env, AGENT24D_BIN: o.daemonBin }
  let before: number | null = null
  return {
    install: async () => {
      await o.ready.catch(() => {})
      before = sys.record()?.pid ?? null
      await sys.run(o.cli, ['service', 'install'], env)
    },
    uninstall: () => sys.run(o.cli, ['service', 'uninstall'], env),
    started: () => until(() => {
      const state = sys.record()
      return state !== null && state.pid !== before && state.enrollEndpoint !== undefined && !sys.spawned(state.pid) && sys.alive(state.pid)
    }, sys.startWaitMs),
    hold: o.hold,
    installed: o.installed,
  }
}

// ── M5 B4c: stopping the service for good (the tray's 「完全退出」) ─────────
// Shutdown first, then uninstall: the plist's KeepAlive is
// {SuccessfulExit:false} (service.rs), so a daemon that exits 0 is not started
// again and nothing races for the lock (PM r1 ①). A refused or timed-out
// presence box changes nothing.

export type StopOutcome = { ok: true } | { ok: false; stage: 'shutdown' | 'uninstall'; code: string }

export interface StopDeps {
  /** The elevated `POST /shutdown` while enrolled with the service daemon;
   * null when not enrolled. */
  shutdown: (() => Promise<{ status: number; code?: string }>) | null
  /** Not enrolled, yet a live service daemon is on record (the tray's
   * disconnect, a refused enrolment): it is not stopped without the presence
   * box (Codex B4c1 C1), so nothing changes until this app is enrolled again.
   * Not enrolled and none on record (a crash loop, waiting): the uninstall's
   * bootout stops whatever launchd runs. */
  unreachable: () => boolean
  /** Resolves true once the daemon that was shut down has exited; false when
   * the wait ran out (the bootout then stops it). */
  exited: () => Promise<boolean>
  /** `agent24 service uninstall`. */
  uninstall: () => Promise<void>
  /** Runs `work` with this app spawning nothing (`whileServiceRestarts`). */
  hold: <T>(work: () => Promise<T>) => Promise<T>
  /** Keeps the answer as declined, so the first-launch box is not shown again. */
  record: () => void
}

/** Inside one hold: shutdown (when there is a daemon to ask), wait for it to
 * exit, uninstall, then record. A refused or failed shutdown stops there with
 * nothing changed; a failed record is ignored (the plist is gone already). */
export function stopService(deps: StopDeps): Promise<StopOutcome> {
  return deps.hold(async (): Promise<StopOutcome> => {
    if (!deps.shutdown && deps.unreachable()) return { ok: false, stage: 'shutdown', code: 'not_connected' }
    if (deps.shutdown) {
      let down: { status: number; code?: string }
      try { down = await deps.shutdown() } catch { return { ok: false, stage: 'shutdown', code: 'shutdown_failed' } }
      if (down.status < 200 || down.status >= 300) return { ok: false, stage: 'shutdown', code: down.code ?? `http_${down.status}` }
      await deps.exited()
    }
    try { await deps.uninstall() } catch (err) { return { ok: false, stage: 'uninstall', code: installErrorCode(err) } }
    try { deps.record() } catch { /* the plist is gone already */ }
    return { ok: true }
  })
}

/** What the real steps run and read; tests put their own. */
export interface StopSystem {
  run: (bin: string, args: string[], env: NodeJS.ProcessEnv) => Promise<void>
  record: () => { pid: number; enrollEndpoint?: string } | null
  alive: (pid: number) => boolean
  shutdown: () => Promise<{ status: number; code?: string }>
  exitWaitMs: number
}

const realStopSystem = (): StopSystem => ({
  run: realInstallSystem(() => false).run,
  record: readExternalDaemonState,
  alive: processAlive,
  shutdown: elevatedShutdown,
  exitWaitMs: DAEMON_EXIT_WAIT_MS,
})

/** The real steps for the enrolled service daemon `pid` (null when not
 * enrolled), with the bundled CLI. */
export function serviceStopDeps(
  o: { cli: string; pid: number | null; spawned: (pid: number) => boolean; hold: StopDeps['hold']; record: () => void },
  sys: StopSystem = realStopSystem(),
): StopDeps {
  const pid = o.pid
  return {
    shutdown: pid === null ? null : sys.shutdown,
    unreachable: () => {
      const other = pid === null ? serviceRecordPid(sys.record(), o.spawned) : null
      return other !== null && sys.alive(other)
    },
    exited: () => until(() => pid === null || !sys.alive(pid), sys.exitWaitMs),
    uninstall: () => sys.run(o.cli, ['service', 'uninstall'], process.env),
    hold: o.hold,
    record: o.record,
  }
}

// ── M5 B4c2: the Settings switch ──────────────────────────────────────────

/** On: install (as the first-launch Allow), kept as allow. Off: stop (as
 * 「完全退出」 without the quit; it keeps declined). Already in that state:
 * nothing to do. A failed record is ignored (the plist says what is true). */
export async function setResident(on: boolean, deps: {
  installed: () => boolean
  install: () => Promise<InstallOutcome>
  stop: () => Promise<StopOutcome>
  record: (answer: ConsentAnswer) => void
}): Promise<{ ok: true } | { ok: false; stage: 'install' | 'start' | 'shutdown' | 'uninstall'; code: string }> {
  if (on) {
    if (deps.installed()) return { ok: true }
    let outcome: InstallOutcome
    try { outcome = await deps.install() } catch { return { ok: false, stage: 'install', code: 'install_failed' } }
    if (!outcome.ok) return { ok: false, stage: outcome.stage, code: outcome.code }
    try { deps.record('allow') } catch { /* the plist says it is on */ }
    return { ok: true }
  }
  if (!deps.installed()) return { ok: true }
  let outcome: StopOutcome
  try { outcome = await deps.stop() } catch { return { ok: false, stage: 'shutdown', code: 'shutdown_failed' } }
  return outcome.ok ? { ok: true } : { ok: false, stage: outcome.stage, code: outcome.code }
}

/** The tray's message for a failed 「完全退出」 (QA on B4c1): not connected;
 * stopped but the plist is still there; or not stopped at all. */
export function stopFailureKey(outcome: { stage: 'shutdown' | 'uninstall'; code: string }): 'tray.serviceNotConnected' | 'tray.serviceStoppedNotRemoved' | 'tray.serviceNotStopped' {
  if (outcome.code === 'not_connected') return 'tray.serviceNotConnected'
  if (outcome.stage === 'uninstall') return 'tray.serviceStoppedNotRemoved'
  return 'tray.serviceNotStopped'
}

// M5 B4c3 (PR-Daemon #1783 N1/N2): the install and the stop, whoever asks
// (the first-launch box, the Settings switch, the tray's 「完全退出」), run one
// at a time. One asked while the last one queued (or running) is the same kind
// gets that one's outcome, not a second presence box or uninstall; any other
// waits its turn, so the plist and the consent answer end as the last asked
// left them (a stop, an install, a stop: the third runs, after the install;
// R1 C1 and QA).
// `work` is called only when its turn comes, so what it reads (the enrolled
// pid) is read then.
export interface ResidentOutcomes { install: InstallOutcome; stop: StopOutcome }
export function residentOps(): <K extends keyof ResidentOutcomes>(op: K, work: () => Promise<ResidentOutcomes[K]>) => Promise<ResidentOutcomes[K]> {
  let tail: Promise<unknown> = Promise.resolve()
  let last: { op: keyof ResidentOutcomes; run: Promise<unknown> } | null = null
  return <K extends keyof ResidentOutcomes>(op: K, work: () => Promise<ResidentOutcomes[K]>) => {
    if (last?.op === op) return last.run as Promise<ResidentOutcomes[K]>
    const run: Promise<ResidentOutcomes[K]> = tail.then(() => work()).finally(() => { if (last?.run === run) last = null })
    last = { op, run }
    tail = run.catch(() => {})
    return run
  }
}

/** M5 B4c3 (Codex C2, R2 C1): an install is done once this app is enrolled
 * with the new service daemon and its presence connection is up (`enrolled`),
 * so that a stop right after it (queued, or the switch turned off again) has
 * the presence box to ask, instead of `not_connected` or `presence_unavailable`. Not enrolled within `ms` (refused,
 * slow), it is done anyway: the service is installed and running. */
export async function installedAndEnrolled(install: () => Promise<InstallOutcome>, enrolled: () => boolean, ms: number): Promise<InstallOutcome> {
  const outcome = await install()
  if (outcome.ok) await until(enrolled, ms)
  return outcome
}
