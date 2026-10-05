// Owner decision 2026-10-05: Open Design is no longer bundled inside the
// desktop installer (it alone grew the AppImage from 146MB to 934MB and the
// deb from 101MB to 622MB). electron-builder now only bakes a small manifest
// (`open-design-component.json`, produced by
// scripts/pack-open-design-component.mjs) describing a downloadable tarball.
// This module is the runtime installer: it turns that manifest into an
// installed `app/ open-design/ open-design-web-standalone/` tree under
// `~/.agent24/components/open-design/<odSha7>-<platform>-<arch>/`, the exact
// layout `CreativeServeWeb` already expects from `resourcesPath`.
//
// PR #670 review follow-ups implemented here (Opus, REQUEST_CHANGES):
// H2 — installed dirs are keyed by odSha/platform/arch only, which would
//      silently reuse an incompatible native module build across an Electron
//      version bump (same odSha, different tarball content). Fixed with a
//      marker file recording the manifest sha256 that produced the install;
//      any mismatch (or missing marker) is treated as not-installed.
// M1 — the symlink-escape check compared an un-resolved root against
//      fs.realpathSync'd targets; on macOS (`/tmp` -> `/private/tmp`) or any
//      root reached through a symlink, that mismatch rejected legitimate
//      relative symlinks. Fixed by resolving the root once up front, and the
//      literalTarget / "crosses another symlink" checks from
//      stage-open-design-resources.mjs's verifySymlinks are ported back in.
// M2 — stale `.partial-*` dirs from a crashed previous run are swept at
//      construction and at the start of every install().
// M4 — download progress is throttled to ~100ms between pushes (always
//      flushes the final one).
// M5 — downloads idle-timeout after 30s with no data, and `abort()` lets the
//      host (main.ts's before-quit) cancel an in-flight install; both paths
//      leave `install()` retryable afterward (state becomes `failed`, which
//      is not `installed`, so a later `install()` call runs again).
// L1 — a corrupt/unparsable baked manifest degrades to `unavailable` instead
//      of throwing out of the constructor (which ran inside
//      `app.whenReady().then()` with no try/catch in main.ts — an uncaught
//      throw there would have skipped registering the rest of the IPC
//      handlers).
// L2 — a manifest whose platform/arch doesn't match the running process is
//      also treated as unavailable rather than attempted.
// L3 — redirects are followed manually (`redirect: 'manual'`), re-validating
//      HTTPS and an explicit host allowlist at every hop, instead of trusting
//      fetch's automatic follow to go wherever the server says.
// Hardening — extraction adds `--no-same-owner`, and the tarball's member
// list is checked for `..`/absolute path entries before extraction even
// starts (defense in depth on top of the post-extraction symlink walk, which
// only inspects symlinks, not regular-file path traversal).

import crypto from 'node:crypto'
import { execFile } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { Readable } from 'node:stream'
import { promisify } from 'node:util'

const execFileAsync = promisify(execFile)

export const OPEN_DESIGN_COMPONENT_MANIFEST_FILE = 'open-design-component.json'
const COMPONENT_MARKER_FILE = '.agent24-component.json'
const RESOURCE_TREES = ['app', 'open-design', 'open-design-web-standalone'] as const
const REQUIRED_FILES = [
  'app/package.json',
  'app/prebundled/agent24-headless.cjs',
  'app/prebundled/agent24-headless.mjs',
  'app/prebundled/daemon/daemon-cli.mjs',
  'app/prebundled/daemon/daemon-sidecar.mjs',
  'app/prebundled/web-sidecar.mjs',
]
const DOWNLOAD_IDLE_TIMEOUT_MS = 30_000
const PROGRESS_THROTTLE_MS = 100
const MAX_REDIRECT_HOPS = 10
const DEFAULT_ALLOWED_DOWNLOAD_HOSTS = ['github.com', 'objects.githubusercontent.com', 'release-assets.githubusercontent.com']

export interface OpenDesignComponentManifest {
  version: string
  odSha: string
  platform: string
  arch: string
  url: string
  sha256: string
  size: number
}

