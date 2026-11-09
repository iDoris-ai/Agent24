// T1-1 (#1159 Q1 = A; ADR-DOC-01 D6): the mac app carries the documents
// module package in Resources/documents-module/ (scripts/stage-documents-module.sh)
// and installs it at launch, before the daemon starts, so the daemon mounts it
// on that same start. Installing is `agent24 os install <dir>` run from the
// app's own CLI: the one implementation the CLI and the desktop share
// (AGENTS.md), with its consent-revocation gate (ADR-K1-03 §3). Installed is
// enabled: a module not named in os.json is on (os_config.rs).
//
// - Not installed               → install.
// - Installed, the same files    → leave it.
// - Installed, any file differs  → uninstall, then install (the module's data
//   directory is not touched by either; os.json is not touched, so a module
//   the user turned off stays off).
//
// "The same" compares payload bytes and manifest business fields. Installation
// adds files.json and top-level integrity, and generate reformats the YAML.
// - The user uninstalled it  → leave it uninstalled (the marker file below;
//   T1-2 writes it when the user uninstalls and clears it when they install).
//
// Nothing here may stop the app from starting: every failure is one warning
// in the log and an outcome the Documents page can read; the next launch tries
// again.
//
// Before installing, the two programs' code signatures are checked, the rule
// ADR-038 §4 sets for macOS executables (`codesign --verify --strict -R`, team
// ID, Hardened Runtime): a hash pinned at staging cannot be used, because
// electron-builder re-signs every Mach-O in the app, which changes its bytes,
// and nothing can be written into the app once it is signed. A signed app's
// programs must carry the app's own team ID and the runtime flag; only an
// app that is itself ad-hoc signed (an internal test build) settles for a
// valid signature. An app whose identity cannot be read installs nothing.
// After verification, generate integrity metadata on a private temporary copy
// of the signed bytes, never inside the sealed app. The signature is the trust
// anchor (D1); the generated manifest detects subsequent installed-file changes.

import { execFile } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { isDeepStrictEqual } from 'node:util'
import { isAlias, isMap, isScalar, parseDocument, visit } from 'yaml'
import { joinLiteral } from './data-dir'

export const MODULE = 'documents'
/** Under userData; `{"uninstalledByUser": true}` while the user has it uninstalled. */
export const MARKER_FILE = 'documents-module.json'
const MANIFEST = 'domain-os.yml'
/** The package's programs, checked before it is installed. */
const PROGRAMS = ['bin/agent24-documents', 'bin/agent24-documents-pdfkit'] as const
export const CODESIGN = '/usr/bin/codesign'
/** Each CLI call; install copies about 10 MB. Past it the call is stopped. */
export const CLI_TIMEOUT_MS = 60_000

export type BundledModuleOutcome =
  /** The app carries no module (Linux, Windows, a dev run, a build that did not stage it). */
  | 'no_bundle'
  /** The carried package has no readable version. */
  | 'bundle_invalid'
  /** A program's code signature does not pass (or the app's identity is unreadable). */
  | 'signature_invalid'
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
  /** The app's own bundle (…/Agent24.app), whose signature sets the rule. */
  appBundle: string | null
  /** Runs /usr/bin/codesign with these arguments. */
  codesign: RunCli
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

/** Every entry under `dir`, relative and sorted, `/` after a directory;
 * null if one is neither a plain file nor a directory (a symlink: `os install`
 * never makes one), or the tree cannot be read. */
function listTree(dir: string, rel = ''): string[] | null {
  let entries: fs.Dirent[]
  try {
    entries = fs.readdirSync(rel === '' ? dir : joinLiteral(dir, rel), { withFileTypes: true })
  } catch {
    return null
  }
  const out: string[] = []
  for (const e of entries.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))) {
    const name = rel === '' ? e.name : `${rel}/${e.name}`
    if (e.isDirectory()) {
      const sub = listTree(dir, name)
      if (sub === null) return null
      out.push(`${name}/`, ...sub)
    } else if (e.isFile()) {
      out.push(name)
    } else {
      return null
    }
  }
  return out
}

