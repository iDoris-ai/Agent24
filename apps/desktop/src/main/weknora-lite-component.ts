// KB-LITE-03a: on-demand download + install of the WeKnora Lite (headless)
// program, following open-design-component.ts (pinned hash baked into the
// App, `.partial-*` scratch dirs, idle timeout, loopback dev override,
// marker file, read-only installed tree). This slice only installs; 3b
// launches it, 3c adds the settings entry.
//
// Layout:
//   program (replaceable): <data_dir>/components/weknora-lite/<sha7>-darwin-arm64/
//     WeKnora-lite, LICENSE, THIRD_PARTY_NOTICES.md, dict/, config/, migrations/, ...
//     .agent24-component.json   (marker, written last before the atomic rename)
//   data (NOT ours): <data_dir>/data/weknora-lite/ — owned by 3b. Nothing in
//   this file reads, writes, or deletes anything outside componentsRoot.
//
// Trust root: WEKNORA_LITE_PIN (weknora-lite-pin.ts), compiled into the App.
// Checked: archive size + sha256 before extraction; after extraction the
// single top-level dir name (= archive name, which encodes version/sha12/
// os/arch), and sha256 of WeKnora-lite, LICENSE and THIRD_PARTY_NOTICES.md.
// os/arch must equal the running process. The #178 manifest signature is
// checked by a developer when the pin is changed, not at runtime.
//
// Main flow only (project rule "只推主干"): first install, and retry after a
// failure (a retry restarts the download from byte 0; no resume). Disk full,
// concurrent installs from two App instances, upgrades and old-dir cleanup are
// FUs.

import crypto from 'node:crypto'
import { execFile } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { Readable } from 'node:stream'
import { promisify } from 'node:util'
import { dataDir, joinLiteral } from './data-dir'
import {
  assertExtractedEntriesAreRegular,
  assertSafeTarMembers,
  makeInstalledTreeReadOnly,
  makeInstalledTreeWritableForRemoval,
  resolveLocalDownloadOverride,
} from './open-design-component'
import { WEKNORA_LITE_PIN, type WeknoraLitePin } from './weknora-lite-pin'

const execFileAsync = promisify(execFile)

export const WEKNORA_LITE_LOCAL_BASE_URL_ENV = 'A24_WEKNORA_LITE_LOCAL_BASE_URL'
export const WEKNORA_LITE_BINARY = 'WeKnora-lite'
const MARKER_FILE = '.agent24-component.json'
const DOWNLOAD_IDLE_TIMEOUT_MS = 30_000
const PROGRESS_THROTTLE_MS = 100
const MAX_REDIRECT_HOPS = 10
const DEFAULT_ALLOWED_DOWNLOAD_HOSTS = ['github.com', 'objects.githubusercontent.com', 'release-assets.githubusercontent.com']

export type WeknoraLiteState = 'unavailable' | 'not-installed' | 'downloading' | 'verifying' | 'installed' | 'failed'

/** Coarse failure class for the 3c UI:
 * - `network`: connection error, HTTP error, idle timeout, short body — retry makes sense;
 * - `integrity`: bytes arrived but do not match the pin (size overflow, sha256, layout);
 * - `other`: local I/O or anything else. */
export type WeknoraLiteFailure = 'network' | 'integrity' | 'other'

export interface WeknoraLiteStatus {
  state: WeknoraLiteState
  /** Bytes received so far (downloading). */
  received?: number
  /** Expected archive size (downloading / not-installed). */
  total?: number
  /** Install dir (installed). */
  dir?: string
  /** Absolute path of the WeKnora-lite program (installed). */
  binaryPath?: string
  /** Set with `failed`. */
  failure?: WeknoraLiteFailure
  /** Human-readable detail for logs (English, no secrets). */
  error?: string
  /** Why `unavailable`. */
  reason?: string
  /** The loopback dev override is active. */
  overrideActive?: boolean
}

export interface WeknoraLiteInstallerOptions {
  pin?: WeknoraLitePin
  /** Defaults to <data_dir>/components/weknora-lite. */
  componentsRoot?: string
  tarBinary?: string
  platform?: string
  arch?: string
  allowedHosts?: Iterable<string>
  idleTimeoutMs?: number
  env?: NodeJS.ProcessEnv
}

