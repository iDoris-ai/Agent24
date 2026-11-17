// Download a registered domain OS and install it with the app's own CLI
// (`agent24 os install`). The CLI's offline signature check and files.json
// are the trust anchor, the same path as the documents module.

import crypto from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import type { OsPackageProgress } from '../shared/ipc-types'
import { joinLiteral } from './data-dir'
import { manifestVersion, type CliResult, type RunCli } from './documents-module'
import { ArchiveRejected, extractTarGz } from './os-package-archive'
import { DownloadRejected, downloadToFile, requestHttps } from './os-package-net'
import {
  checksumForAsset,
  compareVersions,
  isAllowedDownloadRedirect,
  isAllowedDownloadUrl,
  osPackageEntry,
  parseLatestRelease,
  type ReleaseAsset,
} from '../shared/os-package-registry'

export const MAX_ASSET_BYTES = 512 * 1024 * 1024

export type OsPackageCode =
  | 'not_registered'
  | 'unsupported_platform'
  | 'release_invalid'
  | 'already_installed'
  | 'installed_unreadable'
  | 'no_packages_root'
  | 'cancelled'
  | 'download_failed'
  | 'checksum_mismatch'
  | 'checksum_missing'
  | 'extract_rejected'
  | 'cli_failed'
  | 'daemon_running'
  | 'service_shutdown'

export type OsPackageResult =
  | { ok: true; version: string; upgraded: boolean }
  | { ok: false; code: OsPackageCode; message: string }

export interface InstallOsPackageDeps {
  platform: NodeJS.Platform
  arch: string
  userDataDir: string
  packagesRoot: string | null
  runCli: RunCli
  /** Stops a daemon this app can stop, runs the CLI, then starts it again. */
  withDaemonStopped: <T>(work: () => Promise<T>) => Promise<T>
  /** The daemon was left running (external, or a service this app is not enrolled with). */
  leavesDaemonRunning: () => boolean
  confirm: (info: { name: string; version: string; bytes: number }) => Promise<boolean>
  onProgress: (progress: OsPackageProgress) => void
  fetchJson?: (url: string) => Promise<unknown>
  download?: (url: string, dest: string, onProgress: (received: number, total: number | null) => void) => Promise<void>
  downloadText?: (url: string) => Promise<string>
}

export class OsInstallPauseError extends Error {
  constructor(readonly code: 'service_shutdown' | 'daemon_running', message: string) {
    super(message)
    this.name = 'OsInstallPauseError'
  }
}

function fail(code: OsPackageCode, message: string): OsPackageResult {
  return { ok: false, code, message }
}

function sha256File(file: string): string {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')
}

type Installed = { kind: 'absent' } | { kind: 'version'; version: string } | { kind: 'unreadable' }

function installedPackage(packagesRoot: string, id: string): Installed {
  const dir = joinLiteral(packagesRoot, id)
  let st: fs.Stats
  try {
    st = fs.lstatSync(dir)
  } catch {
    return { kind: 'absent' }
  }
  if (!st.isDirectory()) return { kind: 'unreadable' }
  try {
    const version = manifestVersion(fs.readFileSync(joinLiteral(dir, 'domain-os.yml'), 'utf8'))
    return version === null ? { kind: 'unreadable' } : { kind: 'version', version }
  } catch {
    return { kind: 'unreadable' }
  }
}

function packageRoot(dest: string): string {
  const direct = path.join(dest, 'domain-os.yml')
  if (fs.existsSync(direct) && fs.lstatSync(direct).isFile()) return dest
  const dirs = fs.readdirSync(dest, { withFileTypes: true }).filter((entry) => entry.isDirectory() && !entry.isSymbolicLink())
  const hits = dirs.filter((entry) => {
    const manifest = path.join(dest, entry.name, 'domain-os.yml')
    try {
      return fs.lstatSync(manifest).isFile()
    } catch {
      return false
    }
  })
  if (hits.length !== 1) throw new ArchiveRejected('archive has no single package directory')
  return path.join(dest, hits[0]!.name)
}

async function defaultFetchJson(url: string): Promise<unknown> {
  const body = await requestHttps(url, () => false)
  return JSON.parse(body.toString('utf8')) as unknown
}

