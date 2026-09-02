// Crash-safe persistence for the WeChat-user → agent24-session map.
//
// Extracted from main.ts so it is testable: main.ts is an entry point that logs
// in and starts a long poll, so nothing in it could be exercised.
//
// Why this file gets this much care: it is the ONLY record of which agent24
// session belongs to which WeChat user. Losing it silently drops every user's
// conversation context, and the bridge is meant to run unattended for days (F5)
// — exactly when ungraceful shutdowns happen.
//
// Four properties, in the order they matter:
//   1. A reader never sees a partial file      → write-to-temp + rename
//   2. A completed save survives power loss    → fsync temp, then fsync dir
//   3. One bad generation is recoverable       → .bak, rotated ONLY when valid
//   4. Recovery restores redundancy            → load() republishes what it recovered
//
// What this file does NOT do, deliberately, with the reasons:
//   - It is NOT safe against two bridge processes writing concurrently. Unique
//     temp names stop one from publishing the other's half-written file, but
//     both can still publish from a stale read and lose an update. Two bridges
//     is not a supported configuration (they would answer every WeChat message
//     twice, which is a louder problem than a lost mapping). A singleton lock is
//     the real fix and is tracked as FU-30 — not bolted on here, because a
//     stale-lock bug would keep the bridge from starting at all after a crash,
//     which is a WORSE unattended failure than the one this file is fixing.
//   - `fsync` on macOS does not guarantee the drive flushed its own write cache;
//     that needs F_FULLFSYNC, which Node does not expose. This closes the page
//     cache window. It is not "power-loss proof" and must not be described that
//     way anywhere (FU-31).

import fs from 'node:fs'
import path from 'node:path'
import type { SessionStore } from './bridge.js'

/** Directory-fsync failures that mean "this platform/filesystem does not offer
 * it", as opposed to a storage error that the operator needs to hear about. */
const DIR_FSYNC_UNSUPPORTED = new Set(['ENOTSUP', 'EOPNOTSUPP', 'EINVAL', 'EISDIR', 'EACCES', 'EPERM'])

export class FileSessionStore implements SessionStore {
  constructor(private readonly file: string) {}

  private get bak(): string {
    return `${this.file}.bak`
  }

  /** Read the map: the real file, else the last known-good backup.
   *
   * A leftover temp is NOT promoted. To be precise about why — the earlier
   * comment here overstated it, and overstating a guarantee is the exact defect
   * this repo keeps finding in review: a temp CAN be complete and fsynced, and
   * merely have died before its `rename`. It is skipped not because it is
   * necessarily incomplete, but because **`rename` is the commit point**. A
   * generation that was never committed is not a generation, and promoting one
   * of unknown age would make "what does this file contain" depend on how a
   * process happened to die. Stale temps are swept instead (see `sweepTemps`).
   *
   * Recovering from `.bak` REPUBLISHES it as main. Without that, an install that
   * once fell back keeps running on a single copy forever — the next corruption
   * loses everything, and nothing would have warned anyone. (Codex round 2.) */
  load(): Map<string, string> {
    this.sweepTemps()
    for (const candidate of [this.file, this.bak]) {
      let text: string
      try {
        text = fs.readFileSync(candidate, 'utf8')
      } catch {
        continue // absent — normal for .bak before the second save
      }
      const parsed = parseMap(text)
      if (parsed) {
        if (candidate !== this.file) {
          console.warn(`[wechat] 主会话映射不可读,已从 ${path.basename(candidate)} 恢复`)
          this.republish(parsed) // restore redundancy, don't just limp along
        }
        return parsed
      }
      console.warn(`[wechat] ${path.basename(candidate)} 内容损坏,尝试下一个来源`)
    }
    return new Map()
  }

  /** Publish a new generation atomically and durably. */
  save(map: Map<string, string>): void {
    this.rotateBackupIfValid()
    this.republish(map)
  }

  /** The write half of `save`, also used by `load` to repair a recovered
   * generation. Never rotates the backup — the caller decides that. */
  private republish(map: Map<string, string>): void {
    const tmp = `${this.file}.${process.pid}.tmp`
    try {
      fs.mkdirSync(path.dirname(this.file), { recursive: true, mode: 0o700 })
      writeFileDurable(tmp, JSON.stringify(Object.fromEntries(map), null, 2))
      fs.renameSync(tmp, this.file)
      fsyncDir(path.dirname(this.file)) // make the rename itself durable
    } catch (err) {
      try {
        fs.unlinkSync(tmp)
      } catch {
        /* already gone */
      }
      console.error('[wechat] 保存会话映射失败:', err instanceof Error ? err.message : err)
    }
  }