export type OpenDesignComponentState =
  | 'not-installed'
  | 'downloading'
  | 'verifying'
  | 'installed'
  | 'failed'
  | 'unavailable'

export interface OpenDesignComponentStatus {
  state: OpenDesignComponentState
  received?: number
  total?: number
  size?: number
  dir?: string
  error?: string
  reason?: string
}

type FetchFn = typeof fetch

function isNonEmptyString(value: unknown): value is string {
  return typeof value === 'string' && value.length > 0
}

/** Defensive shape check even for the manifest baked at build time — a hand
 * edited or truncated build artifact must fail loudly, not silently install
 * garbage. */
export function parseOpenDesignComponentManifest(raw: unknown): OpenDesignComponentManifest {
  if (raw == null || typeof raw !== 'object' || Array.isArray(raw)) {
    throw new Error('Open Design component manifest must be an object')
  }
  const value = raw as Record<string, unknown>
  if (!isNonEmptyString(value.version)) throw new Error('Open Design component manifest version is invalid')
  if (!isNonEmptyString(value.odSha) || !/^[0-9a-f]{40}$/.test(value.odSha)) {
    throw new Error('Open Design component manifest odSha is invalid')
  }
  if (!isNonEmptyString(value.platform)) throw new Error('Open Design component manifest platform is invalid')
  if (!isNonEmptyString(value.arch)) throw new Error('Open Design component manifest arch is invalid')
  if (!isNonEmptyString(value.url)) throw new Error('Open Design component manifest url is invalid')
  if (!isNonEmptyString(value.sha256) || !/^[0-9a-f]{64}$/.test(value.sha256)) {
    throw new Error('Open Design component manifest sha256 is invalid')
  }
  if (typeof value.size !== 'number' || !Number.isInteger(value.size) || value.size <= 0) {
    throw new Error('Open Design component manifest size is invalid')
  }
  return {
    version: value.version,
    odSha: value.odSha,
    platform: value.platform,
    arch: value.arch,
    url: value.url,
    sha256: value.sha256,
    size: value.size,
  }
}

/** Reads the manifest electron-builder baked into `resourcesPath` at build
 * time. Returns `null` for a dev/unreleased build that never baked one, OR
 * (L1) for a baked-but-corrupt manifest — either way the caller surfaces
 * `unavailable`, not an uncaught throw out of the constructor. */
export function loadOpenDesignComponentManifest(resourcesPath?: string): OpenDesignComponentManifest | null {
  if (!resourcesPath) return null
  const manifestPath = path.join(resourcesPath, OPEN_DESIGN_COMPONENT_MANIFEST_FILE)
  if (!fs.existsSync(manifestPath)) return null
  try {
    return parseOpenDesignComponentManifest(JSON.parse(fs.readFileSync(manifestPath, 'utf8')))
  } catch (error) {
    console.error('[open-design-component] baked manifest is corrupt, treating as unavailable', error)
    return null
  }
}

export function openDesignComponentInstallDir(componentsRoot: string, manifest: OpenDesignComponentManifest): string {
  return path.join(componentsRoot, `${manifest.odSha.slice(0, 7)}-${manifest.platform}-${manifest.arch}`)
}

/** Cheap re-validation (safe to call on every `status()`, not just once at
 * startup — see H2/the self-healing `status()` below): required files
 * present AND (H2) the installed marker's sha256 matches the CURRENT
 * manifest's sha256. A stale install from before an Electron ABI bump (same
 * odSha/platform/arch, different tarball content) fails this and is treated
 * as not-installed rather than silently reused. */
export function quickValidateInstalledComponent(dir: string, expectedSha256: string): boolean {
  if (!fs.existsSync(dir)) return false
  const requiredOk = REQUIRED_FILES.every((relative) => fs.statSync(path.join(dir, relative), { throwIfNoEntry: false })?.isFile())
  if (!requiredOk) return false
  const markerPath = path.join(dir, COMPONENT_MARKER_FILE)
  if (!fs.statSync(markerPath, { throwIfNoEntry: false })?.isFile()) return false
  try {
    const marker = JSON.parse(fs.readFileSync(markerPath, 'utf8')) as { sha256?: unknown }
    return marker.sha256 === expectedSha256
  } catch {
    return false
  }
}

