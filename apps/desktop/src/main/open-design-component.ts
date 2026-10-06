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
import { execFile, spawn } from 'node:child_process'
import { once } from 'node:events'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import readline from 'node:readline'
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
/** M4 (Opus re-review of the fork PR, 2026-10-06): "self-checking is not a
 * boundary" — the fork process checking its own resourceSafeBase chain
 * from inside itself happens AFTER this process already decided to spawn
 * it. The actual boundary has to be enforced by this host, before spawn:
 * record the sha256 of exactly the files the headless launcher will
 * execute at install time (this installer, right after extraction —
 * before the tree is made read-only below), so creative-serve-web.ts can
 * spot-check them again immediately before every launch. Deliberately a
 * SUBSET of REQUIRED_FILES — these four are the ones that actually run as
 * code; app/package.json and the two .mjs files that exist only to
 * satisfy the contract check are not. */
const ENTRY_FILES_FOR_INTEGRITY_CHECK = [
  'app/prebundled/agent24-headless.cjs',
  'app/prebundled/daemon/daemon-cli.mjs',
  'app/prebundled/daemon/daemon-sidecar.mjs',
  'app/prebundled/web-sidecar.mjs',
] as const
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

/** Not a frozen module-level constant — reads os.homedir() fresh every
 * call, matching the constructor's own (pre-existing) behavior of
 * resolving it at construction time rather than at module load. */
