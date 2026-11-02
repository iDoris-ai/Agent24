// Reads the encrypted backup a user picked, ONCE, at pick time (L2): the bytes
// then live in main's memory, so the file can no longer be swapped between
// "picked" and "read". Mirrors the daemon's rules for the restore text: a
// regular file (opened with O_NOFOLLOW, so not a symlink), at most 4 KiB, one
// `ncryptsec1…` token of at most 200 ASCII characters. The daemon re-validates
// everything it receives; this only keeps garbage from becoming a ticket.

import fs from 'node:fs'

export const BACKUP_FILE_MAX_BYTES = 4096
const NCRYPTSEC_MAX_LEN = 200
// `!`..`~` only: no whitespace, no control or non-ASCII character anywhere.
const NCRYPTSEC = /^ncryptsec1[\x21-\x7e]*$/

export type BackupFileResult =
  | { ok: true; ncryptsec: string }
  | { ok: false; reason: 'unreadable' | 'not_regular' | 'bad_size' | 'not_backup' }

export function parseBackupText(text: string): string | null {
  // The file's own single trailing line break is not part of the backup.
  const line = text.endsWith('\r\n') ? text.slice(0, -2) : text.endsWith('\n') ? text.slice(0, -1) : text
  return line.length <= NCRYPTSEC_MAX_LEN && NCRYPTSEC.test(line) ? line : null
}

export function readBackupFile(file: string): BackupFileResult {
  let fd: number
  try {
    // O_NONBLOCK: a FIFO must not hang main. O_NOFOLLOW: no symlink at the end.
    fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK)
  } catch {
    return { ok: false, reason: 'unreadable' }
  }
  try {
    const st = fs.fstatSync(fd)
    if (!st.isFile()) return { ok: false, reason: 'not_regular' }
    if (st.size <= 0 || st.size > BACKUP_FILE_MAX_BYTES) return { ok: false, reason: 'bad_size' }
    // One extra byte so a file that grew after fstat is detected, not truncated.
    const buffer = Buffer.alloc(BACKUP_FILE_MAX_BYTES + 1)
    let total = 0
    while (total < buffer.length) {
      const n = fs.readSync(fd, buffer, total, buffer.length - total, null)
      if (n === 0) break
      total += n
    }
    if (total > BACKUP_FILE_MAX_BYTES) return { ok: false, reason: 'bad_size' }
    const text = buffer.subarray(0, total).toString('utf8')
    const ncryptsec = parseBackupText(text)
    return ncryptsec === null ? { ok: false, reason: 'not_backup' } : { ok: true, ncryptsec }
  } catch {
    return { ok: false, reason: 'unreadable' }
  } finally {
    try { fs.closeSync(fd) } catch { /* nothing to do */ }
  }
}