function isInsideResourceTrees(root: string, target: string): boolean {
  return RESOURCE_TREES.some((tree) => {
    const treeRoot = path.join(root, tree)
    return target === treeRoot || target.startsWith(`${treeRoot}${path.sep}`)
  })
}

/** Ported verbatim (semantics) from stage-open-design-resources.mjs's
 * verifySymlinks: text-target containment (literalTarget, before resolving
 * any further hops) AND fully-resolved containment (resolved) must both hold
 * and must agree with each other — a symlink is rejected if its immediate
 * target is itself another symlink ("crosses another symlink"), not just if
 * the final destination escapes. `root` must already be fully resolved
 * (realpathSync'd) by the caller — see M1. */
function verifySymlinksRecursive(root: string, directory: string): void {
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const fullPath = path.join(directory, entry.name)
    if (entry.isSymbolicLink()) {
      const link = fs.readlinkSync(fullPath)
      if (path.isAbsolute(link)) throw new Error(`Open Design component has an absolute symlink: ${fullPath}`)
      const literalTarget = path.resolve(path.dirname(fullPath), link)
      if (!isInsideResourceTrees(root, literalTarget)) {
        throw new Error(`Open Design component symlink text escapes its resource trees: ${fullPath}`)
      }
      const resolved = fs.realpathSync(fullPath)
      if (!isInsideResourceTrees(root, resolved)) {
        throw new Error(`Open Design component symlink escapes its resource trees: ${fullPath}`)
      }
      if (literalTarget !== resolved) {
        throw new Error(`Open Design component symlink crosses another symlink: ${fullPath}`)
      }
      continue
    }
    if (entry.isDirectory()) verifySymlinksRecursive(root, fullPath)
  }
}

/** Ported from `scripts/stage-open-design-resources.mjs`'s build-time
 * contract — required files, pinned version, and symlink safety — run once
 * against a freshly extracted tarball before it is trusted and renamed into
 * place.
 *
 * M1: `root` is realpathSync'd once up front so every comparison inside the
 * symlink walk operates on fully-resolved paths — without this, a `root`
 * reached through any symlink component (e.g. macOS's `/tmp` ->
 * `/private/tmp`) would wrongly reject every legitimate relative symlink,
 * because `fs.realpathSync` on the symlink's target resolves through that
 * component while the un-resolved `root` string never did. */
export function verifyComponentContract(root: string, expectedVersion: string): void {
  for (const tree of RESOURCE_TREES) {
    const treeRoot = path.join(root, tree)
    const stat = fs.lstatSync(treeRoot, { throwIfNoEntry: false })
    if (stat?.isSymbolicLink()) throw new Error(`Open Design component resource tree root must not be a symlink: ${tree}`)
    if (!stat?.isDirectory()) throw new Error(`Open Design component resource tree is missing: ${tree}`)
  }
  for (const relative of REQUIRED_FILES) {
    if (!fs.statSync(path.join(root, relative), { throwIfNoEntry: false })?.isFile()) {
      throw new Error(`Open Design component is missing a required file: ${relative}`)
    }
  }
  const manifest = JSON.parse(fs.readFileSync(path.join(root, 'app/package.json'), 'utf8')) as { version?: unknown }
  if (manifest.version !== expectedVersion) {
    throw new Error(`Open Design component version mismatch: expected ${expectedVersion}, got ${String(manifest.version)}`)
  }

  const resolvedRoot = fs.realpathSync(root)
  for (const tree of RESOURCE_TREES) verifySymlinksRecursive(resolvedRoot, path.join(resolvedRoot, tree))
}

/** Hardening on top of the post-extraction symlink walk above, which only
 * inspects symlink entries: a REGULAR file or directory entry whose path
 * contains `..` or starts with `/` could still write outside the extraction
 * directory during `tar -x` itself, before any of our verification code
 * runs. Reject the whole tarball before extracting anything. */