/** Thrown for "the bytes are not what the pin says" — maps to failure `integrity`. */
export class WeknoraLiteIntegrityError extends Error {}

/** Thrown for transport problems — maps to failure `network`. */
export class WeknoraLiteNetworkError extends Error {}
/** A local write failed while downloading (disk full, read-only volume): failure `other`. */
export class WeknoraLiteLocalError extends Error {}

export function defaultWeknoraLiteComponentsRoot(): string {
  return joinLiteral(dataDir(), 'components', 'weknora-lite')
}

export function weknoraLiteInstallDir(componentsRoot: string, pin: WeknoraLitePin): string {
  return path.join(componentsRoot, `${pin.sourceSha.slice(0, 7)}-${pin.os}-${pin.arch}`)
}

/** Name of the single top-level directory inside the archive (archive name without `.tar.gz`). */
export function weknoraLiteStageName(pin: WeknoraLitePin): string {
  return pin.archive.replace(/\.tar\.gz$/, '')
}

const HEX40 = /^[0-9a-f]{40}$/
const HEX64 = /^[0-9a-f]{64}$/

/** Shape check of a pin. Returns a reason string when the pin is unusable
 * (placeholder values, bad hex, non-positive size, archive name not ending in
 * `.tar.gz` or containing a path separator), otherwise null. */
export function pinProblem(pin: WeknoraLitePin): string | null {
  if (!HEX40.test(pin.sourceSha)) return 'pin sourceSha is not a 40-hex commit'
  for (const key of ['archiveSha256', 'binarySha256', 'licenseSha256', 'noticesSha256'] as const) {
    if (!HEX64.test(pin[key])) return `pin ${key} is not a sha256`
  }
  if (!Number.isSafeInteger(pin.archiveSize) || pin.archiveSize <= 0) return 'pin archiveSize is invalid'
  if (!pin.archive.endsWith('.tar.gz') || /[/\\]/.test(pin.archive) || pin.archive.startsWith('..')) return 'pin archive name is invalid'
  if (!pin.version || !pin.os || !pin.arch) return 'pin version/os/arch is empty'
  return null
}

interface Marker { sourceSha: string; archiveSha256: string; binarySha256: string }

/** Cheap check used by status(): marker exists and records this pin's
 * archiveSha256 and sourceSha, and the binary is a regular file. */
export function quickValidateWeknoraLiteInstall(dir: string, pin: WeknoraLitePin): boolean {
  try {
    const marker = JSON.parse(fs.readFileSync(path.join(dir, MARKER_FILE), 'utf8')) as Partial<Marker>
    if (marker.sourceSha !== pin.sourceSha || marker.archiveSha256 !== pin.archiveSha256) return false
    return fs.lstatSync(path.join(dir, WEKNORA_LITE_BINARY), { throwIfNoEntry: false })?.isFile() === true
  } catch {
    return false
  }
}

function sha256OfFileSync(filePath: string): string {
  const hash = crypto.createHash('sha256')
  const fd = fs.openSync(filePath, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW)
  try {
    const buffer = Buffer.alloc(1024 * 1024)
    for (;;) {
      const read = fs.readSync(fd, buffer, 0, buffer.length, null)
      if (read === 0) break
      hash.update(buffer.subarray(0, read))
    }
  } finally {
    fs.closeSync(fd)
  }
  return hash.digest('hex')
}

/** For 3b, right before every launch: re-hash the installed WeKnora-lite and
 * compare with pin.binarySha256. Throws WeknoraLiteIntegrityError on mismatch
 * or when the file is missing. Returns the binary path. */
export function verifyInstalledWeknoraLiteBinary(dir: string, pin: WeknoraLitePin = WEKNORA_LITE_PIN): string {
  const binary = path.join(dir, WEKNORA_LITE_BINARY)
  if (fs.lstatSync(binary, { throwIfNoEntry: false })?.isFile() !== true) {
    throw new WeknoraLiteIntegrityError('WeKnora Lite program is missing')
  }
  if (sha256OfFileSync(binary) !== pin.binarySha256) {
    throw new WeknoraLiteIntegrityError('WeKnora Lite program changed after install')
  }
  return binary
}

