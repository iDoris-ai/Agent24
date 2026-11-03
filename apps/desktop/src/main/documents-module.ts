// T1-1 (#1159 Q1 = A; ADR-DOC-01 D6): the mac app carries the documents
// module package in Resources/documents-module/ (scripts/stage-documents-module.sh)
// and installs it at launch, before the daemon starts, so the daemon mounts it
// on that same start. Installing is `agent24 os install <dir>` run from the
// app's own CLI: the one implementation the CLI and the desktop share
// (AGENTS.md), with its consent-revocation gate (ADR-K1-03 §3). Installed is
// enabled: a module not named in os.json is on (os_config.rs).
//
// - Not installed            → install.
// - Installed, same version  → leave it.
// - Installed, other version → uninstall, then install (the module's data
//   directory is not touched by either; os.json is not touched, so a module
//   the user turned off stays off).
// - The user uninstalled it  → leave it uninstalled (the marker file below;
//   T1-2 writes it when the user uninstalls and clears it when they install).
//
// Nothing here may stop the app from starting: every failure is one warning
// in the log and an outcome the Documents page can read; the next launch tries
// again. The package is checked against its SHA256SUMS before it is installed.

import { execFile } from 'node:child_process'
import { createHash } from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'

export const MODULE = 'documents'
/** Under userData; `{"uninstalledByUser": true}` while the user has it uninstalled. */
export const MARKER_FILE = 'documents-module.json'
const MANIFEST = 'domain-os.yml'
const SUMS = 'SHA256SUMS'
/** Each CLI call; install copies about 10 MB. Past it the call is stopped. */
export const CLI_TIMEOUT_MS = 60_000

export type BundledModuleOutcome =
  /** The app carries no module (Linux, Windows, a dev run, a build that did not stage it). */
  | 'no_bundle'
  /** The carried package is unreadable or does not match its SHA256SUMS. */
  | 'bundle_invalid'
  | 'skipped_user_uninstalled'
  /** The data directory could not be resolved. */
  | 'no_packages_root'
  | 'up_to_date'
  /** Something is installed under the name but its manifest is unreadable: left alone. */
  | 'installed_unreadable'
  | 'installed'
  | 'upgraded'
  | 'install_failed'

export interface CliResult {
  code: number | null
  output: string
}
/** Runs the app's `agent24` CLI with these arguments. */
export type RunCli = (args: string[]) => Promise<CliResult>

export interface BundledModuleDeps {
  /** Resources/documents-module, or null when the app carries none. */
  bundleDir: string | null
  /** `A24_OS_PACKAGES`, else `<data dir>/packages` (agent24-os-packages); null if unresolvable. */
  packagesRoot: string | null
  userDataDir: string
  runCli: RunCli
  warn: (message: string) => void
  info: (message: string) => void
}

/** The manifest's top-level `version:` (quoted or not), or null. */
export function manifestVersion(text: string): string | null {
  const m = /^version:[ \t]*(?:"([^"\n]*)"|'([^'\n]*)'|([^\s#]+))[ \t]*(?:#.*)?$/m.exec(text)
  const v = m ? (m[1] ?? m[2] ?? m[3] ?? '') : ''
  return v === '' ? null : v
}

function readText(file: string): string | null {
  try {
    return fs.readFileSync(file, 'utf8')
  } catch {
    return null
  }
}

/** Every line of SHA256SUMS matches its file, and the manifest and the
 * program it spawns are among them. */
export function bundleMatchesSums(dir: string): boolean {
  const sums = readText(path.join(dir, SUMS))
  if (sums === null) return false
  const listed = new Set<string>()
  for (const line of sums.split('\n')) {
    if (line.trim() === '') continue
    const m = /^([0-9a-f]{64}) [ *](.+)$/.exec(line)
    if (!m) return false
    const rel = m[2]!
    if (path.isAbsolute(rel) || rel.split('/').includes('..')) return false
    let digest: string
    try {
      digest = createHash('sha256').update(fs.readFileSync(path.join(dir, rel))).digest('hex')
    } catch {
      return false
    }
    if (digest !== m[1]) return false
    listed.add(rel)
  }
  return listed.has(MANIFEST) && listed.has('bin/agent24-documents')
}

