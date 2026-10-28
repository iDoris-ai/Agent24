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

/** The configured key and the origin it is valid for, or null if the file is
 *  missing, unreadable, not trustworthy, or holds no usable key. Never throws. */
export function readOmlxSettingsKey(file = defaultOmlxSettingsPath()): OmlxSettingsKey | null {
  try {
    const stat = fs.lstatSync(file) // lstat: a symlink is refused, not followed
    if (!stat.isFile() || stat.size > MAX_SETTINGS_BYTES) return null
    if (process.platform !== 'win32') {
      if (typeof process.getuid === 'function' && stat.uid !== process.getuid()) return null
      if ((stat.mode & 0o022) !== 0) return null
    }
    const parsed: unknown = JSON.parse(fs.readFileSync(file, 'utf8'))
    if (typeof parsed !== 'object' || parsed === null) return null
    const root = parsed as { auth?: { api_key?: unknown }; server?: { host?: unknown; port?: unknown } }
    const key = root.auth?.api_key
    if (typeof key !== 'string') return null
    const apiKey = key.trim()
    // eslint-disable-next-line no-control-regex
    if (apiKey === '' || apiKey.length > MAX_KEY_LENGTH || /[\u0000-\u001f\u007f]/.test(apiKey)) return null
    const host = root.server?.host === undefined ? '127.0.0.1' : root.server.host
    const port = root.server?.port === undefined ? 8000 : parsePort(root.server.port)
    if (typeof host !== 'string' || port === null) return null
    const base = parseLoopbackUrl(`http://${host.includes(':') && !host.startsWith('[') ? `[${host}]` : host}:${port}`)
    return base ? { url: base.origin, apiKey } : null
  } catch {
    return null
  }
}