async function defaultDownloadText(url: string, accept: (url: string) => boolean): Promise<string> {
  if (!accept(url)) throw new DownloadRejected('checksum URL is not in the registry')
  const body = await requestHttps(url, isAllowedDownloadRedirect)
  return body.toString('utf8')
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

export { formatBytes }

/** What the page shows as "latest", without downloading the archive. */
export async function fetchOsPackageRelease(id: string, arch: string): Promise<OsPackageResult> {
  const entry = osPackageEntry(id)
  if (!entry) return fail('not_registered', id)
  try {
    const release = await lookupOsPackage(id, arch, defaultFetchJson)
    if (!release) return fail('release_invalid', 'the latest release is not a vX.Y.Z macOS asset')
    return { ok: true, version: release.version, upgraded: false }
  } catch (error) {
    return fail('release_invalid', error instanceof Error ? error.message : String(error))
  }
}

/** Latest release for a registered id, or null when the payload is not one. */
export async function lookupOsPackage(id: string, arch: string, fetchJson: (url: string) => Promise<unknown>): Promise<ReleaseAsset | null> {
  const entry = osPackageEntry(id)
  if (!entry) return null
  const body = await fetchJson(entry.releaseApi)
  return parseLatestRelease(body, arch)
}

/** Download, check SHA256SUMS, unpack, then `agent24 os install` (uninstall first when older). */
export async function performOsPackageInstall(id: string, deps: InstallOsPackageDeps): Promise<OsPackageResult> {
  const entry = osPackageEntry(id)
  if (!entry) return fail('not_registered', id)
  if (deps.platform !== 'darwin') return fail('unsupported_platform', deps.platform)
  if (deps.packagesRoot === null) return fail('no_packages_root', 'the data directory could not be resolved')
  const installed = installedPackage(deps.packagesRoot, id)
  if (installed.kind === 'unreadable') {
    return fail('installed_unreadable', `${id} is installed but its manifest cannot be read`)
  }
  let release: ReleaseAsset | null
  try {
    release = await lookupOsPackage(id, deps.arch, deps.fetchJson ?? defaultFetchJson)
  } catch (error) {
    return fail('release_invalid', error instanceof Error ? error.message : String(error))
  }
  if (!release) return fail('release_invalid', 'the latest release is not a vX.Y.Z macOS asset')
  if (installed.kind === 'version') {
    const cmp = compareVersions(installed.version, release.version)
    if (Number.isNaN(cmp)) return fail('installed_unreadable', installed.version)
    if (cmp >= 0) return fail('already_installed', installed.version)
  }
  const confirmed = await deps.confirm({ name: entry.name, version: release.version, bytes: release.bytes })
  if (!confirmed) return fail('cancelled', 'cancelled')

  const parent = path.join(deps.userDataDir, 'os-package-downloads')
  await fs.promises.mkdir(parent, { recursive: true, mode: 0o700 })
  const work = await fs.promises.mkdtemp(path.join(parent, `${id}-`))
  try {
    await fs.promises.chmod(work, 0o700)
    const archive = path.join(work, release.assetName)
    const report = (phase: OsPackageProgress['phase'], received: number, total: number | null): void => {
      deps.onProgress({ id, phase, received, total })
    }
    try {
      const download = deps.download ?? ((url, dest, onProgress) => downloadToFile(
        url,
        dest,
        (candidate) => candidate === release!.assetUrl,
        onProgress,
        MAX_ASSET_BYTES,
      ))
      await download(release.assetUrl, archive, (received, total) => report('download', received, total))
    } catch (error) {
      return fail('download_failed', error instanceof Error ? error.message : String(error))
    }
    report('verify', release.bytes, release.bytes)
    let sums: string
    try {
      const downloadText = deps.downloadText ?? ((url: string) => defaultDownloadText(url, (candidate) => isAllowedDownloadUrl(candidate, release!.tag, release!.checksumName)))
      sums = await downloadText(release.checksumUrl)
    } catch (error) {
      return fail('download_failed', error instanceof Error ? error.message : String(error))
    }
    const expected = checksumForAsset(sums, release.assetName)
    if (expected === null) return fail('checksum_missing', release.assetName)
    const actual = sha256File(archive)
    if (actual !== expected) return fail('checksum_mismatch', release.assetName)
    const unpacked = path.join(work, 'unpack')
    fs.mkdirSync(unpacked, { mode: 0o700 })
    report('extract', 0, null)
    try {
      extractTarGz(archive, unpacked)
    } catch (error) {
      return fail('extract_rejected', error instanceof Error ? error.message : String(error))
    }
    let root: string
    try {
      root = packageRoot(unpacked)
    } catch (error) {
      return fail('extract_rejected', error instanceof Error ? error.message : String(error))
    }
    report('install', 0, null)
    const upgrading = installed.kind === 'version'
    let cli: CliResult
    try {
      cli = await deps.withDaemonStopped(async () => {
        if (upgrading) {
          const removed = await deps.runCli(['os', 'uninstall', id])
          if (removed.code !== 0) return removed
        }
        return deps.runCli(['os', 'install', root])
      })
    } catch (error) {
      if (error instanceof OsInstallPauseError) return fail(error.code, error.message)
      return fail('daemon_running', error instanceof Error ? error.message : String(error))
    }
    if (cli.code !== 0) {
      const message = cli.output.trim().split('\n').pop() ?? ''
      if (deps.leavesDaemonRunning() && /singleton\.lock|is held/i.test(cli.output)) {
        return fail('daemon_running', message)
      }
      return fail('cli_failed', message)
    }
    return { ok: true, version: release.version, upgraded: upgrading }
  } finally {
    await fs.promises.rm(work, { recursive: true, force: true })
  }
}
