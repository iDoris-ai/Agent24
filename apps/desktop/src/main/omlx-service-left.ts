// PR#1 F1 follow-up (ruling 2026-11-13-pr1-remaining-lows-gate-f, L-a). A
// service stop (「完全退出」 or the Settings switch off) whose shutdown went
// through (the daemon exited; the uninstall's bootout ran first) but whose
// uninstall then failed with the LaunchAgent's plist still there leaves a
// plist with no service daemon behind it. A quit reads that plist as "the
// resident service is installed" and leaves oMLX running for a daemon that is
// not there (omlx-quit-policy.ts). So such a stop records the plist it left
// (inode and mtime) in the data directory, and a quit treats that very plist
// as not installed. The record is cleared, and the plist read as installed
// again, as soon as any of these holds at a quit: the plist is gone (a later
// stop removed it), it was written again (an install rewrites it in place,
// service.rs write_install_files `std::fs::write`: same inode, new mtime), or
// a service daemon is up again (launchd started it from that plist at the
// next login).
import fs from 'node:fs'
import path from 'node:path'
import type { ExternalDaemonState } from './backend-manager'

export const SERVICE_LEFT_FORMAT = 'agent24.desktop.service-left'
export const SERVICE_LEFT_VERSION = 1

/** `<dataDir>/service-left.json`. */
export function serviceLeftPath(dataDir: string): string {
  return path.join(dataDir, 'service-left.json')
}

/** The LaunchAgent's plist, as backend-manager.ts launchAgentPlistExists reads
 *  it (same HOME, same name). */
export function launchAgentPlistPath(env: NodeJS.ProcessEnv = process.env): string | null {
  const home = env['HOME']
  return home === undefined ? null : path.join(home, 'Library', 'LaunchAgents', 'ai.auraai.agent24.plist')
}

interface PlistId { ino: number; mtimeMs: number }

function plistId(plist: string): PlistId | null {
  try {
    const st = fs.statSync(plist)
    return { ino: st.ino, mtimeMs: st.mtimeMs }
  } catch {
    return null
  }
}

function readLeft(file: string): PlistId | null {
  try {
    const value = JSON.parse(fs.readFileSync(file, 'utf8')) as { format?: unknown; version?: unknown; plist?: { ino?: unknown; mtimeMs?: unknown } }
    if (value.format !== SERVICE_LEFT_FORMAT || value.version !== SERVICE_LEFT_VERSION) return null
    const { ino, mtimeMs } = value.plist ?? {}
    if (typeof ino !== 'number' || typeof mtimeMs !== 'number') return null
    return { ino, mtimeMs }
  } catch {
    return null
  }
}

/** Whether the daemon on record can be the service's, up now: not this app's
 * own child, either with an enrolment socket (as serviceRecordPid in
 * resident-service.ts) or a record this app refused to parse (a newer daemon:
 * still a daemon that may be using oMLX), and alive. A legacy record (no
 * socket) is not the service's. */
export function serviceDaemonUp(
  state: Pick<ExternalDaemonState, 'pid' | 'enrollEndpoint' | 'rejected'> | null,
  spawned: (pid: number) => boolean,
  alive: (pid: number) => boolean,
): boolean {
  if (state === null || spawned(state.pid)) return false
  if (state.enrollEndpoint === undefined && state.rejected === undefined) return false
  return alive(state.pid)
}

function clear(file: string): void {
  try { fs.rmSync(file, { force: true }) } catch { /* the next quit tries again */ }
}

/** After a stop whose shutdown went through and whose uninstall failed: when
 * `plist` is still there, record which file it is (atomic, 0600). When it is
 * gone (only the daemon copy could not be removed), nothing is recorded.
 * Never throws. */
export function markServiceLeft(file: string, plist: string | null): void {
  const id = plist === null ? null : plistId(plist)
  if (id === null) return
  const temp = path.join(path.dirname(file), `.service-left.${process.pid}.tmp`)
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true })
    fs.rmSync(temp, { force: true })
    const fd = fs.openSync(temp, 'wx', 0o600)
    try {
      fs.writeFileSync(fd, JSON.stringify({ format: SERVICE_LEFT_FORMAT, version: SERVICE_LEFT_VERSION, plist: id }))
      fs.fchmodSync(fd, 0o600)
    } finally {
      fs.closeSync(fd)
    }
    fs.renameSync(temp, file)
  } catch (err) {
    try { fs.rmSync(temp, { force: true }) } catch { /* the write already failed */ }
    console.warn('[service-left] could not record the plist a stop left behind', err)
  }
}

/** At a quit: whether `plist` is the one a stop left behind with no service
 * daemon. False (and the record cleared) when there is no record, the record
 * cannot be read, the plist is gone or is another file, or `serviceDaemonUp`
 * says a service daemon runs again. Never throws. */
export function serviceLeftBehind(file: string, plist: string | null, serviceDaemonUp: () => boolean): boolean {
  if (!fs.existsSync(file)) return false
  const left = readLeft(file)
  const now = plist === null ? null : plistId(plist)
  let up = true
  try { up = serviceDaemonUp() } catch { up = true }
  if (left === null || now === null || now.ino !== left.ino || now.mtimeMs !== left.mtimeMs || up) {
    clear(file)
    return false
  }
  return true
}