  /** Promote the current main file to `.bak` — but ONLY if it parses.
   *
   * Rotating an unvalidated main is how a backup gets destroyed in exactly the
   * situation it exists for (Codex round 1, High).
   *
   * The copy itself goes through a temp + rename for the same reason the main
   * write does: `copyFileSync` truncates the destination first, so a failure
   * partway leaves `.bak` damaged AND lets the save proceed — one interrupted
   * copy and there is nothing left to recover from. (Codex round 2, Medium.) */
  private rotateBackupIfValid(): void {
    let text: string
    try {
      text = fs.readFileSync(this.file, 'utf8')
    } catch {
      return // no main yet (first save) — nothing to back up
    }
    if (!parseMap(text)) {
      console.warn('[wechat] 主会话映射不可读,保留现有备份不覆盖')
      return
    }
    const tmp = `${this.bak}.${process.pid}.tmp`
    try {
      fs.mkdirSync(path.dirname(this.file), { recursive: true, mode: 0o700 })
      writeFileDurable(tmp, text)
      fs.renameSync(tmp, this.bak)
    } catch (err) {
      // The old .bak is still intact — the temp never made it over. Say so:
      // silently continuing would leave the operator believing there is a
      // backup generation behind the one about to be written.
      try {
        fs.unlinkSync(tmp)
      } catch {
        /* already gone */
      }
      console.warn(
        '[wechat] 备份轮转失败,保留上一代备份(本次写入不会有对应的回退代):',
        err instanceof Error ? err.message : err,
      )
    }
  }

  /** Remove temps this store left behind that no longer belong to a live write.
   *
   * A SIGKILL or power cut bypasses `republish`'s cleanup, so without this one
   * orphan accumulates per crashed PID, forever. Only files matching this
   * store's own basename pattern are touched, and never the current process's
   * temp — a concurrent write of ours must not be deleted out from under it. */
  private sweepTemps(): void {
    const dir = path.dirname(this.file)
    const base = path.basename(this.file)
    const mine = `${base}.${process.pid}.tmp`
    let entries: string[]
    try {
      entries = fs.readdirSync(dir)
    } catch {
      return // directory not there yet — nothing to sweep
    }
    const pattern = new RegExp(`^${escapeRegExp(base)}(\\.bak)?\\.\\d+\\.tmp$`)
    for (const name of entries) {
      if (name === mine || !pattern.test(name)) continue
      try {
        fs.unlinkSync(path.join(dir, name))
      } catch {
        /* another process may have just removed it */
      }
    }
  }
}

/** Write bytes and fsync them before anyone can rename the file into place. */
function writeFileDurable(file: string, content: string): void {
  const fd = fs.openSync(file, 'w', 0o600)
  try {
    fs.writeFileSync(fd, content, 'utf8')
    fs.fsyncSync(fd)
  } finally {
    fs.closeSync(fd)
  }
}

/** Parse the on-disk shape, or null if it is not a usable generation. */
function parseMap(text: string): Map<string, string> | null {
  let raw: unknown
  try {
    raw = JSON.parse(text)
  } catch {
    return null
  }
  // `JSON.parse` accepts `null`, `[]`, `"x"` and numbers — none of which are a
  // session map. Object.entries would happily produce nonsense from an array.
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) return null
  const entries = Object.entries(raw as Record<string, unknown>)
  if (entries.some(([, v]) => typeof v !== 'string')) return null
  return new Map(entries as [string, string][])
}

/** fsync a directory so a rename inside it is durable.
 *
 * Opening a directory for fsync is POSIX behaviour and simply does not work on
 * some platforms — that case is expected and quiet. Anything else (EIO, ENOSPC)
 * is a storage problem, and swallowing it would let `save()` return as if the
 * rename were durable when it may not be. We do not fail the save (the rename
 * already happened; the data is published either way) but the operator hears
 * about it. (Codex round 2, Medium.) */
function fsyncDir(dir: string): void {
  let fd: number | undefined
  try {
    fd = fs.openSync(dir, 'r')
    fs.fsyncSync(fd)
  } catch (err) {
    const code = (err as NodeJS.ErrnoException)?.code
    if (!code || !DIR_FSYNC_UNSUPPORTED.has(code)) {
      console.warn(
        `[wechat] 目录 fsync 失败(${code ?? 'unknown'}),本次写入的持久性未经确认:`,
        err instanceof Error ? err.message : err,
      )
    }
  } finally {
    if (fd !== undefined) {
      try {
        fs.closeSync(fd)
      } catch {
        /* ignore */
      }
    }
  }
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}