function containsSymlink(root: string): string | null {
  for (const entry of fs.readdirSync(root, { withFileTypes: true })) {
    const full = path.join(root, entry.name)
    if (entry.isSymbolicLink()) return full
    if (entry.isDirectory()) {
      const found = containsSymlink(full)
      if (found) return found
    }
  }
  return null
}

function sweepPartials(componentsRoot: string): void {
  if (!fs.statSync(componentsRoot, { throwIfNoEntry: false })?.isDirectory()) return
  for (const entry of fs.readdirSync(componentsRoot)) {
    if (entry.startsWith('.partial-')) {
      const full = path.join(componentsRoot, entry)
      makeInstalledTreeWritableForRemoval(full)
      fs.rmSync(full, { recursive: true, force: true })
    }
  }
}

function failureOf(error: unknown): WeknoraLiteFailure {
  if (error instanceof WeknoraLiteIntegrityError) return 'integrity'
  if (error instanceof WeknoraLiteNetworkError) return 'network'
  return 'other'
}

export class WeknoraLiteInstaller {
  private readonly pin: WeknoraLitePin
  private readonly componentsRoot: string
  private readonly tarBinary: string
  private readonly fetchFn: typeof fetch
  private readonly allowedHosts: Set<string>
  private readonly idleTimeoutMs: number
  private readonly override: URL | null
  private readonly url: string | null
  private readonly unavailableReason: string | null
  private current: WeknoraLiteStatus
  private installing: Promise<WeknoraLiteStatus> | null = null
  private activeAbort: AbortController | null = null
  private readonly listeners = new Set<(status: WeknoraLiteStatus) => void>()

  constructor(options: WeknoraLiteInstallerOptions = {}, fetchFn: typeof fetch = fetch) {
    this.pin = options.pin ?? WEKNORA_LITE_PIN
    this.componentsRoot = options.componentsRoot ?? defaultWeknoraLiteComponentsRoot()
    this.tarBinary = options.tarBinary ?? 'tar'
    this.fetchFn = fetchFn
    this.allowedHosts = new Set(options.allowedHosts ?? DEFAULT_ALLOWED_DOWNLOAD_HOSTS)
    this.idleTimeoutMs = options.idleTimeoutMs ?? DOWNLOAD_IDLE_TIMEOUT_MS
    this.override = resolveLocalDownloadOverride(options.env ?? process.env, WEKNORA_LITE_LOCAL_BASE_URL_ENV)
    const platform = options.platform ?? process.platform
    const arch = options.arch ?? process.arch
    const problem = pinProblem(this.pin)
    if (problem) {
      this.url = null
      this.unavailableReason = problem
    } else if (this.pin.os !== platform || this.pin.arch !== arch) {
      this.url = null
      this.unavailableReason = `WeKnora Lite is pinned for ${this.pin.os}-${this.pin.arch}, not ${platform}-${arch}`
    } else {
      this.url = this.override ? new URL(this.pin.archive, this.override).href : this.pin.url
      this.unavailableReason = null
    }
    if (this.url && this.override) {
      console.warn(`[weknora-lite] ${WEKNORA_LITE_LOCAL_BASE_URL_ENV} active: downloading from ${this.url} (loopback only; hashes still checked against the pin)`)
    }
    this.current = this.computeInitialStatus()
    try {
      sweepPartials(this.componentsRoot)
    } catch (error) {
      console.warn('[weknora-lite] could not sweep stale partial dirs', error)
    }
  }

  private installedStatus(): WeknoraLiteStatus {
    const dir = weknoraLiteInstallDir(this.componentsRoot, this.pin)
    return { state: 'installed', dir, binaryPath: path.join(dir, WEKNORA_LITE_BINARY) }
  }