function defaultComponentsRoot(): string {
  return path.join(os.homedir(), '.agent24', 'components', 'open-design')
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

interface ComponentMarker {
  sha256: string
  entrySha256: Record<string, string>
}

function sha256OfFile(filePath: string): string {
  return crypto.createHash('sha256').update(fs.readFileSync(filePath)).digest('hex')
}

function readComponentMarker(dir: string): ComponentMarker | null {
  try {
    const parsed = JSON.parse(fs.readFileSync(path.join(dir, COMPONENT_MARKER_FILE), 'utf8')) as Partial<ComponentMarker>
    if (typeof parsed.sha256 !== 'string' || parsed.entrySha256 == null || typeof parsed.entrySha256 !== 'object') return null
    return { sha256: parsed.sha256, entrySha256: parsed.entrySha256 as Record<string, string> }
  } catch {
    return null
  }
}

/** M4 (Opus re-review, 2026-10-06): re-hashes the launcher entry files
 * under `dir` and compares against the marker this installer wrote at
 * install time (right after extraction, before the tree below is made
 * read-only). Intended to be called by creative-serve-web.ts immediately
 * before every launch — independent of, and in addition to, the read-only
 * permission bits makeInstalledTreeReadOnly sets: permissions stop a
 * normal process from writing here at all, but this spot-check still
 * catches anything that managed to change the content anyway (e.g. a
 * privileged process, or a bug elsewhere in this installer) without
 * trusting mode bits alone. */
export function verifyEntryFileIntegrity(dir: string): void {
  const marker = readComponentMarker(dir)
  if (marker == null) throw new Error('Open Design component is missing its integrity marker')
  for (const relative of ENTRY_FILES_FOR_INTEGRITY_CHECK) {
    const expected = marker.entrySha256[relative]
    if (typeof expected !== 'string' || expected.length === 0) {
      throw new Error(`Open Design component integrity marker is missing a hash for ${relative}`)
    }
    const actual = sha256OfFile(path.join(dir, relative))
    if (actual !== expected) {
      throw new Error(`Open Design component entry file changed after install: ${relative}`)
    }
  }
}

// M1 (Opus re-review, 2026-10-06): sticky-bit directories (e.g. /tmp, mode
// 1777) are the standard safe exception to "group/other writable" — see
// the matching fork-side comment (iDoris-ai/open-design-agent24#7,
// apps/packaged/src/agent24-headless.ts) for the full reasoning; kept
// identical here so the host and the launcher agree on what "safe" means.
function hasUnsafeAncestorWriteBits(mode: number): boolean {
  if ((mode & 0o1000) !== 0) return false
  return (mode & 0o022) !== 0
}

function ancestorOwnerIsTrusted(uid: number): boolean {
  return uid === process.getuid!() || uid === 0
}

function chainFromAncestor(ancestor: string, target: string): string[] {
  const rel = path.relative(ancestor, target)
  if (rel === '') return [ancestor]
  const segments = rel.split(path.sep).filter((segment) => segment.length > 0)
  const chain = [ancestor]
  let acc = ancestor
  for (const segment of segments) {
    acc = path.join(acc, segment)
    chain.push(acc)
  }
  return chain
}

function rootToTargetChain(target: string): string[] {
  const segments = target.split(path.sep).filter((segment) => segment.length > 0)
  const chain: string[] = []
  let acc = ''
  for (const segment of segments) {
    acc = `${acc}${path.sep}${segment}`
    chain.push(acc)
  }
  return chain
}

/** M1/M4 ("self-checking is not a boundary", Opus re-review of
 * iDoris-ai/open-design-agent24#7, 2026-10-06): the fork's own
 * ancestor-chain check runs INSIDE the already-spawned headless launcher
 * process — strictly later than THIS process (creative-serve-web.ts)
 * deciding to spawn it at all. This is the same check (ownership + no
 * unsafe group/other write bits, sticky-bit excepted, walked from $HOME
 * down to resourceSafeBase), run by the host, before spawn — defense in
 * depth, not a replacement for the fork's own check. POSIX-only: Windows
 * has no uid/mode-writable-bits model here, matching the fork's own
 * win32 fail-closed behavior for an explicit resourceSafeBase. */
export function assertResourceSafeBaseAncestryIsSafe(safeBase: string): void {
  if (typeof process.getuid !== 'function') return
  const home = os.homedir()
  const relativeToHome = path.relative(home, safeBase)
  const underHome = relativeToHome === '' || (!relativeToHome.startsWith('..') && !path.isAbsolute(relativeToHome))
  const chain = underHome ? chainFromAncestor(home, safeBase) : rootToTargetChain(safeBase)
  for (const target of chain) {
    const info = fs.lstatSync(target)
    if (!ancestorOwnerIsTrusted(info.uid)) {
      throw new Error(`Open Design resourceSafeBase ancestor is not owned by the current user or root: ${target}`)
    }
    if (hasUnsafeAncestorWriteBits(info.mode)) {
      throw new Error(`Open Design resourceSafeBase ancestor is group- or other-writable without the sticky bit: ${target}`)
    }
  }
}

/** M4: after a successful install, nothing should be able to modify the
 * installed tree's content again short of deleting and reinstalling it —
 * chmod every directory to 0500 (owner read+execute, no write — not even
 * for the owner) and every file to 0444 (owner/group/other read-only).
 * Bottom-up (children before their parent) so this process's own
 * traversal never needs write access to a directory it just locked down;
 * chmod itself only needs ownership of the target, never write permission
 * on its parent. Symlinks are left untouched — verifyComponentContract's
 * own symlink walk (run just before this, via the caller) already proved
 * every one of them resolves inside the installed tree. */
function makeInstalledTreeReadOnly(root: string): void {
  const stat = fs.lstatSync(root)
  if (stat.isSymbolicLink()) return
  if (stat.isDirectory()) {
    for (const entry of fs.readdirSync(root)) makeInstalledTreeReadOnly(path.join(root, entry))
    // Codex re-review (2026-10-06): hardcoding 0o500/0o444 here stripped
    // the EXECUTE bit from every file, including ones that legitimately
    // need it — e.g. @ffmpeg-installer's bundled ffmpeg binary, which the
    // fork's own daemon spawns directly (spawn(ffmpegInstaller.path)).
    // That made video export / cover-image generation fail with EACCES
    // the moment the tree went read-only, reproduced for real. Clear only
    // the write bits (owner/group/other) and keep whatever read/execute
    // bits tar extraction already set — "read-only" should mean exactly
    // that, not "read-only AND non-executable".
    fs.chmodSync(root, (stat.mode & 0o777) & ~0o222)
  } else if (stat.isFile()) {
    fs.chmodSync(root, (stat.mode & 0o777) & ~0o222)
  }
}

/** Counterpart to makeInstalledTreeReadOnly, needed before this installer
 * can remove an OLD install at the same dir to make room for a re-install
 * (manifest/version upgrade at the same odSha/platform/arch) — deleting a
 * directory entry needs write on its PARENT, which 0500 deliberately
 * removed. Top-down (parent before children) is fine either way here:
 * 0500 already permits readdir/traversal, this just restores write too. */
function makeInstalledTreeWritableForRemoval(root: string): void {
  const stat = fs.lstatSync(root, { throwIfNoEntry: false })
  if (stat == null || stat.isSymbolicLink()) return
  if (stat.isDirectory()) {
    fs.chmodSync(root, 0o700)
    for (const entry of fs.readdirSync(root)) makeInstalledTreeWritableForRemoval(path.join(root, entry))
  } else if (stat.isFile()) {
    fs.chmodSync(root, 0o600)
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
  // M1 (review finding, 2026-10-05): this used to buffer the whole listing
  // via execFileAsync with a fixed maxBuffer. Real-app verification already
  // showed a real open-design-web-standalone/node_modules tree can exceed
  // 1MB of listing text — but a FIXED ceiling (we had raised it to 64MB) is
  // never actually safe against an arbitrarily large (or maliciously
  // padded) tarball; it only moves the failure threshold. spawn + readline
  // processes the listing one line at a time and never holds more than a
  // single line in memory.
  const child = spawn(tarBinary, ['-tzf', tarballPath], { stdio: ['ignore', 'pipe', 'pipe'] })
  // Register the 'exit' listener BEFORE draining stdout below. `tar -tzf`
  // on a small tarball can exit before the readline loop over stdout even
  // finishes draining — 'exit' only fires once per process lifetime, so a
  // listener attached after the loop can miss an event that already fired,
  // and `await`ing it then hangs forever. once() queues immediately and
  // resolves whenever the event lands, so there is no ordering race.
  const exitPromise = once(child, 'exit') as Promise<[number | null, NodeJS.Signals | null]>
  const rl = readline.createInterface({ input: child.stdout, crlfDelay: Infinity })
  let stderr = ''
  child.stderr.on('data', (chunk: Buffer) => { stderr = (stderr + chunk.toString()).slice(-4_000) })

  let unsafeEntry: string | null = null
  try {
    for await (const rawLine of rl) {
      const line = rawLine.trim()
      if (!line) continue
      const name = line.endsWith('/') ? line.slice(0, -1) : line
      if (name.startsWith('/') || name.split('/').includes('..')) {
        unsafeEntry = line
        break
      }
    }
  } finally {
    rl.close()
  }

  if (unsafeEntry) {
    child.kill()
    throw new Error(`Open Design component tarball contains an unsafe path entry: ${unsafeEntry}`)
  }

  const [exitCode] = await exitPromise
  if (exitCode !== 0) {
    throw new Error(`Open Design component tarball listing failed (tar -tzf exit ${exitCode}): ${stderr.trim()}`)
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
    this.componentsRoot = options.componentsRoot ?? defaultComponentsRoot()
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

    // M1 (Opus re-review, 2026-10-06): the fork's explicit resourceSafeBase
    // check (iDoris-ai/open-design-agent24#7) walks the full ancestor chain
    // from $HOME down to the safe base, rejecting any level that isn't
    // owned by the current user or is group/other-writable. Explicitly
    // create AND chmod both ~/.agent24/components and its open-design
    // subdirectory (this.componentsRoot) as 0700 — not just the leaf
    // install dir below — rather than relying on mkdirSync's `mode` option
    // alone (not guaranteed to apply to every intermediate directory it
    // creates) or on whatever a prior run or the host's umask already left
    // in place.
    //
    // Only reach up to componentsRoot's PARENT when componentsRoot is the
    // real default (~/.agent24/components/open-design) — this installer
    // only actually owns that one specific directory tree. A test (or any
    // future caller) that overrides componentsRoot to some unrelated
    // standalone location must never have THIS installer start chmod'ing
    // that override's parent, which could be anything up to a shared
    // system temp directory it has no business touching.
    if (this.componentsRoot === defaultComponentsRoot()) {
      const componentsRootParent = path.dirname(this.componentsRoot)
      fs.mkdirSync(componentsRootParent, { recursive: true, mode: 0o700 })
      fs.chmodSync(componentsRootParent, 0o700)
    }
    fs.mkdirSync(this.componentsRoot, { recursive: true, mode: 0o700 })
    fs.chmodSync(this.componentsRoot, 0o700)
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
      // C1 (Opus review of #670, 2026-10-05): the fork's new explicit
      // resourceSafeBase check (iDoris-ai/open-design-agent24#7) requires
      // this directory to be owned by the current user with group/other
      // write bits cleared. 0700 (owner rwx only) satisfies that with room
      // to spare — nothing but this process should be able to read or
      // write the on-demand component once it's installed.
      fs.mkdirSync(partialDir, { recursive: true, mode: 0o700 })
      await execFileAsync(this.tarBinary, ['-xzf', tarballPath, '--no-same-owner', '-C', partialDir], { maxBuffer: 64 * 1024 * 1024 })
      verifyComponentContract(partialDir, manifest.version)
      // M4: record the entry files' content hashes now, while they're
      // still known-good (just extracted + contract-verified), so
      // creative-serve-web.ts can spot-check them again right before every
      // launch — see verifyEntryFileIntegrity's own doc comment.
      const entrySha256 = Object.fromEntries(
        ENTRY_FILES_FOR_INTEGRITY_CHECK.map((relative) => [relative, sha256OfFile(path.join(partialDir, relative))]),
      )
      fs.writeFileSync(path.join(partialDir, COMPONENT_MARKER_FILE), JSON.stringify({ sha256: manifest.sha256, entrySha256 }))

      const finalDir = openDesignComponentInstallDir(this.componentsRoot, manifest)
      // M4: an OLD install at this exact path (a re-install / upgrade) was
      // made read-only below the last time install() succeeded — restore
      // write access before removing it. A no-op (throwIfNoEntry guards
      // inside) when there's nothing here yet.
      makeInstalledTreeWritableForRemoval(finalDir)
      fs.rmSync(finalDir, { recursive: true, force: true })
      fs.renameSync(partialDir, finalDir)
      // M4: lock the whole tree read-only now that it's in its final
      // location — nothing (including this process) can change its
      // content again short of deleting and reinstalling it.
      makeInstalledTreeReadOnly(finalDir)
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
    // H2 (review finding, 2026-10-05): a write error while we're `await`ing
    // 'drain' used to hang installOnce() forever — the no-op 'error'
    // listener below swallowed it, and a stream that has errored never
    // emits 'drain'. Aborting the shared controller on a write error means
    // every `events.once(..., { signal: controller.signal })` wait below
    // (and the in-flight fetch) rejects/cancels promptly instead of
    // hanging; the listener itself still has to exist so the 'error' event
    // never surfaces as an unhandled EventEmitter error and crashes the
    // process.
    file.on('error', (error) => {
      if (!controller.signal.aborted) controller.abort(error)
    })
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
        if (!file.write(chunk)) {
          try {
            await once(file, 'drain', { signal: controller.signal })
          } catch (error) {
            // `events.once(..., { signal })` rejects with a generic
            // AbortError ("The operation was aborted"), losing WHY we
            // aborted (e.g. the file's own 'error' listener above). Surface
            // the real reason when we have one.
            throw controller.signal.reason instanceof Error ? controller.signal.reason : error
          }
        }
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
