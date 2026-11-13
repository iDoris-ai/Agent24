// PR#1 F1 follow-up (rulings 2026-11-13-pr1-omlx-orphan-ownership, 乙,
// 2026-11-13-pr1-omlx-kept-deactivate, A, and
// 2026-11-13-pr1-remaining-lows-gate-f, L-b). A quit that leaves the oMLX
// this app started running (omlx-quit-policy.ts: the resident service uses
// it) records which process that is, in the data directory. A later launch
// does not own it (App.tsx finds it running and starts none), so a later quit
// with the service not installed (after 「完全退出」, after the Settings
// switch off, or with the plist removed outside the app) stops what is on
// record, and only when every part of the identity still matches: the same
// pid, started at the same time, with the same command, listening on the same
// port. A mismatch stops nothing and clears the record; a part that cannot be
// read (ps or lsof timed out or failed) is not a mismatch: nothing is
// signalled and the record stays for the next quit. The Settings switch off
// itself stops nothing (the app may be using it), nor does Settings' oMLX
// stop (a known limit).
import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'

/** One oMLX left running by a quit. */
export interface KeptOmlx {
  pid: number
  port: number
  /** `ps -o lstart=` when it was kept: tells a reused pid apart. */
  started: string
  /** `ps -o command=` when it was kept (oMLX retitles itself `omlx-server`). */
  command: string
}

/** A value read from the system, or `ok: false` when it could not be read
 *  (the tool timed out or failed): never the same as a value that differs. */
export type ProbeRead<T> = { ok: true; value: T } | { ok: false }

/** What the record and the stop read and do; tests put their own. */
export interface OmlxProcessProbe {
  /** `process.kill(pid, 0)`: 'yes' when it succeeds; 'gone' on ESRCH (no such
   *  process); 'other' on EPERM or anything else (a process of another user:
   *  not one this app started, whatever its pid). */
  exists: (pid: number) => 'yes' | 'gone' | 'other'
  /** `ps -o lstart= -p pid`, trimmed. */
  started: (pid: number) => ProbeRead<string>
  /** `ps -o command= -p pid`, trimmed. */
  command: (pid: number) => ProbeRead<string>
  /** Whether `pid` listens on TCP `port` (lsof); `value: false` when lsof
   *  found no such listener. */
  listensOn: (pid: number, port: number) => ProbeRead<boolean>
  /** process.kill(pid, signal); false when it threw. */
  signal: (pid: number, signal: NodeJS.Signals) => boolean
}

/** At most this many records are kept (the newest); in practice there is one:
 *  a launch that finds oMLX already running on 8088 starts none. */
export const MAX_KEPT_OMLX = 2

export const KEPT_OMLX_FORMAT = 'agent24.desktop.omlx-kept'
export const KEPT_OMLX_VERSION = 1

/** `<dataDir>/omlx-kept.json`. */
export function keptOmlxPath(dataDir: string): string {
  return path.join(dataDir, 'omlx-kept.json')
}

function parseKept(value: unknown): KeptOmlx | null {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return null
  const entry = value as { pid?: unknown; port?: unknown; started?: unknown; command?: unknown }
  if (typeof entry.pid !== 'number' || !Number.isInteger(entry.pid)) return null
  if (typeof entry.port !== 'number' || !Number.isInteger(entry.port)) return null
  if (typeof entry.started !== 'string' || typeof entry.command !== 'string') return null
  return { pid: entry.pid, port: entry.port, started: entry.started, command: entry.command }
}

/** The records in `file`; [] when it is missing, unreadable, of another
 * format or version, or any entry is malformed (the whole file is then
 * ignored: a stop then stops nothing). */
export function readKeptOmlx(file: string): KeptOmlx[] {
  try {
    const value: unknown = JSON.parse(fs.readFileSync(file, 'utf8'))
    if (typeof value !== 'object' || value === null || Array.isArray(value)) return []
    const body = value as { format?: unknown; version?: unknown; kept?: unknown }
    if (body.format !== KEPT_OMLX_FORMAT || body.version !== KEPT_OMLX_VERSION || !Array.isArray(body.kept)) return []
    const kept: KeptOmlx[] = []
    for (const entry of body.kept) {
      const row = parseKept(entry)
      if (row === null) return []
      kept.push(row)
    }
    return kept
  } catch {
    return []
  }
}

/** Writes `kept` (the newest {@link MAX_KEPT_OMLX}) to `file` atomically
 *  (temp file + rename), mode 0600, its directory created; an empty list
 *  removes the file. Throws on failure; callers catch. */