  /** An existing valid install is recognised even when this run has no
   * download source (installed via the loopback override, relaunched without it). */
  private computeInitialStatus(): WeknoraLiteStatus {
    if (this.unavailableReason) return { state: 'unavailable', reason: this.unavailableReason }
    if (quickValidateWeknoraLiteInstall(weknoraLiteInstallDir(this.componentsRoot, this.pin), this.pin)) return this.installedStatus()
    return this.notInstalledStatus()
  }

  private notInstalledStatus(): WeknoraLiteStatus {
    if (!this.url) return { state: 'unavailable', reason: 'This build has no WeKnora Lite download source' }
    return { state: 'not-installed', total: this.pin.archiveSize }
  }

  /** Current status; an `installed` status is re-validated against disk on every read. */
  status(): WeknoraLiteStatus {
    if (this.current.state === 'installed' && !quickValidateWeknoraLiteInstall(this.current.dir ?? '', this.pin)) {
      this.current = this.notInstalledStatus()
    }
    return this.override && this.url ? { ...this.current, overrideActive: true } : { ...this.current }
  }

  /** Subscribe to status pushes (download progress throttled to ~100 ms). Returns an unsubscribe function. */
  onProgress(callback: (status: WeknoraLiteStatus) => void): () => void {
    this.listeners.add(callback)
    return () => this.listeners.delete(callback)
  }

  private setStatus(status: WeknoraLiteStatus): void {
    this.current = status
    // A final status ends the install before anyone hears of it, so a listener
    // may call install() again at once and get a new attempt (PR-Daemon #2104 Low 2).
    if (status.state === 'installed' || status.state === 'failed') this.installing = null
    queueMicrotask(() => {
      for (const listener of [...this.listeners]) {
        // A throwing listener neither reaches the process nor stops the others (Low 5).
        try {
          listener(this.status())
        } catch (error) {
          console.warn('[weknora-lite] install listener failed', error instanceof Error ? error.message : String(error))
        }
      }
    })
  }

  /** Start (or join the in-flight) install. Resolves with the final status;
   * never rejects. From `failed` it starts over — that is the retry. A no-op
   * when `unavailable` or already `installed`. */
  install(): Promise<WeknoraLiteStatus> {
    const state = this.status().state
    if (state === 'unavailable' || state === 'installed') return Promise.resolve(this.status())
    if (this.installing) return this.installing
    const pending = this.installOnce(this.url!)
    this.installing = pending
    void pending.finally(() => { if (this.installing === pending) this.installing = null })
    return pending
  }

  /** Cancel an in-flight install (App quit). Ends in `failed`; install() may be called again. */
  abort(reason = 'WeKnora Lite install was aborted'): void {
    this.activeAbort?.abort(new Error(reason))
  }

