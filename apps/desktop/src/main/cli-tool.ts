import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { constants } from 'node:fs'
import { access, lstat, realpath, readlink, stat } from 'node:fs/promises'
import path from 'node:path'
import { randomUUID } from 'node:crypto'
import type { CliToolStatus, CliInstallResult } from '../shared/ipc-types'

const run = promisify(execFile)
export const CLI_DESTINATION = '/usr/local/bin/agent24'
const VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([\da-zA-Z-]+(?:\.[\da-zA-Z-]+)*))?(?:\+[\da-zA-Z-]+(?:\.[\da-zA-Z-]+)*)?$/

function parseVersion(value: string) {
  const match = VERSION.exec(value)
  if (match?.[4]?.split('.').some((id) => /^\d+$/.test(id) && id.length > 1 && id.startsWith('0'))) return null
  return match
}

export function compareVersions(a: string, b: string): -1 | 0 | 1 | null {
  const x = parseVersion(a), y = parseVersion(b)
  if (!x || !y) return null
  for (const i of [1, 2, 3]) {
    if (BigInt(x[i]) !== BigInt(y[i])) return BigInt(x[i]) < BigInt(y[i]) ? -1 : 1
  }
  if (x[4] === y[4]) return 0
  if (!x[4]) return 1
  if (!y[4]) return -1
  const left = x[4].split('.'), right = y[4].split('.')
  for (let i = 0; i < Math.max(left.length, right.length); i++) {
    const l = left[i], r = right[i]
    if (l === undefined) return -1
    if (r === undefined) return 1
    if (l === r) continue
    const ln = /^\d+$/.test(l), rn = /^\d+$/.test(r)
    if (ln && rn) return BigInt(l) < BigInt(r) ? -1 : 1
    if (ln !== rn) return ln ? -1 : 1
    return l < r ? -1 : 1
  }
  return 0
}

export async function resolveBundledCli(executable: string, resources: string): Promise<string> {
  const exe = await realpath(executable)
  const bundle = path.resolve(exe, '../../..')
  if (!bundle.endsWith('.app') || exe !== path.join(bundle, 'Contents/MacOS', path.basename(exe))) throw new Error('Invalid App bundle')
  const expected = path.join(bundle, 'Contents/Resources/backend/agent24')
  if (path.resolve(resources) !== path.dirname(path.dirname(expected)) || await realpath(expected) !== expected) throw new Error('Invalid bundled CLI target')
  if (!(await stat(expected)).isFile()) throw new Error('CLI is not a file')
  await access(expected, constants.X_OK)
  return expected
}

async function versionOf(executable: string): Promise<string | null> {
  try {
    const { stdout } = await run(executable, ['--version'], { timeout: 3_000, maxBuffer: 4096 })
    const version = /^agent24\s+(\S+)\s*$/.exec(stdout.trim())?.[1]
    return version && parseVersion(version) ? version : null
  } catch { return null }
}

export async function inspectCommand(searchPath: string): Promise<{ path: string | null; version: string | null }> {
  for (const dir of searchPath.split(path.delimiter)) {
    if (!path.isAbsolute(dir)) continue
    const candidate = path.join(dir, 'agent24')
    try {
      await access(candidate, constants.X_OK)
      if (!(await stat(candidate)).isFile()) continue
    } catch { continue }
    return { path: candidate, version: await versionOf(candidate) }
  }
  return { path: null, version: null }
}

async function loginPath(): Promise<string> {
  const shell = ['/bin/zsh', '/bin/bash', '/bin/sh'].includes(process.env.SHELL ?? '') ? process.env.SHELL ?? '/bin/zsh' : '/bin/zsh'
  try {
    const { stdout } = await run(shell, ['-ilc', 'printf "\\nAGENT24_PATH=%s" "$PATH"'], { timeout: 3_000, maxBuffer: 65536 })
    const marker = '\nAGENT24_PATH='
    if (!stdout.includes(marker)) throw new Error('Login PATH unavailable')
    return stdout.slice(stdout.lastIndexOf(marker) + marker.length).trim()
  } catch { return process.env.PATH ?? '/usr/local/bin:/usr/bin:/bin' }
}

export async function cliToolStatus(executable: string, resources: string, packaged: boolean): Promise<CliToolStatus> {
  if (process.platform !== 'darwin' || !packaged) return { state: 'unavailable', path: null, version: null, bundledVersion: null }
  const command = await inspectCommand(await loginPath())
  try {
    const target = await resolveBundledCli(executable, resources)
    const bundledVersion = await versionOf(target)
    if (!bundledVersion) return { ...command, bundledVersion: null, state: 'unavailable' }
    const comparison = command.version ? compareVersions(command.version, bundledVersion) : null
    const state = !command.path ? 'missing' : comparison === null ? 'unknown' : comparison < 0 ? 'outdated' : comparison > 0 ? 'newer' : 'current'
    return { ...command, bundledVersion, state }
  } catch { return { ...command, bundledVersion: null, state: 'unavailable' } }
}