export function userUninstalled(userDataDir: string): boolean {
  const text = readText(path.join(userDataDir, MARKER_FILE))
  if (text === null) return false
  try {
    const v = JSON.parse(text) as unknown
    return typeof v === 'object' && v !== null && (v as Record<string, unknown>)['uninstalledByUser'] === true
  } catch {
    return false
  }
}

function lastLine(output: string): string {
  const lines = output.trim().split('\n')
  return lines[lines.length - 1] ?? ''
}

export async function ensureBundledDocumentsModule(deps: BundledModuleDeps): Promise<BundledModuleOutcome> {
  try {
    if (deps.bundleDir === null) return 'no_bundle'
    const bundled = readText(path.join(deps.bundleDir, MANIFEST))
    if (bundled === null) return 'no_bundle'
    const want = manifestVersion(bundled)
    if (want === null || !bundleMatchesSums(deps.bundleDir)) {
      deps.warn(`[documents-module] the bundled package in ${deps.bundleDir} does not match its ${SUMS}; not installing it`)
      return 'bundle_invalid'
    }
    if (userUninstalled(deps.userDataDir)) return 'skipped_user_uninstalled'
    if (deps.packagesRoot === null) {
      deps.warn('[documents-module] the data directory could not be resolved; not installing the documents module')
      return 'no_packages_root'
    }
    const installedDir = path.join(deps.packagesRoot, MODULE)
    const upgrading = fs.existsSync(installedDir)
    if (upgrading) {
      const text = readText(path.join(installedDir, MANIFEST))
      const have = text === null ? null : manifestVersion(text)
      if (have === null) {
        deps.warn(`[documents-module] ${installedDir} has no readable ${MANIFEST}; leaving it as it is`)
        return 'installed_unreadable'
      }
      if (have === want) return 'up_to_date'
      const removed = await deps.runCli(['os', 'uninstall', MODULE])
      if (removed.code !== 0) {
        deps.warn(`[documents-module] upgrading ${have} → ${want}: os uninstall failed (rc=${removed.code}): ${lastLine(removed.output)}`)
        return 'install_failed'
      }
    }
    const added = await deps.runCli(['os', 'install', deps.bundleDir])
    if (added.code !== 0) {
      deps.warn(`[documents-module] os install failed (rc=${added.code}): ${lastLine(added.output)}`)
      return 'install_failed'
    }
    deps.info(`[documents-module] ${upgrading ? 'upgraded to' : 'installed'} documents ${want}: ${lastLine(added.output)}`)
    return upgrading ? 'upgraded' : 'installed'
  } catch (error) {
    deps.warn(`[documents-module] ${error instanceof Error ? error.message : String(error)}`)
    return 'install_failed'
  }
}

/** `A24_OS_PACKAGES` when set (as the CLI and daemon read it), else `<dataDir>/packages`. */
export function packagesRootFor(env: NodeJS.ProcessEnv, dataDir: string | null): string | null {
  const over = env['A24_OS_PACKAGES']
  if (over !== undefined) return over === '' ? null : over
  return dataDir === null ? null : path.join(dataDir, 'packages')
}

/** Runs `bin` with a timeout; never rejects. */
export function execCli(bin: string, timeoutMs: number = CLI_TIMEOUT_MS): RunCli {
  return (args) =>
    new Promise((resolve) => {
      execFile(bin, args, { timeout: timeoutMs, maxBuffer: 1024 * 1024 }, (error, stdout, stderr) => {
        const output = `${stdout ?? ''}${stderr ?? ''}`
        if (!error) return resolve({ code: 0, output })
        const code = typeof (error as { code?: unknown }).code === 'number' ? (error as { code: number }).code : null
        resolve({ code, output: output || error.message })
      })
    })
}

/** The outcome of this launch's check, for the Documents page. */
let lastOutcome: BundledModuleOutcome | null = null
export function recordOutcome(outcome: BundledModuleOutcome): void {
  lastOutcome = outcome
}
export function bundledModuleOutcome(): BundledModuleOutcome | null {
  return lastOutcome
}
