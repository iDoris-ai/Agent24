// The same local user's own oMLX config (`~/.omlx/settings.json`) can carry
// the API key the running oMLX server demands. Auto-detect may use it so a
// keyed server is found without the user retyping the key.
//
// Handling rules (the key is a credential):
//   - read only in the main process, held only in memory for the probe;
//   - never logged, never returned over IPC, never written anywhere;
//   - only trusted when the file is the current user's own regular file
//     (not a symlink, not writable by group/other), and only ever sent to the
//     host:port that same file says oMLX listens on (loopback only).

import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { parseLoopbackUrl, parsePort } from './omlx-validate'

const MAX_SETTINGS_BYTES = 1024 * 1024
const MAX_KEY_LENGTH = 512

export interface OmlxSettingsKey {
  /** `http://<loopback host>:<port>` of the server this key belongs to. */
  url: string
  apiKey: string
}

export function defaultOmlxSettingsPath(): string {
  return path.join(os.homedir(), '.omlx', 'settings.json')
}

/** Literal loopback addresses only: a name such as `localhost` is resolved by
 *  the OS and so could point elsewhere; the key is never sent to a name. */
const LITERAL_LOOPBACK = new Set(['127.0.0.1', '::1', '[::1]'])

function isOwnedAndNotWritableByOthers(stat: fs.Stats): boolean {
  if (process.platform === 'win32') return true
  if (typeof process.getuid === 'function' && stat.uid !== process.getuid()) return false
  return (stat.mode & 0o022) === 0
}

/** The configured key and the origin it is valid for, or null if the file is
 *  missing, unreadable, not trustworthy, or holds no usable key. Never throws.
 *  Cheap enough to call per request, so nothing needs to be cached (a rotated
 *  or removed key takes effect at once). */
export function readOmlxSettingsKey(file = defaultOmlxSettingsPath()): OmlxSettingsKey | null {
  let fd: number | null = null
  try {
    // The directory holding it must not be swappable by another user either.
    if (!isOwnedAndNotWritableByOthers(fs.statSync(path.dirname(file)))) return null
    // Open without following a symlink, then check and read THAT descriptor, so
    // nothing can be swapped between the check and the read.
    const noFollow = process.platform === 'win32' ? 0 : fs.constants.O_NOFOLLOW
    fd = fs.openSync(file, fs.constants.O_RDONLY | noFollow)
    const stat = fs.fstatSync(fd)
    if (!stat.isFile() || stat.size > MAX_SETTINGS_BYTES || !isOwnedAndNotWritableByOthers(stat)) return null
    const buf = Buffer.alloc(stat.size)
    let read = 0
    while (read < buf.length) {
      const n = fs.readSync(fd, buf, read, buf.length - read, read)
      if (n === 0) break
      read += n
    }
    const parsed: unknown = JSON.parse(buf.subarray(0, read).toString('utf8'))
    if (typeof parsed !== 'object' || parsed === null) return null
    const root = parsed as { auth?: { api_key?: unknown }; server?: { host?: unknown; port?: unknown } }
    const key = root.auth?.api_key
    if (typeof key !== 'string') return null
    const apiKey = key.trim()
    // eslint-disable-next-line no-control-regex
    if (apiKey === '' || apiKey.length > MAX_KEY_LENGTH || /[\u0000-\u001f\u007f]/.test(apiKey)) return null
    const host = root.server?.host === undefined ? '127.0.0.1' : root.server.host
    const port = root.server?.port === undefined ? 8000 : parsePort(root.server.port)
    if (typeof host !== 'string' || !LITERAL_LOOPBACK.has(host) || port === null) return null
    const base = parseLoopbackUrl(`http://${host.includes(':') && !host.startsWith('[') ? `[${host}]` : host}:${port}`)
    return base ? { url: base.origin, apiKey } : null
  } catch {
    return null
  } finally {
    if (fd !== null) { try { fs.closeSync(fd) } catch { /* ignore */ } }
  }
}
