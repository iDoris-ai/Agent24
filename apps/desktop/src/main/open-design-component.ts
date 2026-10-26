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
import { dataDir, joinLiteral, tryDataDir } from './data-dir'
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
  | 'paused'
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
  /** SD-01: rolling-window transfer rate while `downloading`, in bytes/sec.
   * `0` (not absent) means "no recent progress" — the renderer shows
   * "等待网络" rather than a bogus 0 B/s. */
  bytesPerSecond?: number
  /** SD-01: estimated seconds remaining at the current rate; `null` when
   * the rate is unknown or zero (renderer shows "等待网络" instead of an
   * infinite/garbage ETA). Absent outside `downloading`. */
  etaSeconds?: number | null
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

/** `<data_dir>/components/open-design` (ADR-037 data-dir contract). Not a
 * frozen module-level constant — resolves the data dir fresh every call,
 * matching the constructor's own (pre-existing) behavior of resolving it at
 * construction time rather than at module load. */
export function defaultComponentsRoot(): string {
  return joinLiteral(dataDir(), 'components', 'open-design')
}

/** Like [defaultComponentsRoot] but null when the data dir is unresolvable. */
function tryDefaultComponentsRoot(): string | null {
  const dir = tryDataDir()
  return dir === null ? null : joinLiteral(dir, 'components', 'open-design')
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

/** SD-01: streamed (not `readFileSync`'d whole) because this hashes the
 * downloaded tarball itself — potentially hundreds of MB — and, unlike
 * sha256OfFile's small entry-file use, is also what makes resuming across
 * process restarts correct: the hash is always computed from what's
 * actually on disk now, never from an in-memory accumulator that would
 * only have seen the bytes THIS process wrote. */
async function sha256OfFileStreamed(filePath: string): Promise<string> {
  return new Promise((resolve, reject) => {
    const hash = crypto.createHash('sha256')
    fs.createReadStream(filePath)
      .on('data', (chunk) => hash.update(chunk as Buffer))
      .on('error', reject)
      .on('end', () => resolve(hash.digest('hex')))
  })
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
async function assertSafeTarMembers(tarBinary: string, tarballPath: string, signal: AbortSignal): Promise<void> {
  // M1 (review finding, 2026-10-05): this used to buffer the whole listing
  // via execFileAsync with a fixed maxBuffer. Real-app verification already
  // showed a real open-design-web-standalone/node_modules tree can exceed
  // 1MB of listing text — but a FIXED ceiling (we had raised it to 64MB) is
  // never actually safe against an arbitrarily large (or maliciously
  // padded) tarball; it only moves the failure threshold. spawn + readline
  // processes the listing one line at a time and never holds more than a
  // single line in memory.
  // R2: `signal` lets a cancel/pause kill this own `tar -tzf` child promptly;
  // the resulting non-zero exit surfaces as a plain listing-failed error.
  const child = spawn(tarBinary, ['-tzf', tarballPath], { stdio: ['ignore', 'pipe', 'pipe'], signal })
  child.on('error', () => {})
  // Register the 'exit' listener BEFORE draining stdout below. `tar -tzf`
  // on a small tarball can exit before the readline loop over stdout even
  // finishes draining — 'exit' only fires once per process lifetime, so a
  // listener attached after the loop can miss an event that already fired,
  // and `await`ing it then hangs forever. once() queues immediately and
  // resolves whenever the event lands, so there is no ordering race.
  const exitPromise = once(child, 'exit') as Promise<[number | null, NodeJS.Signals | null]>
  // R2: `events.once()` rejects on 'error', which the signal-driven kill
  // above emits before 'exit' — mark handled now so it's never reported as
  // unhandled; the real rejection still surfaces at `await exitPromise`.
  exitPromise.catch(() => {})
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

// SD-01: resumable download state. Deliberately a DIFFERENT name prefix
// (`.download-`) than the `.partial-` extraction scratch dirs above —
// `cleanupStalePartials` must never sweep these, since (unlike the
// extraction scratch dir) they are meant to survive a quit/crash/pause so a
// later install() can resume them. Keyed by the manifest's sha256, not a
// random token: that's what lets a brand-new installer instance (e.g.
// after an app restart) recognize "this file on disk is a prefix of THIS
// exact asset" without guessing. The sidecar metadata is the actual
// resume-eligibility check — see readMatchingDownloadMeta.
interface DownloadMeta { version: string; url: string; sha256: string; total: number }

function downloadFilePath(componentsRoot: string, manifest: OpenDesignComponentManifest): string {
  return path.join(componentsRoot, `.download-${manifest.sha256}.part`)
}

function downloadMetaPath(componentsRoot: string, manifest: OpenDesignComponentManifest): string {
  return path.join(componentsRoot, `.download-${manifest.sha256}.meta.json`)
}

/** R1: opens with O_NOFOLLOW so a part/meta symlink can never be read or
 * written through — open() itself fails (ELOOP) instead of following it,
 * closing the TOCTOU window a separate lstat-then-open check would leave.
 * `null` means "doesn't exist / is a symlink / isn't a regular file".
 * R2: also O_NONBLOCK — a planted FIFO otherwise blocks this synchronous
 * open() (and the whole main event loop) forever before fstat ever runs;
 * O_NONBLOCK is a no-op for a real regular file, so fstat below still gates. */
function openRegularFileNoFollow(filePath: string, flags: number, mode?: number): number | null {
  let fd: number
  try {
    fd = fs.openSync(filePath, flags | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK, mode)
  } catch {
    return null
  }
  if (!fs.fstatSync(fd).isFile()) {
    fs.closeSync(fd)
    return null
  }
  return fd
}

/** R1: write via a temp file + atomic rename instead of opening metaPath
 * directly — rename() replaces the directory entry (symlink or not)
 * without ever following it, so a pre-planted symlink can't redirect the
 * write outside componentsRoot. */
function writeDownloadMeta(metaPath: string, manifest: OpenDesignComponentManifest): void {
  const payload = JSON.stringify({ version: manifest.version, url: manifest.url, sha256: manifest.sha256, total: manifest.size } satisfies DownloadMeta)
  const tempPath = path.join(path.dirname(metaPath), `.download-meta-${crypto.randomBytes(6).toString('hex')}.tmp`)
  try {
    fs.writeFileSync(tempPath, payload, { flag: 'wx', mode: 0o600 }) // wx = O_CREAT|O_EXCL, never follows/reuses an existing entry; writeFileSync loops over short writes and always closes, even on error.
    fs.renameSync(tempPath, metaPath) // nothing after this can throw, so a renamed-away temp never reaches the rm below.
  } catch (error) {
    if ((error as NodeJS.ErrnoException).syscall !== 'open') fs.rmSync(tempPath, { force: true }) // failed open means the temp isn't ours (EEXIST/EMFILE/...) — never delete it.
    throw error
  }
}

/** Only a file whose sidecar metadata matches EVERY one of version/url/
 * sha256/total against the CURRENT manifest is reused — any mismatch (or a
 * missing/corrupt sidecar, e.g. a pre-SD-01 random `.partial-*` leftover)
 * is discarded and the download restarts from byte 0. This is what keeps
 * resume from ever guessing which asset an unrelated partial belongs to.
 * R1: both candidates open via O_NOFOLLOW — a symlinked part/meta is never
 * stat'd or read through, only ever treated as "not resumable". */
function readMatchingDownloadMeta(filePath: string, metaPath: string, manifest: OpenDesignComponentManifest): number {
  const fileFd = openRegularFileNoFollow(filePath, fs.constants.O_RDONLY)
  if (fileFd == null) return 0
  let size: number
  try {
    size = fs.fstatSync(fileFd).size
  } finally {
    fs.closeSync(fileFd)
  }
  if (size <= 0 || size > manifest.size) return 0
  const metaFd = openRegularFileNoFollow(metaPath, fs.constants.O_RDONLY)
  if (metaFd == null) return 0
  try {
    const meta = JSON.parse(fs.readFileSync(metaFd, 'utf8')) as Partial<DownloadMeta>
    if (meta.version === manifest.version && meta.url === manifest.url && meta.sha256 === manifest.sha256 && meta.total === manifest.size) {
      return size
    }
  } catch {
    // missing/corrupt sidecar — fall through to "not resumable"
  } finally {
    fs.closeSync(metaFd)
  }
  return 0
}

function discardDownloadFile(filePath: string, metaPath: string): void {
  fs.rmSync(filePath, { force: true })
  fs.rmSync(metaPath, { force: true })
}

/** Thrown when the server's response to a Range request doesn't agree with
 * what we asked for (206 with a mismatched Content-Range, or 416 while we
 * are NOT already holding the full file) — signals installOnce to discard
 * the local prefix and retry exactly once from byte 0, never looping. */
class RangeResetRequiredError extends Error {}

/** R2: pause()/abort() only flip `controller.signal` — installOnce checks
 * this after every await and once more before the final publish, so a
 * cancel anywhere in verify/extract/publish is honored instead of racing a
 * synchronous tail to `installed`. */
function throwIfAborted(controller: AbortController): void {
  if (controller.signal.aborted) {
    throw controller.signal.reason instanceof Error ? controller.signal.reason : new Error('Open Design component install was aborted')
  }
}

/** Parses a `Content-Range: bytes <start>-<end>/<total>` response header.
 * Returns `null` for anything that doesn't parse as that exact shape, isn't
 * a safe integer, or has a structurally invalid span (end before start, or
 * past total) — callers treat an unparsable/invalid header the same as a
 * mismatched one. R3: the caller separately checks `end === total - 1`,
 * since this installer only ever sends open-ended `bytes=N-` requests. */
function parseContentRange(header: string | null): { start: number; end: number; total: number } | null {
  const match = header?.match(/^bytes (\d+)-(\d+)\/(\d+)$/)
  if (!match) return null
  const start = Number(match[1])
  const end = Number(match[2])
  const total = Number(match[3])
  if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || !Number.isSafeInteger(total)) return null
  if (start < 0 || end < start || total <= 0 || end >= total) return null
  return { start, end, total }
}

const SPEED_WINDOW_MS = 3_000
/** R5: no new sample within this long means "no recent progress" (shown as
 * "等待网络"), not the last real speed repeated forever. */
const SPEED_IDLE_MS = 3_000

/** Rolling-window speed/ETA estimator (SD-01) — no stats library, just a
 * capped FIFO of recent (time, bytesOnDisk) samples. Returns `bytesPerSecond:
 * 0, etaSeconds: null` ("等待网络") until there are at least two samples
 * spanning a non-zero amount of time. */
class SpeedEstimator {
  private samples: Array<{ t: number; bytes: number }> = []
  private lastReading: { bytesPerSecond: number; etaSeconds: number | null } = { bytesPerSecond: 0, etaSeconds: null }

  /** `remaining` is the number of bytes still left to transfer, used only to
   * turn the measured rate into an ETA — the estimator itself tracks rate. */
  sample(bytes: number, remaining: number): { bytesPerSecond: number; etaSeconds: number | null } {
    const now = Date.now()
    this.samples.push({ t: now, bytes })
    while (this.samples.length > 1 && now - this.samples[0]!.t > SPEED_WINDOW_MS) this.samples.shift()
    const oldest = this.samples[0]!
    const elapsedMs = now - oldest.t
    const bytesPerSecond = elapsedMs > 0 ? ((bytes - oldest.bytes) / elapsedMs) * 1_000 : 0
    this.lastReading = bytesPerSecond > 0 ? { bytesPerSecond, etaSeconds: remaining / bytesPerSecond } : { bytesPerSecond: 0, etaSeconds: null }
    return this.lastReading
  }

  /** R5: queried on every status read instead of trusting the stored
   * reading — past SPEED_IDLE_MS since the last sample, reports "no recent
   * progress" like the no-samples-yet case, instead of a stale number. */
  currentReading(now: number): { bytesPerSecond: number; etaSeconds: number | null } {
    const latest = this.samples[this.samples.length - 1]
    if (!latest || now - latest.t > SPEED_IDLE_MS) return { bytesPerSecond: 0, etaSeconds: null }
    return this.lastReading
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
  /** SD-01: set by `pause()` right before aborting, so installOnce's catch
   * block can tell "the user asked to pause" apart from every other abort
   * reason (quit, idle-timeout, a real network error) — only the former
   * reports `paused` (shows "继续下载"); every other reason reports
   * `failed` (shows "重试"). Both paths now preserve the on-disk prefix. */
  private pauseRequested = false
  private readonly listeners = new Set<(status: OpenDesignComponentStatus) => void>()
  /** R5: owned by the in-flight download(); status() reads it live. */
  private speedEstimator: SpeedEstimator | null = null

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
    // SD-01: a quit/pause/network-interruption from a PREVIOUS process can
    // have left a resumable prefix on disk — surface it as `paused` (with
    // its saved byte count) rather than `not-installed`, so re-opening the
    // page or switching tabs after a restart shows "已保存 X / Y MiB" +
    // "继续下载" instead of silently forgetting the progress.
    const resumable = readMatchingDownloadMeta(
      downloadFilePath(this.componentsRoot, this.manifest),
      downloadMetaPath(this.componentsRoot, this.manifest),
      this.manifest,
    )
    if (resumable > 0 && resumable < this.manifest.size) return { state: 'paused', received: resumable, total: this.manifest.size }
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
    // R5: recompute live rather than returning a stored, possibly-stale reading.
    if (this.current.state === 'downloading' && this.speedEstimator) {
      const { bytesPerSecond, etaSeconds } = this.speedEstimator.currentReading(Date.now())
      return { ...this.current, bytesPerSecond, etaSeconds }
    }
    return { ...this.current }
  }

  onProgress(callback: (status: OpenDesignComponentStatus) => void): () => void {
    this.listeners.add(callback)
    return () => this.listeners.delete(callback)
  }

  private setStatus(status: OpenDesignComponentStatus): void {
    this.current = status
    // R4: a listener that synchronously re-enters install() used to see
    // `this.installing` still unset (install() assigns it right after this
    // call returns) and start a second job. Deferring to a microtask means
    // the job promise is always registered first.
    queueMicrotask(() => {
      for (const listener of this.listeners) listener(this.status())
    })
  }

  /** M5: lets the host cancel an in-flight install (e.g. main.ts's
   * `before-quit`) without leaving a partial dir or a dangling connection.
   * A no-op if nothing is in flight. */
  abort(reason = 'Open Design component install was aborted'): void {
    this.activeAbort?.abort(new Error(reason))
  }

  /** SD-01: user-facing pause — unlike `abort()` (quit path), this reports
   * `paused` rather than `failed`. A no-op unless a download is actually in
   * flight. The already-written prefix is kept on disk either way; a later
   * `install()` call (the renderer's existing "继续下载"/"下载" button)
   * resumes it via HTTP Range. */
  pause(reason = 'Open Design component download paused by user'): void {
    if (this.status().state !== 'downloading') return
    this.pauseRequested = true
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

  private async installOnce(manifest: OpenDesignComponentManifest, alreadyReset = false): Promise<OpenDesignComponentStatus> {
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
    if (this.componentsRoot === tryDefaultComponentsRoot()) {
      const componentsRootParent = path.dirname(this.componentsRoot)
      fs.mkdirSync(componentsRootParent, { recursive: true, mode: 0o700 })
      fs.chmodSync(componentsRootParent, 0o700)
    }
    fs.mkdirSync(this.componentsRoot, { recursive: true, mode: 0o700 })
    fs.chmodSync(this.componentsRoot, 0o700)
    cleanupStalePartials(this.componentsRoot)
    const token = crypto.randomBytes(6).toString('hex')
    const partialDir = path.join(this.componentsRoot, `.partial-${token}`)
    // SD-01: the download itself now lands in a deterministic, sha256-keyed
    // file (NOT this random partialDir) so it survives across install()
    // calls — including a fresh OpenDesignComponentInstaller after a quit —
    // and can be resumed via Range instead of restarted from byte 0.
    const tarballPath = downloadFilePath(this.componentsRoot, manifest)
    const metaPath = downloadMetaPath(this.componentsRoot, manifest)
    const controller = new AbortController()
    this.activeAbort = controller
    // SD-01: once the full, hash-verified bytes are in hand, a failure
    // during verify/extract (unsafe tar member, bad contract, a corrupt
    // archive) is NOT a network interruption — pause() is also a no-op past
    // this point (it only acts while `downloading`; see pause()'s own
    // guard). Retrying would just re-run the same doomed verification
    // against the same bytes, so the download file is discarded rather
    // than kept around as a false "resumable" match.
    let downloadComplete = false

    try {
      // F1 (external review #967): moved inside the try — a metadata ENOSPC here now reports `failed` like any other step below.
      const resumeFromBytes = readMatchingDownloadMeta(tarballPath, metaPath, manifest)
      if (resumeFromBytes === 0) discardDownloadFile(tarballPath, metaPath)
      writeDownloadMeta(metaPath, manifest)
      this.setStatus({ state: 'downloading', received: resumeFromBytes, total: manifest.size })
      await this.download(manifest, tarballPath, metaPath, controller, resumeFromBytes)
      downloadComplete = true
      // R2: the no-network "already have every byte" path never touches
      // the signal itself — check explicitly now that a retry is no longer free.
      throwIfAborted(controller)

      this.setStatus({ state: 'verifying' })
      await assertSafeTarMembers(this.tarBinary, tarballPath, controller.signal)
      throwIfAborted(controller)
      // C1 (Opus review of #670, 2026-10-05): the fork's new explicit
      // resourceSafeBase check (iDoris-ai/open-design-agent24#7) requires
      // this directory to be owned by the current user with group/other
      // write bits cleared. 0700 (owner rwx only) satisfies that with room
      // to spare — nothing but this process should be able to read or
      // write the on-demand component once it's installed.
      fs.mkdirSync(partialDir, { recursive: true, mode: 0o700 })
      await execFileAsync(this.tarBinary, ['-xzf', tarballPath, '--no-same-owner', '-C', partialDir], {
        maxBuffer: 64 * 1024 * 1024,
        signal: controller.signal,
      })
      throwIfAborted(controller)
      verifyComponentContract(partialDir, manifest.version)
      // M4: record the entry files' content hashes now, while they're
      // still known-good (just extracted + contract-verified), so
      // creative-serve-web.ts can spot-check them again right before every
      // launch — see verifyEntryFileIntegrity's own doc comment.
      const entrySha256 = Object.fromEntries(
        ENTRY_FILES_FOR_INTEGRITY_CHECK.map((relative) => [relative, sha256OfFile(path.join(partialDir, relative))]),
      )
      fs.writeFileSync(path.join(partialDir, COMPONENT_MARKER_FILE), JSON.stringify({ sha256: manifest.sha256, entrySha256 }))
      // R2: last gate before touching the OLD install — a cancel here must
      // leave it exactly as it was, not mid-replace.
      throwIfAborted(controller)

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
      // The downloaded tarball is only needed up to extraction — once the
      // final tree is published, drop it (and its resume sidecar) rather
      // than leaving it to be rediscovered as a false "resumable" match.
      discardDownloadFile(tarballPath, metaPath)
      this.setStatus({ state: 'installed', dir: finalDir })
      return this.status()
    } catch (error) {
      fs.rmSync(partialDir, { recursive: true, force: true })
      // SD-01: the server disagreed with our Range request in a way
      // download() couldn't safely recover from inline (it already
      // discarded the now-untrustworthy local prefix) — retry exactly once
      // from byte 0, never recursing further.
      if (error instanceof RangeResetRequiredError && !alreadyReset) {
        return this.installOnce(manifest, true)
      }
      // SD-01/R2: pause() preserves the on-disk prefix unconditionally —
      // including when downloadComplete is already true (full prefix on
      // disk, or verifying/extracting) — so this must run BEFORE the
      // downloadComplete discard below.
      if (this.pauseRequested) {
        this.pauseRequested = false
        const onDisk = fs.statSync(tarballPath, { throwIfNoEntry: false })?.size ?? 0
        this.setStatus({ state: 'paused', received: onDisk, total: manifest.size })
        return this.status()
      }
      // A genuine failure (not a pause) past the point the full bytes were
      // already verified — retrying would just re-run the same doomed
      // verification against the same bytes, so the download file is
      // discarded rather than kept around as a false "resumable" match.
      if (downloadComplete) discardDownloadFile(tarballPath, metaPath)
      const message = error instanceof Error ? error.message : String(error)
      this.setStatus({ state: 'failed', error: message })
      return this.status()
    } finally {
      if (this.activeAbort === controller) this.activeAbort = null
    }
  }

  /** L3: follows redirects manually, re-checking HTTPS + the host allowlist
   * at every hop — fetch's automatic `redirect: 'follow'` would trust
   * wherever the server points next without either check. */
  private async fetchWithAllowedRedirects(initialUrl: string, signal: AbortSignal, headers?: Record<string, string>): Promise<Response> {
    let current = initialUrl
    for (let hop = 0; hop < MAX_REDIRECT_HOPS; hop++) {
      const parsed = new URL(current)
      if (parsed.protocol !== 'https:') {
        throw new Error(`Open Design component download must use HTTPS at every hop: ${current}`)
      }
      if (!this.allowedHosts.has(parsed.hostname)) {
        throw new Error(`Open Design component download host is not allow-listed: ${parsed.hostname}`)
      }
      const response = await this.fetchFn(current, { signal, redirect: 'manual', headers })
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

  private async download(
    manifest: OpenDesignComponentManifest,
    destination: string,
    metaPath: string,
    controller: AbortController,
    resumeFromBytes: number,
  ): Promise<void> {
    // SD-01: the sidecar metadata already matched this exact manifest and
    // the file on disk already has every byte — nothing left to transfer
    // (e.g. a previous run completed the download but crashed before
    // extraction). Skip the network entirely and just re-verify.
    if (resumeFromBytes === manifest.size) {
      const digest = await sha256OfFileStreamed(destination)
      if (digest !== manifest.sha256) {
        discardDownloadFile(destination, metaPath)
        throw new Error(`Open Design component sha256 mismatch: expected ${manifest.sha256}, got ${digest}`)
      }
      return
    }

    let idleTimer: NodeJS.Timeout | undefined
    const resetIdleTimer = (): void => {
      clearTimeout(idleTimer)
      idleTimer = setTimeout(
        () => controller.abort(new Error(`Open Design component download idle-timed-out (${this.idleTimeoutMs}ms without data)`)),
        this.idleTimeoutMs,
      )
      idleTimer.unref?.()
    }

    const sentRange = resumeFromBytes > 0
    resetIdleTimer()
    let response: Response
    try {
      response = await this.fetchWithAllowedRedirects(
        manifest.url, controller.signal, sentRange ? { Range: `bytes=${resumeFromBytes}-` } : undefined,
      )
    } finally {
      clearTimeout(idleTimer)
    }

    // SD-01: a Range request can come back three ways — 206 (resume
    // accepted: must re-check the server actually agrees on WHERE it's
    // resuming from and the total size), 200 (server ignored Range and is
    // sending the whole thing again: must reset the local file to byte 0,
    // never append a second copy onto it), or 416 (server rejects the
    // range outright: our local prefix can no longer be trusted against
    // this server/asset — discard and retry once from byte 0). Any other
    // status is a plain HTTP failure, same as a non-resumed request.
    let onDiskReceived = 0
    let appendMode = false
    if (sentRange && response.status === 416) {
      discardDownloadFile(destination, metaPath)
      throw new RangeResetRequiredError('Open Design component download: server rejected the resume Range request')
    }
    if (sentRange && response.status === 206) {
      const range = parseContentRange(response.headers.get('content-range'))
      // R3: a well-formed response's end must equal total-1 (we always send
      // an open-ended `bytes=N-`) — previously unchecked, so e.g. `bytes
      // 3-1/total` was accepted as long as start/total happened to match.
      if (!range || range.start !== resumeFromBytes || range.total !== manifest.size || range.end !== range.total - 1) {
        discardDownloadFile(destination, metaPath)
        throw new RangeResetRequiredError('Open Design component download: server returned an unexpected Content-Range for the resume request')
      }
      onDiskReceived = resumeFromBytes
      appendMode = true
    } else if (!response.ok) {
      throw new Error(`Open Design component download failed: HTTP ${response.status}`)
    }
    if (!response.body) throw new Error('Open Design component download returned no body')

    let lastEmit = 0
    this.speedEstimator = new SpeedEstimator()
    // R5: status() recomputes live, but nothing PUSHES while no chunk
    // arrives — this job-owned timer re-emits once per idle onset so
    // onProgress listeners see "等待网络" without polling status().
    let idlePushedIdle = false
    // R3 (independent review, 2026-10-15): only declared here — created
    // below, inside the try/finally that clears it, once the write-open
    // has succeeded. Created this early before, it used to outlive an
    // `openRegularFileNoFollow` rejection and overwrite the resulting
    // `failed` status back to `downloading` a second later.
    let idlePushTimer: NodeJS.Timeout | undefined
    // R1: hand createWriteStream an already-open, O_NOFOLLOW-verified fd
    // instead of a path — a symlink planted at `destination` since
    // readMatchingDownloadMeta's check can no longer be opened at all.
    const fileFd = openRegularFileNoFollow(
      destination,
      appendMode ? fs.constants.O_WRONLY | fs.constants.O_APPEND : fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_TRUNC,
      0o600,
    )
    if (fileFd == null) throw new Error('Open Design component refused to write the download through a non-regular file')
    const file = fs.createWriteStream(destination, { fd: fileFd })
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
      idlePushTimer = setInterval(() => {
        const reading = this.speedEstimator!.currentReading(Date.now())
        if (reading.bytesPerSecond > 0) idlePushedIdle = false
        else if (!idlePushedIdle) {
          idlePushedIdle = true
          this.setStatus({ state: 'downloading', received: onDiskReceived, total: manifest.size, ...reading })
        }
      }, 1_000)
      idlePushTimer.unref?.()
      resetIdleTimer()
      for await (const chunk of nodeStream as AsyncIterable<Buffer>) {
        resetIdleTimer()
        onDiskReceived += chunk.length
        if (onDiskReceived > manifest.size) {
          throw new Error('Open Design component download exceeded the manifest-declared size')
        }
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
        if (onDiskReceived === manifest.size || now - lastEmit >= PROGRESS_THROTTLE_MS) {
          const { bytesPerSecond, etaSeconds } = this.speedEstimator.sample(onDiskReceived, manifest.size - onDiskReceived)
          this.setStatus({ state: 'downloading', received: onDiskReceived, total: manifest.size, bytesPerSecond, etaSeconds })
          lastEmit = now
        }
      }
      await new Promise<void>((resolve, reject) => {
        file.end((error: unknown) => (error ? reject(error) : resolve()))
      })
    } catch (error) {
      // Wait for the fd to actually close before anything else touches
      // this path — fs.WriteStream.destroy() can leave an in-flight
      // open/write that still touches the path after we return.
      await new Promise<void>((resolve) => file.close(() => resolve()))
      void response.body?.cancel?.().catch(() => {})
      // Overflow means THIS attempt's bytes can no longer be trusted as a
      // valid prefix of the declared size — discard rather than "resume"
      // garbage next time. Every other error (pause/quit-abort/idle-timeout/
      // network/write-stream) leaves the already-flushed prefix in place.
      if (onDiskReceived > manifest.size) discardDownloadFile(destination, metaPath)
      throw error
    } finally {
      clearTimeout(idleTimer)
      clearInterval(idlePushTimer)
    }

    if (onDiskReceived !== manifest.size) {
      throw new Error(`Open Design component download size mismatch: expected ${manifest.size}, got ${onDiskReceived}`)
    }
    const digest = await sha256OfFileStreamed(destination)
    if (digest !== manifest.sha256) {
      discardDownloadFile(destination, metaPath)
      throw new Error(`Open Design component sha256 mismatch: expected ${manifest.sha256}, got ${digest}`)
    }
  }
}