async function assertSafeTarMembers(tarBinary: string, tarballPath: string): Promise<void> {
  const { stdout } = await execFileAsync(tarBinary, ['-tzf', tarballPath])
  for (const rawName of stdout.split('\n').map((line) => line.trim()).filter(Boolean)) {
    const name = rawName.endsWith('/') ? rawName.slice(0, -1) : rawName
    if (name.startsWith('/') || name.split('/').includes('..')) {
      throw new Error(`Open Design component tarball contains an unsafe path entry: ${rawName}`)
    }
  }
}

function cleanupStalePartials(componentsRoot: string): void {
  if (!fs.statSync(componentsRoot, { throwIfNoEntry: false })?.isDirectory()) return
  for (const entry of fs.readdirSync(componentsRoot)) {
    if (entry.startsWith('.partial-')) {
      fs.rmSync(path.join(componentsRoot, entry), { recursive: true, force: true })
    }
  }
}

export interface OpenDesignComponentInstallerOptions {
  manifest?: OpenDesignComponentManifest | null
  resourcesPath?: string
  componentsRoot?: string
  tarBinary?: string
  /** L2: defaults to the real running process — injectable for tests. */
  platform?: string
  arch?: string
  /** L3: hosts the download (including every redirect hop) is allowed to
   * reach. Defaults to GitHub's release-asset hosts; tests override this to
   * exercise the download pipeline against a local fixture server while
   * still keeping the allowlist mechanism itself real (see its own test
   * against the untouched default). */
  allowedHosts?: Iterable<string>
  /** M5: ms of no data before an in-flight download is aborted. Defaults to
   * 30s; tests shrink this instead of waiting out a real 30s timer. */
  idleTimeoutMs?: number
}

export class OpenDesignComponentInstaller {
  private readonly manifest: OpenDesignComponentManifest | null
  private readonly componentsRoot: string
  private readonly tarBinary: string
  private readonly fetchFn: FetchFn
  private readonly allowedHosts: Set<string>
  private readonly idleTimeoutMs: number
  private current: OpenDesignComponentStatus
  private installing: Promise<OpenDesignComponentStatus> | null = null
  private activeAbort: AbortController | null = null
  private readonly listeners = new Set<(status: OpenDesignComponentStatus) => void>()

  constructor(options: OpenDesignComponentInstallerOptions = {}, fetchFn: FetchFn = fetch) {
    const loaded = options.manifest !== undefined ? options.manifest : loadOpenDesignComponentManifest(options.resourcesPath)
    const platform = options.platform ?? process.platform
    const arch = options.arch ?? process.arch
    if (loaded && (loaded.platform !== platform || loaded.arch !== arch)) {
      // L2: a manifest built for a different platform/arch than this process
      // is running on (e.g. a packaging mistake) must never be attempted.
      console.error(
        `[open-design-component] baked manifest platform/arch (${loaded.platform}-${loaded.arch}) ` +
        `does not match the running process (${platform}-${arch}); treating as unavailable`,
      )
      this.manifest = null
    } else {
      this.manifest = loaded
    }
    this.componentsRoot = options.componentsRoot ?? path.join(os.homedir(), '.agent24', 'components', 'open-design')
    this.tarBinary = options.tarBinary ?? 'tar'
    this.fetchFn = fetchFn
    this.allowedHosts = new Set(options.allowedHosts ?? DEFAULT_ALLOWED_DOWNLOAD_HOSTS)
    this.idleTimeoutMs = options.idleTimeoutMs ?? DOWNLOAD_IDLE_TIMEOUT_MS
    this.current = this.computeInitialStatus()
    cleanupStalePartials(this.componentsRoot)
  }

  private computeInitialStatus(): OpenDesignComponentStatus {
    if (!this.manifest) {
      return { state: 'unavailable', reason: 'This build does not bundle an Open Design component manifest' }
    }
    const dir = openDesignComponentInstallDir(this.componentsRoot, this.manifest)
    if (quickValidateInstalledComponent(dir, this.manifest.sha256)) return { state: 'installed', dir }
    return { state: 'not-installed', size: this.manifest.size }
  }