function writeKeptOmlx(file: string, kept: KeptOmlx[]): void {
  const rows = kept.slice(-MAX_KEPT_OMLX)
  if (rows.length === 0) {
    fs.rmSync(file, { force: true })
    return
  }
  const temp = path.join(path.dirname(file), `.omlx-kept.${process.pid}.tmp`)
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true })
    fs.rmSync(temp, { force: true })
    const fd = fs.openSync(temp, 'wx', 0o600)
    try {
      fs.writeFileSync(fd, JSON.stringify({ format: KEPT_OMLX_FORMAT, version: KEPT_OMLX_VERSION, kept: rows }))
      fs.fchmodSync(fd, 0o600)
    } finally {
      fs.closeSync(fd)
    }
    fs.renameSync(temp, file)
  } catch (err) {
    try { fs.rmSync(temp, { force: true }) } catch { /* the write already failed */ }
    throw err
  }
}

/** Adds `row` to `file`, replacing any record with the same pid; the others
 *  stay (they survive any number of launches). Throws on failure. */
function addKeptOmlx(file: string, row: KeptOmlx): void {
  const kept = readKeptOmlx(file)
  const index = kept.findIndex((k) => k.pid === row.pid)
  if (index >= 0) kept[index] = row
  else kept.push(row)
  writeKeptOmlx(file, kept)
}

/** At a quit that keeps `child` running: read its identity now through
 * `probe` and add it to `file` (see {@link addKeptOmlx}). When the identity
 * cannot be read, nothing is written. Never throws: a failure is logged and
 * the quit goes on. */
export function rememberKeptOmlx(file: string, child: { pid: number; port: number }, probe: OmlxProcessProbe): void {
  try {
    const started = probe.started(child.pid)
    const command = probe.command(child.pid)
    if (!started.ok || !command.ok) return
    addKeptOmlx(file, { pid: child.pid, port: child.port, started: started.value, command: command.value })
  } catch (err) {
    console.warn('[omlx-kept] could not record the oMLX left running', err)
  }
}

export interface KeptStopResult {
  /** Records whose identity matched and SIGTERM went out. */
  stopped: number[]
  /** Records not signalled and cleared: gone, another user's, reused pid,
   *  other command, other port, or the SIGTERM itself failed. */
  dropped: number[]
  /** Records not signalled (or not killed) because a part of the identity
   *  could not be read: kept on record for the next quit. */
  kept: number[]
}

/** Poll until `pid` is no longer this user's process or `ms` has passed. The
 *  gap between looks is at most 50ms. */
async function waitUntilGone(probe: OmlxProcessProbe, pid: number, ms: number): Promise<void> {
  const deadline = Date.now() + ms
  while (probe.exists(pid) === 'yes') {
    const left = deadline - Date.now()
    if (left <= 0) return
    await new Promise<void>((resolve) => { setTimeout(resolve, Math.min(50, left)) })
  }
}

type Check = 'match' | 'mismatch' | 'unread'

function check(probe: OmlxProcessProbe, k: KeptOmlx): Check {
  if (probe.exists(k.pid) !== 'yes') return 'mismatch'
  const started = probe.started(k.pid)
  if (!started.ok) return 'unread'
  if (started.value !== k.started) return 'mismatch'
  const command = probe.command(k.pid)
  if (!command.ok) return 'unread'
  if (command.value !== k.command) return 'mismatch'
  const listens = probe.listensOn(k.pid, k.port)
  if (!listens.ok) return 'unread'
  return listens.value ? 'match' : 'mismatch'
}

/** At a quit with the resident service not installed (its plist gone: after
 * 「完全退出」, after the Settings switch off, or removed outside the app):
 * for each record (the newest {@link MAX_KEPT_OMLX}), check the identity:
 * a match is to be stopped; a mismatch is dropped without a signal; a part
 * that could not be read keeps the record, unsignalled, for the next quit
 * (a timeout is not a mismatch). Right after these checks, before any signal,
 * the file is rewritten to hold only the kept records (removed when none), so
 * what it holds does not depend on how long the stop takes (a quit's budget
 * may cut the wait short; what then survives is not tried again). Then, for
 * all matched records at once: SIGTERM; when still this user's process after
 * `graceMs`, and its started still reads the same, SIGKILL, then wait at most
 * `killMs` more; when that started cannot be read, no SIGKILL and the record
 * goes back on file. The checks are synchronous (ps, lsof) and run before the
 * caller's budget starts: at most 2 records × 2 s in the worst case (each ps
 * 0.5 s, lsof 1 s, when they time out), plus one ps per record before a
 * SIGKILL. Never throws. */
