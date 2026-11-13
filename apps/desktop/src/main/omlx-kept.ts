// PR#1 F1 follow-up (rulings 2026-11-13-pr1-omlx-orphan-ownership, 乙, and
// 2026-11-13-pr1-omlx-kept-deactivate, A). A quit that leaves the oMLX this
// app started running (omlx-quit-policy.ts: the resident service uses it)
// records which process that is, in the data directory. A later launch does
// not own it (App.tsx finds it running and starts none), so a later quit with
// the service not installed (after 「完全退出」, after the Settings switch
// off, or with the plist removed outside the app) stops what is on record,
// and only when every part of the identity still matches: the same pid,
// started at the same time, with the same command, listening on the same
// port. Any mismatch stops nothing; the record is cleared either way. The
// Settings switch off itself stops nothing (the app may be using it), nor
// does Settings' oMLX stop (a known limit).
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

/** What the record and the stop read and do; tests put their own. */
export interface OmlxProcessProbe {
  /** `ps -o lstart= -p pid`, trimmed; null when there is no such process. */
  started: (pid: number) => string | null
  /** `ps -o command= -p pid`, trimmed; null when there is no such process. */
  command: (pid: number) => string | null
  /** Whether `pid` listens on 127.0.0.1:`port` (lsof). */
  listensOn: (pid: number, port: number) => boolean
  /** process.kill(pid, 0) succeeds. */
  alive: (pid: number) => boolean
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

/** At a quit that keeps `child` running: read its identity now through
 * `probe` and add it to `file`, replacing any record with the same pid; other
 * records stay (they survive any number of launches). When the identity
 * cannot be read (started or command null) nothing is written. Written
 * atomically (temp file + rename), mode 0600, its directory created. Never
 * throws: a failure is logged and the quit goes on. */
export function rememberKeptOmlx(file: string, child: { pid: number; port: number }, probe: OmlxProcessProbe): void {
  const temp = path.join(path.dirname(file), `.omlx-kept.${process.pid}.tmp`)
  try {
    const started = probe.started(child.pid)
    const command = probe.command(child.pid)
    if (started === null || command === null) return
    const kept = readKeptOmlx(file)
    const next: KeptOmlx = { pid: child.pid, port: child.port, started, command }
    const index = kept.findIndex((row) => row.pid === child.pid)
    if (index >= 0) kept[index] = next
    else kept.push(next)
    while (kept.length > MAX_KEPT_OMLX) kept.shift()
    fs.mkdirSync(path.dirname(file), { recursive: true })
    fs.rmSync(temp, { force: true })
    const fd = fs.openSync(temp, 'wx', 0o600)
    try {
      fs.writeFileSync(fd, JSON.stringify({ format: KEPT_OMLX_FORMAT, version: KEPT_OMLX_VERSION, kept }))
      fs.fchmodSync(fd, 0o600)
    } finally {
      fs.closeSync(fd)
    }
    fs.renameSync(temp, file)
  } catch (err) {
    try { fs.rmSync(temp, { force: true }) } catch { /* the write already failed */ }
    console.warn('[omlx-kept] could not record the oMLX left running', err)
  }
}

export interface KeptStopResult {
  /** Records whose identity matched and SIGTERM went out. */
  stopped: number[]
  /** Records not signalled: gone, reused pid, other command, other port, or
   *  the SIGTERM itself failed. */
  dropped: number[]
}

/** Poll until `pid` is gone or `ms` has passed. The gap between looks is at most 50ms. */
async function waitUntilGone(probe: OmlxProcessProbe, pid: number, ms: number): Promise<void> {
  const deadline = Date.now() + ms
  while (probe.alive(pid)) {
    const left = deadline - Date.now()
    if (left <= 0) return
    await new Promise<void>((resolve) => { setTimeout(resolve, Math.min(50, left)) })
  }
}

/** At a quit with the resident service not installed (its plist gone: after
 * 「完全退出」, after the Settings switch off, or removed outside the app):
 * for each record (the newest {@link MAX_KEPT_OMLX}), when `probe` still reads
 * the same started, the same command and a listener on the same port for that
 * pid, it is to be stopped; any other record is dropped without a signal. The
 * file is removed right after these checks, before any signal, so whether it
 * is cleared does not depend on how long the stop takes (a quit's budget may
 * cut the wait short; what then survives is not tried again). Then, for all
 * matched records at once: SIGTERM; when still alive after `graceMs`, and its
 * started still matches, SIGKILL; then wait at most `killMs` more. The checks
 * are synchronous (ps, lsof) and run before the caller's budget starts: at
 * most 2 records × 2 s in the worst case (each ps 0.5 s, lsof 1 s, when they
 * time out), plus one ps per record before a SIGKILL. A check that times out
 * reads as a mismatch. Never throws. */
export async function stopKeptOmlx(
  file: string,
  probe: OmlxProcessProbe,
  timing: { graceMs: number; killMs: number },
): Promise<KeptStopResult> {
  const stopped: number[] = []
  const dropped: number[] = []
  const matched: KeptOmlx[] = []
  const all = readKeptOmlx(file)
  const looked = all.slice(-MAX_KEPT_OMLX)
  for (const k of all.slice(0, all.length - looked.length)) dropped.push(k.pid)
  for (const k of looked) {
    try {
      if (probe.started(k.pid) === k.started && probe.command(k.pid) === k.command && probe.listensOn(k.pid, k.port)) matched.push(k)
      else dropped.push(k.pid)
    } catch {
      dropped.push(k.pid)
    }
  }
  try {
    fs.rmSync(file, { force: true })
  } catch {
    // never throws
  }
  await Promise.all(matched.map(async (k) => {
    try {
      if (!probe.signal(k.pid, 'SIGTERM')) {
        dropped.push(k.pid)
        return
      }
      stopped.push(k.pid)
      await waitUntilGone(probe, k.pid, timing.graceMs)
      if (probe.alive(k.pid) && probe.started(k.pid) === k.started) {
        probe.signal(k.pid, 'SIGKILL')
        await waitUntilGone(probe, k.pid, timing.killMs)
      }
    } catch {
      // a probe failure: what was decided stays
    }
  }))
  return { stopped, dropped }
}

/** `ps` reads in a fixed zone and locale, so the start time recorded at one
 *  launch reads the same at a later one (a changed TZ or LC_TIME otherwise
 *  changes the text). Short timeout: it runs synchronously in before-quit. */
function psEnv(): NodeJS.ProcessEnv {
  return { ...process.env, TZ: 'UTC', LC_ALL: 'C' }
}
const PS_TIMEOUT_MS = 500
const LSOF_TIMEOUT_MS = 1_000

function psValue(pid: number, column: 'lstart=' | 'command='): string | null {
  try {
    const out = execFileSync('/bin/ps', ['-o', column, '-p', String(pid)], { encoding: 'utf8', timeout: PS_TIMEOUT_MS, env: psEnv() })
    const text = out.trim()
    return text === '' ? null : text
  } catch {
    return null
  }
}

/** The real probe: `ps` and `lsof` through execFileSync, each bounded by a
 * short timeout; any failure reads as "no such process" / false. */
export function realOmlxProcessProbe(): OmlxProcessProbe {
  return {
    started: (pid) => psValue(pid, 'lstart='),
    command: (pid) => psValue(pid, 'command='),
    listensOn: (pid, port) => {
      try {
        const out = execFileSync('/usr/sbin/lsof', ['-nP', '-a', '-p', String(pid), `-iTCP:${port}`, '-sTCP:LISTEN', '-t'], { encoding: 'utf8', timeout: LSOF_TIMEOUT_MS })
        return out.split('\n').some((line) => line.trim() === String(pid))
      } catch {
        return false
      }
    },
    alive: (pid) => {
      try {
        process.kill(pid, 0)
        return true
      } catch {
        return false
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