  /** H2: self-healing — an `installed` status is cheaply re-checked against
   * disk (required files + marker sha256) every time it's read, not just
   * once at construction. If the installed dir was deleted, corrupted, or
   * is a stale pre-ABI-bump install, this demotes to `not-installed` so the
   * renderer's existing "下载" button doubles as the "重新下载" entry point
   * the install() guard below also relies on this, not a separately cached
   * flag, so a deleted dir is always retried rather than permanently stuck. */
  status(): OpenDesignComponentStatus {
    if (this.current.state === 'installed' && this.manifest) {
      if (!this.current.dir || !quickValidateInstalledComponent(this.current.dir, this.manifest.sha256)) {
        this.current = { state: 'not-installed', size: this.manifest.size }
      }
    }
    return { ...this.current }
  }

  onProgress(callback: (status: OpenDesignComponentStatus) => void): () => void {
    this.listeners.add(callback)
    return () => this.listeners.delete(callback)
  }

  private setStatus(status: OpenDesignComponentStatus): void {
    this.current = status
    for (const listener of this.listeners) listener(this.status())
  }

  /** M5: lets the host cancel an in-flight install (e.g. main.ts's
   * `before-quit`) without leaving a partial dir or a dangling connection.
   * A no-op if nothing is in flight. */
  abort(reason = 'Open Design component install was aborted'): void {
    this.activeAbort?.abort(new Error(reason))
  }

  install(): Promise<OpenDesignComponentStatus> {
    if (!this.manifest) return Promise.resolve(this.status())
    if (this.status().state === 'installed') return Promise.resolve(this.status())
    if (this.installing) return this.installing
    const pending = this.installOnce(this.manifest)
    this.installing = pending
    pending.then(
      () => { if (this.installing === pending) this.installing = null },
      () => { if (this.installing === pending) this.installing = null },
    )
    return pending
  }

  private async installOnce(manifest: OpenDesignComponentManifest): Promise<OpenDesignComponentStatus> {
    if (!manifest.url.startsWith('https://')) {
      this.setStatus({ state: 'failed', error: 'Open Design component manifest URL must use HTTPS' })
      return this.status()
    }

    fs.mkdirSync(this.componentsRoot, { recursive: true })
    cleanupStalePartials(this.componentsRoot)
    const token = crypto.randomBytes(6).toString('hex')
    const partialDir = path.join(this.componentsRoot, `.partial-${token}`)
    const tarballPath = path.join(this.componentsRoot, `.partial-${token}.tar.gz`)
    const controller = new AbortController()
    this.activeAbort = controller

    try {
      this.setStatus({ state: 'downloading', received: 0, total: manifest.size })
      await this.download(manifest, tarballPath, controller)

      this.setStatus({ state: 'verifying' })
      await assertSafeTarMembers(this.tarBinary, tarballPath)
      fs.mkdirSync(partialDir, { recursive: true })
      await execFileAsync(this.tarBinary, ['-xzf', tarballPath, '--no-same-owner', '-C', partialDir])
      verifyComponentContract(partialDir, manifest.version)
      fs.writeFileSync(path.join(partialDir, COMPONENT_MARKER_FILE), JSON.stringify({ sha256: manifest.sha256 }))

      const finalDir = openDesignComponentInstallDir(this.componentsRoot, manifest)
      fs.rmSync(finalDir, { recursive: true, force: true })
      fs.renameSync(partialDir, finalDir)
      this.setStatus({ state: 'installed', dir: finalDir })
      return this.status()
    } catch (error) {
      fs.rmSync(partialDir, { recursive: true, force: true })
      const message = error instanceof Error ? error.message : String(error)
      this.setStatus({ state: 'failed', error: message })
      return this.status()
    } finally {
      if (this.activeAbort === controller) this.activeAbort = null
      fs.rmSync(tarballPath, { force: true })
    }
  }