export async function stopKeptOmlx(
  file: string,
  probe: OmlxProcessProbe,
  timing: { graceMs: number; killMs: number },
): Promise<KeptStopResult> {
  const stopped: number[] = []
  const dropped: number[] = []
  const kept: number[] = []
  const matched: KeptOmlx[] = []
  const retry: KeptOmlx[] = []
  const all = readKeptOmlx(file)
  const looked = all.slice(-MAX_KEPT_OMLX)
  for (const k of all.slice(0, all.length - looked.length)) dropped.push(k.pid)
  for (const k of looked) {
    let result: Check
    try {
      result = check(probe, k)
    } catch {
      result = 'unread'
    }
    if (result === 'match') matched.push(k)
    else if (result === 'unread') { retry.push(k); kept.push(k.pid) }
    else dropped.push(k.pid)
  }
  try {
    writeKeptOmlx(file, retry)
  } catch (err) {
    console.warn('[omlx-kept] could not rewrite the record of the oMLX left running', err)
  }
  await Promise.all(matched.map(async (k) => {
    try {
      if (!probe.signal(k.pid, 'SIGTERM')) {
        dropped.push(k.pid)
        return
      }
      stopped.push(k.pid)
      await waitUntilGone(probe, k.pid, timing.graceMs)
      if (probe.exists(k.pid) !== 'yes') return
      const started = probe.started(k.pid)
      if (!started.ok) {
        // Not killed on a start time that could not be read: back on file.
        kept.push(k.pid)
        try { addKeptOmlx(file, k) } catch (err) { console.warn('[omlx-kept] could not keep the record of the oMLX left running', err) }
        return
      }
      if (started.value === k.started) {
        probe.signal(k.pid, 'SIGKILL')
        await waitUntilGone(probe, k.pid, timing.killMs)
      }
    } catch {
      // a probe failure: what was decided stays
    }
  }))
  return { stopped, dropped, kept }
}

/** `ps` reads in a fixed zone and locale, so the start time recorded at one
 *  launch reads the same at a later one (a changed TZ or LC_TIME otherwise
 *  changes the text). Short timeout: it runs synchronously in before-quit. */
function psEnv(): NodeJS.ProcessEnv {
  return { ...process.env, TZ: 'UTC', LC_ALL: 'C' }
}
const PS_TIMEOUT_MS = 500
const LSOF_TIMEOUT_MS = 1_000

function psValue(pid: number, column: 'lstart=' | 'command='): ProbeRead<string> {
  try {
    const out = execFileSync('/bin/ps', ['-o', column, '-p', String(pid)], { encoding: 'utf8', timeout: PS_TIMEOUT_MS, env: psEnv() })
    const text = out.trim()
    // Asked only after kill(pid, 0) succeeded: an empty answer is not "gone".
    return text === '' ? { ok: false } : { ok: true, value: text }
  } catch {
    return { ok: false }
  }
}

/** lsof's own "nothing matched": exit status 1, not killed, no output. */
function lsofFoundNothing(err: unknown): boolean {
  const e = err as { status?: unknown; signal?: unknown; stdout?: unknown }
  const out = typeof e.stdout === 'string' ? e.stdout : Buffer.isBuffer(e.stdout) ? e.stdout.toString() : ''
  return e.status === 1 && (e.signal === null || e.signal === undefined) && out.trim() === ''
}

/** The real probe: `kill(pid, 0)`, `ps` and `lsof` through execFileSync, each
 *  bounded by a short timeout. */
export function realOmlxProcessProbe(): OmlxProcessProbe {
  return {
    exists: (pid) => {
      try {
        process.kill(pid, 0)
        return 'yes'
      } catch (err) {
        return (err as { code?: unknown }).code === 'ESRCH' ? 'gone' : 'other'
      }
    },
    started: (pid) => psValue(pid, 'lstart='),
    command: (pid) => psValue(pid, 'command='),
    listensOn: (pid, port) => {
      try {
        const out = execFileSync('/usr/sbin/lsof', ['-nP', '-a', '-p', String(pid), `-iTCP:${port}`, '-sTCP:LISTEN', '-t'], { encoding: 'utf8', timeout: LSOF_TIMEOUT_MS })
        return { ok: true, value: out.split('\n').some((line) => line.trim() === String(pid)) }
      } catch (err) {
        return lsofFoundNothing(err) ? { ok: true, value: false } : { ok: false }
      }
    },
    signal: (pid, signal) => {
      try {
        process.kill(pid, signal)
        return true
      } catch {
        return false
      }
    },
  }
}
