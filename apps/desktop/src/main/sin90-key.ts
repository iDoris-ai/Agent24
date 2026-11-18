// Human actor key for Sin90 writes. Read in the main process only.
// The key never goes to the renderer and must not be logged.

import fs from 'node:fs'
import { joinLiteral, tryDataDir } from './data-dir'
import { packagesRootFor } from './documents-module'

const KEY_FILE = 'actor-keys.json'

export type HumanKey = { ok: true; human: string } | { ok: false }

export interface KeyFiles {
  manifestText: string | null
  home: string
  uid: number
  stat: (file: string) => { mode: number; uid: number } | null
  readFile: (file: string) => string | null
}

function manifestDataDir(text: string): string | null {
  for (const line of text.split('\n')) {
    const match = /^data_dir:\s*(.*?)\s*$/.exec(line)
    if (!match) continue
    let value = match[1] ?? ''
    if ((value.startsWith('"') && value.endsWith('"')) || (value.startsWith("'") && value.endsWith("'"))) {
      value = value.slice(1, -1)
    }
    return value || null
  }
  return null
}

/** `~/.agent24/os/sin90` → an absolute path. Rejects anything that is not a home or absolute path. */
export function expandDataDir(value: string, home: string): string | null {
  const trimmed = value.trim().replace(/\/+$/, '')
  if (!trimmed || trimmed.includes('..') || trimmed.includes('\0') || trimmed.includes('\\')) return null
  if (trimmed.startsWith('~/')) {
    const root = home.replace(/\/+$/, '')
    if (!root.startsWith('/')) return null
    return `${root}/${trimmed.slice(2)}`
  }
  if (trimmed.startsWith('/')) return trimmed
  return null
}

function headerSafe(value: string): boolean {
  return value.length >= 32 && [...value].every((ch) => {
    const code = ch.charCodeAt(0)
    return code >= 0x21 && code <= 0x7e
  })
}

export function humanKey(files: KeyFiles): HumanKey {
  if (!files.manifestText || !files.home) return { ok: false }
  const declared = manifestDataDir(files.manifestText)
  if (!declared) return { ok: false }
  const dir = expandDataDir(declared, files.home)
  if (!dir) return { ok: false }
  const file = joinLiteral(dir, KEY_FILE)
  const info = files.stat(file)
  if (!info || (info.mode & 0o777) !== 0o600 || info.uid !== files.uid) return { ok: false }
  const raw = files.readFile(file)
  if (raw === null) return { ok: false }
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    return { ok: false }
  }
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) return { ok: false }
  const human = (parsed as { human?: unknown }).human
  if (typeof human !== 'string' || !headerSafe(human)) return { ok: false }
  return { ok: true, human }
}

function readText(file: string): string | null {
  try {
    return fs.readFileSync(file, 'utf8')
  } catch {
    return null
  }
}

function readStat(file: string): { mode: number; uid: number } | null {
  try {
    const info = fs.statSync(file)
    return { mode: info.mode, uid: info.uid }
  } catch {
    return null
  }
}

/** The installed Sin90 package's `data_dir`, then `<data_dir>/actor-keys.json`. */
export function readInstalledHumanKey(env: NodeJS.ProcessEnv = process.env): HumanKey {
  const packages = packagesRootFor(env, tryDataDir())
  const home = env['HOME']
  const uid = typeof process.getuid === 'function' ? process.getuid() : null
  if (!packages || !home || uid === null) return { ok: false }
  return humanKey({
    manifestText: readText(joinLiteral(packages, 'sin90', 'domain-os.yml')),
    home,
    uid,
    stat: readStat,
    readFile: readText,
  })
}