/** Parse business fields; reject ambiguous YAML instead of treating it as equal. */
function businessManifest(bytes: Buffer, installed: boolean): unknown {
  const doc = parseDocument(bytes.toString('utf8'), { uniqueKeys: true })
  visit(doc, {
    Node(_key, node) {
      if (isAlias(node) || node.tag !== undefined || node.anchor !== undefined) {
        throw new Error('unsupported module manifest YAML')
      }
    },
    Pair(_key, pair) {
      if (isScalar(pair.key) && pair.key.value === '<<') {
        throw new Error('unsupported module manifest merge key')
      }
    },
  })
  if (doc.errors.length || doc.warnings.length || !isMap(doc.contents)
    || doc.contents.items.some(({ key }) => !isScalar(key) || typeof key.value !== 'string')) {
    throw new Error('invalid module manifest')
  }
  if (installed) {
    if (!isMap(doc.get('integrity', true))) throw new Error('missing module integrity mapping')
    doc.delete('integrity')
  }
  return doc.toJS({ mapAsMap: true })
}

/** Bundle versus installed copy: ignore installed integrity metadata, compare payload bytes. */
export function sameFiles(a: string, b: string): boolean {
  const left = listTree(a)
  const installedTree = listTree(b)
  if (!installedTree?.includes('files.json')) return false
  const right = installedTree.filter((name) => name !== 'files.json')
  if (left === null || right === null || left.length !== right.length) return false
  if (left.some((name, i) => name !== right[i])) return false
  try {
    return left.every((name) => {
      if (name.endsWith('/')) return true
      const bundled = fs.readFileSync(joinLiteral(a, name))
      const installed = fs.readFileSync(joinLiteral(b, name))
      return name === MANIFEST
        ? isDeepStrictEqual(businessManifest(bundled, false), businessManifest(installed, true))
        : bundled.equals(installed)
    })
  } catch {
    return false
  }
}

function readText(file: string): string | null {
  try {
    return fs.readFileSync(file, 'utf8')
  } catch {
    return null
  }
}

export type AppIdentity = { kind: 'team'; team: string } | { kind: 'adhoc' }

/** The identity in `codesign -dv` output: a team ID, or ad-hoc; else null. */
export function parseIdentity(output: string): AppIdentity | null {
  const team = /^TeamIdentifier=([A-Z0-9]{10})$/m.exec(output)?.[1]
  if (team) return { kind: 'team', team }
  if (/^Signature=adhoc$/m.test(output) && /^TeamIdentifier=not set$/m.test(output)) return { kind: 'adhoc' }
  return null
}

function teamRequirement(team: string): string {
  return `=anchor apple generic and certificate leaf[subject.OU] = "${team}"`
}

/** Why the package may not be installed, or null when it may. The programs
 * are also checked one by one: once installed, they run from the packages
 * root, outside the app's seal. */
export async function signatureProblem(
  bundleDir: string,
  appBundle: string | null,
  codesign: RunCli,
): Promise<string | null> {
  if (appBundle === null) return 'the app bundle is unknown'
  const app = await codesign(['-dv', appBundle])
  const identity = app.code === 0 ? parseIdentity(app.output) : null
  if (identity === null) return `the app's own signature could not be read: ${lastLine(app.output)}`
  // The app's own seal covers everything in Resources/documents-module — the
  // manifest (which names what the daemon spawns) and any file added beside
  // the two programs — not only the programs checked one by one below.
  const sealed = await codesign(
    identity.kind === 'team'
      ? ['--verify', '--strict', '-R', teamRequirement(identity.team), appBundle]
      : ['--verify', '--strict', appBundle],
  )
  if (sealed.code !== 0) return `the app's signature does not verify: ${lastLine(sealed.output)}`
  for (const rel of PROGRAMS) {
    const file = path.join(bundleDir, rel)
    if (identity.kind === 'adhoc') {
      const r = await codesign(['--verify', '--strict', file])
      if (r.code !== 0) return `${rel}: ${lastLine(r.output)}`
      continue
    }
    const r = await codesign(['--verify', '--strict', '-R', teamRequirement(identity.team), file])
    if (r.code !== 0) return `${rel} is not signed by team ${identity.team}: ${lastLine(r.output)}`
    const d = await codesign(['-dv', file])
    if (d.code !== 0 || !/^CodeDirectory .*flags=0x[0-9a-f]+\([^)]*\bruntime\b[^)]*\)/m.test(d.output)) {
      return `${rel} is not signed with the Hardened Runtime`
    }
  }
  return null
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