  private async installOnce(url: string): Promise<WeknoraLiteStatus> {
    const controller = new AbortController()
    this.activeAbort = controller
    let partialDir: string | null = null
    const throwIfAborted = (): void => {
      if (controller.signal.aborted) throw controller.signal.reason instanceof Error ? controller.signal.reason : new Error('aborted')
    }
    try {
      if (!url.startsWith('https://') && !this.isOverrideUrl(url)) throw new Error('WeKnora Lite download URL must use HTTPS')
      fs.mkdirSync(this.componentsRoot, { recursive: true, mode: 0o700 })
      fs.chmodSync(this.componentsRoot, 0o700)
      sweepPartials(this.componentsRoot)
      partialDir = path.join(this.componentsRoot, `.partial-${crypto.randomBytes(6).toString('hex')}`)
      fs.mkdirSync(partialDir, { mode: 0o700 })
      const archivePath = path.join(partialDir, 'archive.tar.gz')

      this.setStatus({ state: 'downloading', received: 0, total: this.pin.archiveSize })
      await this.download(url, archivePath, controller)
      throwIfAborted()

      this.setStatus({ state: 'verifying' })
      try {
        await assertSafeTarMembers(this.tarBinary, archivePath, controller.signal, 'WeKnora Lite component')
      } catch (error) {
        throwIfAborted()
        throw new WeknoraLiteIntegrityError((error as Error).message)
      }
      const extractRoot = path.join(partialDir, 'x')
      fs.mkdirSync(extractRoot, { mode: 0o700 })
      try {
        await execFileAsync(this.tarBinary, ['-xzf', archivePath, '--no-same-owner', '-C', extractRoot], { signal: controller.signal })
      } catch (error) {
        throwIfAborted()
        throw new WeknoraLiteIntegrityError(`WeKnora Lite archive did not extract: ${(error as Error).message}`)
      }
      throwIfAborted()
      const stage = this.verifyExtracted(extractRoot)
      const marker: Marker = { sourceSha: this.pin.sourceSha, archiveSha256: this.pin.archiveSha256, binarySha256: this.pin.binarySha256 }
      fs.writeFileSync(path.join(stage, MARKER_FILE), JSON.stringify(marker), { mode: 0o600 })
      throwIfAborted()

      const finalDir = weknoraLiteInstallDir(this.componentsRoot, this.pin)
      makeInstalledTreeWritableForRemoval(finalDir)
      fs.rmSync(finalDir, { recursive: true, force: true })
      fs.renameSync(stage, finalDir)
      makeInstalledTreeReadOnly(finalDir)
      fs.rmSync(partialDir, { recursive: true, force: true })
      partialDir = null
      this.setStatus(this.installedStatus())
      return this.status()
    } catch (error) {
      if (partialDir) {
        try {
          makeInstalledTreeWritableForRemoval(partialDir)
          fs.rmSync(partialDir, { recursive: true, force: true })
        } catch { /* swept again on the next install() */ }
      }
      this.setStatus({ state: 'failed', failure: failureOf(error), error: error instanceof Error ? error.message : String(error) })
      return this.status()
    } finally {
      if (this.activeAbort === controller) this.activeAbort = null
    }
  }

  /** Layout + hash checks on the extracted tree. Returns the stage dir. */
  private verifyExtracted(extractRoot: string): string {
    const entries = fs.readdirSync(extractRoot)
    const stageName = weknoraLiteStageName(this.pin)
    if (entries.length !== 1 || entries[0] !== stageName) {
      throw new WeknoraLiteIntegrityError(`WeKnora Lite archive layout is not ${stageName}/`)
    }
    const stage = path.join(extractRoot, stageName)
    if (!fs.lstatSync(stage).isDirectory()) throw new WeknoraLiteIntegrityError('WeKnora Lite archive top entry is not a directory')
    const link = containsSymlink(stage)
    if (link) throw new WeknoraLiteIntegrityError(`WeKnora Lite archive contains a symlink: ${path.relative(stage, link)}`)
    try {
      assertExtractedEntriesAreRegular(stage)
    } catch (error) {
      throw new WeknoraLiteIntegrityError((error as Error).message)
    }
    const expected: Array<[string, string]> = [
      [WEKNORA_LITE_BINARY, this.pin.binarySha256],
      ['LICENSE', this.pin.licenseSha256],
      ['THIRD_PARTY_NOTICES.md', this.pin.noticesSha256],
    ]
    for (const [name, sha] of expected) {
      const file = path.join(stage, name)
      if (fs.lstatSync(file, { throwIfNoEntry: false })?.isFile() !== true) throw new WeknoraLiteIntegrityError(`WeKnora Lite archive is missing ${name}`)
      if (sha256OfFileSync(file) !== sha) throw new WeknoraLiteIntegrityError(`WeKnora Lite ${name} does not match the pin`)
    }
    if ((fs.statSync(path.join(stage, WEKNORA_LITE_BINARY)).mode & 0o100) === 0) {
      throw new WeknoraLiteIntegrityError('WeKnora Lite program is not executable')
    }
    return stage
  }

  private isOverrideUrl(url: string): boolean {
    if (!this.override) return false
    try {
      return new URL(url).origin === this.override.origin
    } catch {
      return false
    }
  }