export function shellQuote(value: string): string {
  if ([...value].some((char) => char.charCodeAt(0) < 32 || char.charCodeAt(0) === 127)) throw new Error('Invalid path characters')
  return `'${value.replace(/'/g, `'"'"'`)}'`
}

export function appleScript(command: string): string {
  return `do shell script "${command.replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\n')}" with administrator privileges`
}

async function identity(file: string): Promise<string | null> {
  try {
    const s = await lstat(file, { bigint: true })
    if (!s.isFile() && !s.isSymbolicLink()) throw new Error('Destination is not a file or symlink')
    return `${s.dev}:${s.ino}:${s.mtimeNs}:${s.ctimeNs}`
  } catch (error) {
    if (error instanceof Error && 'code' in error && error.code === 'ENOENT') return null
    throw error
  }
}

async function assertParents(directory: string): Promise<void> {
  for (let dir = directory; dir !== path.dirname(dir); dir = path.dirname(dir)) {
    try {
      if ((await lstat(dir)).isSymbolicLink()) throw new Error('Symlinked parent directory')
    } catch (error) {
      if (!(error instanceof Error && 'code' in error && error.code === 'ENOENT')) throw error
    }
  }
}

/** Main-process only. Renderer supplies neither paths nor shell text. The same
 * script runs unprivileged first and through osascript only on permission denial. */
export async function installLink(options: {
  target: string
  destination: string
  confirm: () => Promise<boolean>
  elevate?: (script: string) => Promise<void>
}): Promise<CliInstallResult> {
  const { target, destination } = options
  if (await realpath(target) !== target) throw new Error('Invalid target')
  await assertParents(path.dirname(destination))
  const before = await identity(destination)
  if (before && (await lstat(destination)).isSymbolicLink() && await readlink(destination) === target) return { state: 'installed', backup: null }
  if (before && !await options.confirm()) return { state: 'cancelled' }
  if (await identity(destination) !== before || await realpath(target) !== target) throw new Error('Installation paths changed')
  const backup = before ? `${destination}.bak-${new Date().toISOString().replace(/[:.]/g, '-')}-${randomUUID()}` : null
  const q = shellQuote
  const parents: string[] = []
  for (const start of [path.dirname(destination), target]) {
    for (let dir = start; dir !== path.dirname(dir); dir = path.dirname(dir)) parents.push(`[ ! -L ${q(dir)} ]`)
  }
  // inode/device guard also runs AFTER the administrator prompt. Never overwrite
  // a destination that appeared or changed while the user was deciding.
  const expected = before ? before.split(':').slice(0, 2).join(':') : null
  const statFormat = process.platform === 'darwin' ? "-f '%d:%i'" : "-c '%d:%i'"
  const check = expected
    ? `[ "$(/usr/bin/stat ${statFormat} ${q(destination)})" = ${q(expected)} ]`
    : `[ ! -e ${q(destination)} ] && [ ! -L ${q(destination)} ]`
  const move = backup ? `[ ! -e ${q(backup)} ] && [ ! -L ${q(backup)} ] && /bin/mv -n ${q(destination)} ${q(backup)} && ` : ''
  const rollback = backup ? `/bin/mv -n ${q(backup)} ${q(destination)}; ` : ''
  const script = `${parents.join(' && ')} && [ -f ${q(target)} ] && [ -x ${q(target)} ] && ${check} && /bin/mkdir -p ${q(path.dirname(destination))} && ${move}{ /bin/ln -s ${q(target)} ${q(destination)} || { ${rollback}exit 1; }; }`
  try {
    await run('/bin/sh', ['-c', script], { timeout: 10_000, maxBuffer: 4096, env: { ...process.env, LC_ALL: 'C' } })
  } catch (error) {
    const message = error instanceof Error ? error.message : ''
    if (!options.elevate || !/Permission denied|Operation not permitted/.test(message)) throw error
    // Only the production's fixed destination can ever be used with elevation.
    if (destination !== CLI_DESTINATION) throw new Error('Invalid privileged destination')
    try { await options.elevate(script) } catch (failure) {
      if (failure instanceof Error && /\(-128\)|User canceled|User cancelled/.test(failure.message)) return { state: 'cancelled' }
      throw failure
    }
  }
  if (await readlink(destination) !== target) throw new Error('Installed link verification failed')
  return { state: 'installed', backup }
}

export async function installBundledCli(options: {
  executable: string; resources: string; packaged: boolean; confirm: () => Promise<boolean>
}): Promise<CliInstallResult> {
  if (process.platform !== 'darwin' || !options.packaged) return { state: 'failed' }
  try {
    const target = await resolveBundledCli(options.executable, options.resources)
    // A link into a mounted DMG / App Translocation would break after ejecting.
    if (target.startsWith('/Volumes/') || target.includes('/AppTranslocation/')) return { state: 'move-app' }
    return await installLink({
      target, destination: CLI_DESTINATION, confirm: options.confirm,
      elevate: async (script) => { await run('/usr/bin/osascript', ['-e', appleScript(script)], { timeout: 120_000, maxBuffer: 4096 }) },
    })
  } catch { return { state: 'failed' } }
}
