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
// Three properties, in the order they matter:
//   1. A reader never sees a partial file        → write-to-temp + rename
//   2. A completed save survives power loss      → fsync temp, then fsync dir
//   3. One bad generation is recoverable         → .bak, rotated ONLY when valid

import fs from 'node:fs'
import path from 'node:path'
import type { SessionStore } from './bridge.js'

export class FileSessionStore implements SessionStore {
  constructor(private readonly file: string) {}

  private get bak(): string {
    return `${this.file}.bak`
  }

  /** Read the map: the real file, else the last known-good backup.
   *
   * A leftover `.tmp` is deliberately NOT consulted. It exists only because a
   * write did not reach its `rename`, so it is incomplete by definition — newer
   * in intent, but not a valid generation. `save()` publishes atomically, so the
   * main file is always a whole generation; if it is unreadable, the damage came
   * from outside us and `.bak` is the better source, not a truncated `.tmp`. */
  load(): Map<string, string> {
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
        }
        return parsed
      }
      console.warn(`[wechat] ${path.basename(candidate)} 内容损坏,尝试下一个来源`)
    }
    return new Map()
  }

  /** Publish a new generation atomically and durably.
   *
   * Backup rotation is CONDITIONAL — this is the subtle part. Copying the main
   * file into `.bak` unconditionally destroys the backup in precisely the
   * situation it exists for: main is corrupt, `load()` just recovered from
   * `.bak`, and the next save would overwrite that good `.bak` with the corrupt
   * main. Two saves later there is nothing left to recover. So: a generation is
   * promoted to `.bak` only after it is confirmed readable.
   *
   * Durability note, stated honestly: `fsyncSync` asks the OS to flush, but on
   * macOS it does NOT guarantee the drive has flushed its own write cache —
   * that needs `F_FULLFSYNC`, which Node does not expose. This closes the window
   * where the data sits only in the OS page cache; it does not make the write
   * survive every possible power cut. */
  save(map: Map<string, string>): void {
    // A unique temp name keeps two bridge processes from clobbering each other's
    // in-flight write (they would still race on the rename, but neither can
    // publish a file the other was halfway through writing).
    const tmp = `${this.file}.${process.pid}.tmp`
    try {
      fs.mkdirSync(path.dirname(this.file), { recursive: true, mode: 0o700 })
      this.rotateBackupIfValid()

      const json = JSON.stringify(Object.fromEntries(map), null, 2)
      const fd = fs.openSync(tmp, 'w', 0o600)
      try {
        fs.writeFileSync(fd, json, 'utf8')
        fs.fsyncSync(fd) // the bytes, before the rename that publishes them
      } finally {
        fs.closeSync(fd)
      }
      fs.renameSync(tmp, this.file)
      fsyncDir(path.dirname(this.file)) // the rename itself
    } catch (err) {
      // Leave nothing behind for `load()` to trip over, then report. The main
      // file is untouched — a failed save loses the update, never the data.
      try {
        fs.unlinkSync(tmp)
      } catch {
        /* already gone */
      }
      console.error('[wechat] 保存会话映射失败:', err instanceof Error ? err.message : err)
    }
  }

  /** Promote the current main file to `.bak` — but ONLY if it parses. See the
   * `save()` doc: rotating an unvalidated main is how a backup gets destroyed. */
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
    try {
      fs.copyFileSync(this.file, this.bak)
    } catch {
      // A failed backup must not block the write: the rename is what protects
      // the data, the backup is only a second chance.
    }
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

/** fsync a directory so a rename inside it is durable. Best-effort: opening a
 * directory for fsync is POSIX behaviour and throws on Windows. */
function fsyncDir(dir: string): void {
  let fd: number | undefined
  try {
    fd = fs.openSync(dir, 'r')
    fs.fsyncSync(fd)
  } catch {
    /* not supported here — the rename is still ordered, just less durable */
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
