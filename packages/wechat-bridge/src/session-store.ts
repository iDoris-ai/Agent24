// Crash-safe persistence for the WeChat-user → agent24-session map.
//
// Extracted from main.ts so it is testable: main.ts is an entry point that
// logs in and starts a long poll, so nothing in it could be exercised.
//
// Why atomic writes matter here specifically: this file is the ONLY record of
// which agent24 session belongs to which WeChat user. A plain `writeFileSync`
// truncates before writing, so an ungraceful shutdown mid-write leaves a
// truncated file that no longer parses — and every user silently loses their
// conversation context. The bridge is meant to run unattended for days (F5),
// which is exactly the situation where ungraceful shutdowns happen.

import fs from 'node:fs'
import path from 'node:path'
import type { SessionStore } from './bridge.js'

export class FileSessionStore implements SessionStore {
  constructor(private readonly file: string) {}

  /** Read the map, falling back through the artifacts an interrupted write can
   * leave behind.
   *
   * Order is deliberate: the real file first; then `.tmp`, which exists only if
   * a write died between `writeFileSync` and `rename` (so it is NEWER than the
   * real file); then `.bak`, the previous good copy. A truncated candidate
   * throws on `JSON.parse` and falls through rather than winning. */
  load(): Map<string, string> {
    for (const candidate of [this.file, `${this.file}.tmp`, `${this.file}.bak`]) {
      let text: string
      try {
        text = fs.readFileSync(candidate, 'utf8')
      } catch {
        continue // absent — normal for .tmp/.bak
      }
      try {
        const raw = JSON.parse(text) as Record<string, string>
        if (candidate !== this.file) {
          console.warn(`[wechat] 主会话映射不可读,已从 ${path.basename(candidate)} 恢复`)
        }
        return new Map(Object.entries(raw))
      } catch {
        // Present but corrupt (the truncation case) — say so, then fall through.
        console.warn(`[wechat] ${path.basename(candidate)} 内容损坏,尝试下一个来源`)
      }
    }
    return new Map()
  }

  /** write-to-temp → rename. POSIX `rename` within one filesystem is atomic, so
   * a reader sees either the whole old file or the whole new one, never a
   * half-written one. `.bak` keeps one generation as a second chance. */
  save(map: Map<string, string>): void {
    const tmp = `${this.file}.tmp`
    const bak = `${this.file}.bak`
    try {
      fs.mkdirSync(path.dirname(this.file), { recursive: true, mode: 0o700 })
      if (fs.existsSync(this.file)) {
        try {
          fs.copyFileSync(this.file, bak)
        } catch {
          // A failed backup must not block the write: the rename is what
          // protects the data, the backup is only a second chance.
        }
      }
      fs.writeFileSync(tmp, JSON.stringify(Object.fromEntries(map), null, 2), { mode: 0o600 })
      fs.renameSync(tmp, this.file)
    } catch (err) {
      console.error('[wechat] 保存会话映射失败:', err instanceof Error ? err.message : err)
    }
  }
}