/** Called only after signatures pass. Keep the old installation until generate succeeds. */
async function installWithIntegrity(bundleDir: string, userDataDir: string, runCli: RunCli, beforeInstall?: () => Promise<void>): Promise<CliResult> {
  const tmp = path.join(userDataDir, 'documents-module-staging')
  fs.mkdirSync(userDataDir, { recursive: true })
  fs.rmSync(tmp, { recursive: true, force: true })
  fs.mkdirSync(tmp, { mode: 0o700 })
  try {
    // Copy entries, not the root: preserve the staging directory's private 0700 mode.
    for (const name of fs.readdirSync(bundleDir)) {
      fs.cpSync(path.join(bundleDir, name), path.join(tmp, name), { recursive: true })
    }
    const generated = await runCli([
      'package-integrity', 'generate', '--root', tmp,
      ...PROGRAMS.flatMap((program) => ['--executable', program]),
    ])
    if (generated.code !== 0) {
      throw new Error(`package-integrity generate failed (rc=${generated.code}): ${lastLine(generated.output)}`)
    }
    await beforeInstall?.()
    return await runCli(['os', 'install', tmp])
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true })
  }
}

export async function ensureBundledDocumentsModule(deps: BundledModuleDeps): Promise<BundledModuleOutcome> {
  try {
    if (deps.bundleDir === null) return 'no_bundle'
    const bundled = readText(path.join(deps.bundleDir, MANIFEST))
    if (bundled === null) return 'no_bundle'
    const want = manifestVersion(bundled)
    if (want === null) {
      deps.warn(`[documents-module] the bundled ${MANIFEST} in ${deps.bundleDir} has no version; not installing it`)
      return 'bundle_invalid'
    }
    if (userUninstalled(deps.userDataDir)) return 'skipped_user_uninstalled'
    if (deps.packagesRoot === null) {
      deps.warn('[documents-module] the data directory could not be resolved; not installing the documents module')
      return 'no_packages_root'
    }
    // Joined literally, as the CLI's PathBuf::join does (no `..` folding).
    const installedDir = joinLiteral(deps.packagesRoot, MODULE)
    const upgrading = fs.existsSync(installedDir)
    let have: string | null = null
    if (upgrading) {
      const text = readText(joinLiteral(installedDir, MANIFEST))
      have = text === null ? null : manifestVersion(text)
      if (have === null) {
        deps.warn(`[documents-module] ${installedDir} has no readable ${MANIFEST}; leaving it as it is`)
        return 'installed_unreadable'
      }
      if (sameFiles(deps.bundleDir, installedDir)) return 'up_to_date'
    }
    const problem = await signatureProblem(deps.bundleDir, deps.appBundle, deps.codesign)
    if (problem !== null) {
      deps.warn(`[documents-module] not installing the bundled documents ${want}: ${problem}`)
      return 'signature_invalid'
    }
    const added = await installWithIntegrity(deps.bundleDir, deps.userDataDir, deps.runCli, async () => {
      if (!upgrading) return
      const removed = await deps.runCli(['os', 'uninstall', MODULE])
      if (removed.code !== 0) {
        throw new Error(`replacing ${have} with ${want}: os uninstall failed (rc=${removed.code}): ${lastLine(removed.output)}`)
      }
    })
    if (added.code !== 0) {
      deps.warn(`[documents-module] os install failed (rc=${added.code}): ${lastLine(added.output)}`)
      return 'install_failed'
    }
    const where = /^installed .*$/m.exec(added.output)?.[0] ?? lastLine(added.output)
    deps.info(`[documents-module] ${upgrading ? 'upgraded to' : 'installed'} documents ${want}: ${where}`)
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
  return dataDir === null ? null : joinLiteral(dataDir, 'packages')
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

// ── T1-2: the user uninstalls and installs it from the Modules page ────────
// Both run the app's CLI, as the launch check does, and take effect when the
// daemon next starts (the CLI's own rule). Neither touches the module's data
// (`<data dir>/os/documents`) or os.json.

export type ModuleActionError =
  /** The app carries no module to install. */
  | 'no_bundle'
  | 'no_packages_root'
  | 'already_installed'
  | 'signature_invalid'
  /** The CLI exited non-zero (or could not run); its last line is in the message. */
  | 'cli_failed'
  /** The "user uninstalled" marker could not be written. */
  | 'marker_failed'
  /** The app's daemon did not exit in time, so the CLI was not run. */
  | 'daemon_running'

export type ModuleActionResult = { ok: true } | { ok: false; code: ModuleActionError; message: string }

/** True if anything (file, dir, even a dangling symlink) is at `p`: what the CLI counts as installed. */
function entryExists(p: string): boolean {
  try {
    fs.lstatSync(p)
    return true
  } catch {
    return false
  }
}

/** What is on disk now, for the Modules and Documents pages. */
export function documentsModuleOnDisk(
  bundleDir: string | null,
  packagesRoot: string | null,
): { bundled: boolean; installed: boolean } {
  const text = bundleDir === null ? null : readText(path.join(bundleDir, MANIFEST))
  return {
    bundled: text !== null && manifestVersion(text) !== null,
    installed: packagesRoot !== null && entryExists(joinLiteral(packagesRoot, MODULE)),
  }
}

/** Writes `{"uninstalledByUser": true, "at": …}`, whole, then renamed into place. Throws on failure. */
export function writeUninstalledMarker(userDataDir: string, now: Date): void {
  fs.mkdirSync(userDataDir, { recursive: true })
  const file = path.join(userDataDir, MARKER_FILE)
  const tmp = `${file}.${process.pid}.tmp`
  fs.writeFileSync(tmp, `${JSON.stringify({ uninstalledByUser: true, at: now.toISOString() })}\n`)
  fs.renameSync(tmp, file)
}

/** Removes the marker; a missing one is fine. Throws on any other failure. */
export function clearUninstalledMarker(userDataDir: string): void {
  fs.rmSync(path.join(userDataDir, MARKER_FILE), { force: true })
}

export interface UninstallDeps {
  userDataDir: string
  runCli: RunCli
  warn: (message: string) => void
  info: (message: string) => void
  now: () => Date
}

/** `agent24 os uninstall documents`, marked first so that no later launch puts it back. */
export async function uninstallDocumentsModule(deps: UninstallDeps): Promise<ModuleActionResult> {
  const marker = path.join(deps.userDataDir, MARKER_FILE)
  const hadMarker = entryExists(marker)
  // Marked before removing: a removal that went through with no marker
  // would be undone by the next launch.
  try {
    writeUninstalledMarker(deps.userDataDir, deps.now())
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error)
    deps.warn(`[documents-module] not uninstalling: could not write ${marker}: ${message}`)
    return { ok: false, code: 'marker_failed', message }
  }
  const r = await deps.runCli(['os', 'uninstall', MODULE])
  if (r.code !== 0) {
    // Still installed: take the marker back, or launches would stop upgrading it.
    if (!hadMarker) {
      try {
        clearUninstalledMarker(deps.userDataDir)
      } catch (error) {
        deps.warn(`[documents-module] could not remove ${marker}: ${error instanceof Error ? error.message : String(error)}`)
      }
    }
    const message = `agent24 os uninstall ${MODULE} (rc=${r.code}): ${lastLine(r.output)}`
    deps.warn(`[documents-module] ${message}`)
    return { ok: false, code: 'cli_failed', message }
  }
  deps.info(`[documents-module] uninstalled by the user: ${lastLine(r.output)}`)
  return { ok: true }
}

/** Installs the module the app carries, checked as at launch, then clears the marker. */
export async function installBundledDocumentsModule(deps: BundledModuleDeps): Promise<ModuleActionResult> {
  const disk = documentsModuleOnDisk(deps.bundleDir, deps.packagesRoot)
  if (deps.bundleDir === null || !disk.bundled) {
    return { ok: false, code: 'no_bundle', message: 'the app carries no documents module' }
  }
  if (deps.packagesRoot === null) {
    return { ok: false, code: 'no_packages_root', message: 'the data directory could not be resolved' }
  }
  if (disk.installed) {
    return { ok: false, code: 'already_installed', message: `${joinLiteral(deps.packagesRoot, MODULE)} is already there` }
  }
  const problem = await signatureProblem(deps.bundleDir, deps.appBundle, deps.codesign)
  if (problem !== null) {
    deps.warn(`[documents-module] not installing the bundled documents: ${problem}`)
    return { ok: false, code: 'signature_invalid', message: problem }
  }
  let r: CliResult
  try {
    r = await installWithIntegrity(deps.bundleDir, deps.userDataDir, deps.runCli)
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error)
    deps.warn(`[documents-module] ${message}`)
    return { ok: false, code: 'cli_failed', message }
  }
  if (r.code !== 0) {
    const message = `agent24 os install (rc=${r.code}): ${lastLine(r.output)}`
    deps.warn(`[documents-module] ${message}`)
    return { ok: false, code: 'cli_failed', message }
  }
  // Installed either way. A marker left behind only stops later launches
  // from upgrading it, so this is a warning, not a failure.
  try {
    clearUninstalledMarker(deps.userDataDir)
  } catch (error) {
    deps.warn(`[documents-module] installed, but could not remove the uninstalled marker: ${error instanceof Error ? error.message : String(error)}`)
  }
  deps.info(`[documents-module] installed by the user: ${/^installed .*$/m.exec(r.output)?.[0] ?? lastLine(r.output)}`)
  return { ok: true }
}