  /** L3: follows redirects manually, re-checking HTTPS + the host allowlist
   * at every hop — fetch's automatic `redirect: 'follow'` would trust
   * wherever the server points next without either check. */
  private async fetchWithAllowedRedirects(initialUrl: string, signal: AbortSignal): Promise<Response> {
    let current = initialUrl
    for (let hop = 0; hop < MAX_REDIRECT_HOPS; hop++) {
      const parsed = new URL(current)
      if (parsed.protocol !== 'https:') {
        throw new Error(`Open Design component download must use HTTPS at every hop: ${current}`)
      }
      if (!this.allowedHosts.has(parsed.hostname)) {
        throw new Error(`Open Design component download host is not allow-listed: ${parsed.hostname}`)
      }
      const response = await this.fetchFn(current, { signal, redirect: 'manual' })
      if (response.status >= 300 && response.status < 400) {
        const location = response.headers.get('location')
        if (!location) throw new Error('Open Design component download redirect is missing a Location header')
        current = new URL(location, current).toString()
        continue
      }
      return response
    }
    throw new Error('Open Design component download exceeded the redirect hop limit')
  }

  private async download(manifest: OpenDesignComponentManifest, destination: string, controller: AbortController): Promise<void> {
    let idleTimer: NodeJS.Timeout | undefined
    const resetIdleTimer = (): void => {
      clearTimeout(idleTimer)
      idleTimer = setTimeout(
        () => controller.abort(new Error(`Open Design component download idle-timed-out (${this.idleTimeoutMs}ms without data)`)),
        this.idleTimeoutMs,
      )
      idleTimer.unref?.()
    }

    resetIdleTimer()
    let response: Response
    try {
      response = await this.fetchWithAllowedRedirects(manifest.url, controller.signal)
    } finally {
      clearTimeout(idleTimer)
    }
    if (!response.ok) throw new Error(`Open Design component download failed: HTTP ${response.status}`)
    if (!response.body) throw new Error('Open Design component download returned no body')

    const hash = crypto.createHash('sha256')
    let received = 0
    let lastEmit = 0
    const file = fs.createWriteStream(destination)
    // We handle every failure ourselves (via the catch below and the
    // caller's try/catch); without this, a stray write/close error after we
    // have already moved on would surface as an unhandled 'error' event and
    // crash the process (Node's EventEmitter default for a listener-less
    // 'error' emission).
    file.on('error', () => {})
    const nodeStream = Readable.fromWeb(response.body as never)

    try {
      resetIdleTimer()
      for await (const chunk of nodeStream as AsyncIterable<Buffer>) {
        resetIdleTimer()
        received += chunk.length
        if (received > manifest.size) {
          throw new Error('Open Design component download exceeded the manifest-declared size')
        }
        hash.update(chunk)
        if (!file.write(chunk)) await new Promise<void>((resolve) => file.once('drain', () => resolve()))
        const now = Date.now()
        if (received === manifest.size || now - lastEmit >= PROGRESS_THROTTLE_MS) {
          this.setStatus({ state: 'downloading', received, total: manifest.size })
          lastEmit = now
        }
      }
      await new Promise<void>((resolve, reject) => {
        file.end((error: unknown) => (error ? reject(error) : resolve()))
      })
    } catch (error) {
      // Wait for the fd to actually close before the caller deletes this
      // path — fs.WriteStream.destroy() can leave an in-flight open/write
      // that still touches the path after we return.
      await new Promise<void>((resolve) => file.close(() => resolve()))
      void response.body?.cancel?.().catch(() => {})
      throw error
    } finally {
      clearTimeout(idleTimer)
    }

    if (received !== manifest.size) {
      throw new Error(`Open Design component download size mismatch: expected ${manifest.size}, got ${received}`)
    }
    const digest = hash.digest('hex')
    if (digest !== manifest.sha256) {
      throw new Error(`Open Design component sha256 mismatch: expected ${manifest.sha256}, got ${digest}`)
    }
  }
}