  /** Same redirect policy as Open Design: the loopback override is exempt on the first hop only. */
  private async fetchFollowingAllowedRedirects(initialUrl: string, signal: AbortSignal): Promise<Response> {
    let current = initialUrl
    for (let hop = 0; hop < MAX_REDIRECT_HOPS; hop++) {
      const parsed = new URL(current)
      const overrideHop = this.isOverrideUrl(current)
      if (hop > 0 && overrideHop) throw new Error(`WeKnora Lite local test source applies only to the first hop: ${current}`)
      if (!(hop === 0 && overrideHop)) {
        if (parsed.protocol !== 'https:') throw new Error(`WeKnora Lite download must use HTTPS at every hop: ${current}`)
        if (!this.allowedHosts.has(parsed.hostname)) throw new Error(`WeKnora Lite download host is not allow-listed: ${parsed.hostname}`)
      }
      let response: Response
      try {
        response = await this.fetchFn(current, { signal, redirect: 'manual' })
      } catch (error) {
        throw new WeknoraLiteNetworkError(`WeKnora Lite download failed: ${(signal.reason as Error | undefined)?.message ?? (error as Error).message}`)
      }
      if (response.status >= 300 && response.status < 400) {
        const location = response.headers.get('location')
        if (!location) throw new Error('WeKnora Lite download redirect is missing a Location header')
        current = new URL(location, current).toString()
        continue
      }
      return response
    }
    throw new Error('WeKnora Lite download exceeded the redirect hop limit')
  }

  private async download(url: string, destination: string, controller: AbortController): Promise<void> {
    const total = this.pin.archiveSize
    let idleTimer: NodeJS.Timeout | undefined
    let idleFired = false
    const resetIdle = (): void => {
      clearTimeout(idleTimer)
      idleTimer = setTimeout(() => {
        idleFired = true
        controller.abort(new WeknoraLiteNetworkError(`WeKnora Lite download idle-timed-out (${this.idleTimeoutMs}ms without data)`))
      }, this.idleTimeoutMs)
      idleTimer.unref?.()
    }
    const hash = crypto.createHash('sha256')
    let received = 0
    let lastEmit = 0
    const fd = fs.openSync(destination, fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_NOFOLLOW, 0o600)
    try {
      resetIdle()
      const response = await this.fetchFollowingAllowedRedirects(url, controller.signal)
      if (!response.ok) throw new WeknoraLiteNetworkError(`WeKnora Lite download failed: HTTP ${response.status}`)
      if (!response.body) throw new WeknoraLiteNetworkError('WeKnora Lite download returned no body')
      try {
        for await (const chunk of Readable.fromWeb(response.body as never) as AsyncIterable<Buffer>) {
          resetIdle()
          received += chunk.length
          if (received > total) throw new WeknoraLiteIntegrityError('WeKnora Lite download exceeded the pinned size')
          hash.update(chunk)
          try {
            fs.writeSync(fd, chunk)
          } catch (error) {
            // Local I/O (disk full, say), not the network: `other` (PR-Daemon #2104 Low 4).
            throw new WeknoraLiteLocalError(`WeKnora Lite download could not be written: ${(error as NodeJS.ErrnoException).code ?? (error as Error).message}`)
          }
          const now = Date.now()
          if (received === total || now - lastEmit >= PROGRESS_THROTTLE_MS) {
            this.setStatus({ state: 'downloading', received, total })
            lastEmit = now
          }
        }
      } catch (error) {
        void response.body.cancel().catch(() => {})
        if (error instanceof WeknoraLiteIntegrityError || error instanceof WeknoraLiteLocalError) throw error
        if (controller.signal.aborted && !idleFired) throw controller.signal.reason
        throw new WeknoraLiteNetworkError(`WeKnora Lite download interrupted: ${idleFired ? (controller.signal.reason as Error).message : (error as Error).message}`)
      }
    } catch (error) {
      if (controller.signal.aborted && !idleFired && !(error instanceof WeknoraLiteIntegrityError) && !(error instanceof WeknoraLiteLocalError)) throw controller.signal.reason
      throw error
    } finally {
      clearTimeout(idleTimer)
      fs.closeSync(fd)
    }
    if (received !== total) throw new WeknoraLiteNetworkError(`WeKnora Lite download ended early: ${received} of ${total} bytes`)
    if (hash.digest('hex') !== this.pin.archiveSha256) throw new WeknoraLiteIntegrityError('WeKnora Lite archive sha256 does not match the pin')
  }
}
