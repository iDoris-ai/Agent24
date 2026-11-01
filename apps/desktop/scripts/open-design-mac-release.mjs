// macOS-release helpers for the Open Design on-demand component (used by
// release-mac.sh). Kept as a node module so the logic that decides whether a
// release is correct (tag -> version, sha pin, which files get signed) is unit
// tested instead of living only in shell.
//
//   node open-design-mac-release.mjs version <release-tag>
//   node open-design-mac-release.mjs check-source <open-design-checkout>
//   node open-design-mac-release.mjs sign <staged-dir> <identity> <entitlements.plist>
//
// Why the component's native code must be signed: the component is downloaded
// at runtime and its daemon runs INSIDE the app's own binary
// (ELECTRON_RUN_AS_NODE, see CreativeServeWeb.startPackagedHeadless). The app is
// Developer ID signed with Hardened Runtime and WITHOUT
// com.apple.security.cs.disable-library-validation, so dlopen() of every
// .node/.dylib is only allowed if it carries the same Team ID. Standalone
// executables the daemon spawns are signed too (consistent, and Hardened Runtime
// rejects nothing for them, but an unsigned arm64 binary is not worth the risk).

import { execFile, execFileSync } from 'node:child_process'
import crypto from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { promisify } from 'node:util'

import { OPEN_DESIGN_PIN_VERSION, OPEN_DESIGN_SHA } from './pack-open-design-component.mjs'

export const OD_SOURCE_STAMP_FILE = '.agent24-od-source.json'
export const OPEN_DESIGN_REPOSITORY = 'iDoris-ai/Agent24'
export const EXPECTED_TEAM_ID = 'HYAP444FN9'

/**
 * The only stage-tree path that may carry allow-jit and
 * allow-unsigned-executable-memory. This is opencode (bun / JavaScriptCore)
 * inside the packed component, not every Mach-O whose basename is `opencode`.
 * Signing and `verify-signed` both fail unless this relative path occurs
 * exactly once. ffmpeg, esbuild, spawn-helper, and vela stay on Hardened
 * Runtime with no entitlements.
 */
export const JIT_ENTITLEMENT_RELATIVE_PATH = 'open-design/bin/libexec/opencode/opencode'

const execFileAsync = promisify(execFile)

export function desktopPackageVersion() {
  const packageJson = fileURLToPath(new URL('../package.json', import.meta.url))
  return JSON.parse(fs.readFileSync(packageJson, 'utf8')).version
}

/** `v0.6.0-preview.20261028.10` -> `0.6.0-preview.20261028.10`. The result is
 * baked into the manifest URL `releases/download/v<version>/...`, so it MUST be
 * the real GitHub release tag minus the `v` — never a guessed tag. It must
 * also start with `apps/desktop/package.json`'s version (`v0.6.0` or
 * `v0.6.0-…` when that version is 0.6.0). */
export function versionFromReleaseTag(tag, packageVersion = desktopPackageVersion()) {
  if (typeof tag !== 'string' || tag.trim() === '') {
    throw new Error(
      'A24_RELEASE_TAG is required (the GitHub release tag this build will be published under, e.g. v0.6.0-preview.20261028.10); ' +
      "refusing to guess from package.json because the manifest's download URL must match a tag that really exists",
    )
  }
  const match = /^v(\d+\.\d+\.\d+(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?)$/.exec(tag.trim())
  if (!match) throw new Error(`A24_RELEASE_TAG must look like v<semver> (e.g. v0.6.0-preview.20261028.10); got: ${tag}`)
  const version = match[1]
  if (packageVersion != null && packageVersion !== '' && version !== packageVersion && !version.startsWith(`${packageVersion}-`)) {
    throw new Error(
      `A24_RELEASE_TAG ${tag.trim()} must start with apps/desktop package.json version ${packageVersion} (v${packageVersion} or v${packageVersion}-…); got ${version}`,
    )
  }
  return version
}

export function assertPinnedSha(actualSha, expected = OPEN_DESIGN_SHA) {
  if (String(actualSha).trim() !== expected) {
    throw new Error(`Open Design checkout must be exact ${expected}; found ${String(actualSha).trim() || '(empty)'}`)
  }
}

export function checkOpenDesignSource(dir) {
  if (!dir || !path.isAbsolute(dir)) throw new Error('Open Design checkout path must be absolute')
  const status = execFileSync('git', ['-C', dir, 'status', '--porcelain'], { encoding: 'utf8' })
  if (status.trim() !== '') {
    throw new Error(`Open Design checkout has uncommitted changes (git status --porcelain must be empty):\n${status.trim()}`)
  }
  const sha = execFileSync('git', ['-C', dir, 'rev-parse', 'HEAD'], { encoding: 'utf8' })
  assertPinnedSha(sha)
  return sha.trim()
}

const TREE_DIGEST_CHUNK = 1024 * 1024

/** sha256 of one file, read in fixed-size chunks so a large resource is never
 * held in memory all at once. */
function sha256File(filePath) {
  const hash = crypto.createHash('sha256')
  const fd = fs.openSync(filePath, 'r')
  const buf = Buffer.allocUnsafe(TREE_DIGEST_CHUNK)
  try {
    while (true) {
      const n = fs.readSync(fd, buf, 0, buf.length, null)
      if (n === 0) break
      hash.update(n === buf.length ? buf : buf.subarray(0, n))
    }
  } finally {
    fs.closeSync(fd)
  }
  return hash.digest('hex')
}

/**
 * Digest of a prepared Resources tree. Entries are sorted by relative path.
 * Each regular file contributes a streaming sha256 and its permission bits;
 * each symlink contributes a sha256 of its target text plus that target;
 * each directory contributes `{path, kind:"dir", mode}` only.
 * The resources root is included (relative path `"."`) so its mode is part of
 * the digest: chmod on the Resources directory itself must not reuse a stamp.
 * The stamp file is excluded (it records this digest). Directory symlinks are
 * not followed, so a link cannot pull outside files into the hash.
 * FIFO, socket, and device nodes are rejected. Folding them into one `other`
 * kind dropped the file type and let different special files share a digest.
 */
export function openDesignResourceTreeDigest(resourcesRoot) {
  const entries = []
  const recordDirectory = (full) => {
    const rel = path.relative(resourcesRoot, full).split(path.sep).join('/')
    entries.push({
      path: rel === '' ? '.' : rel,
      kind: 'dir',
      mode: fs.lstatSync(full).mode & 0o7777,
    })
  }
  const visit = (directory) => {
    recordDirectory(directory)
    for (const name of fs.readdirSync(directory)) {
      const full = path.join(directory, name)
      const rel = path.relative(resourcesRoot, full).split(path.sep).join('/')
      if (rel === OD_SOURCE_STAMP_FILE) continue
      const st = fs.lstatSync(full)
      const mode = st.mode & 0o7777
      if (st.isSymbolicLink()) {
        const link = fs.readlinkSync(full)
        entries.push({
          path: rel,
          kind: 'symlink',
          mode,
          sha256: crypto.createHash('sha256').update(link).digest('hex'),
          link,
        })
        continue
      }
      if (st.isDirectory()) {
        visit(full)
        continue
      }
      if (st.isFile()) {
        entries.push({ path: rel, kind: 'file', mode, sha256: sha256File(full), link: null })
        continue
      }
      throw new Error(`Open Design Resources tree contains an unsupported file type at ${rel} (only regular files, directories, and symlinks are allowed)`)
    }
  }
  visit(resourcesRoot)
  entries.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0))
  const hash = crypto.createHash('sha256')
  for (const entry of entries) {
    const recorded = entry.kind === 'dir'
      ? { path: entry.path, kind: entry.kind, mode: entry.mode }
      : { path: entry.path, kind: entry.kind, mode: entry.mode, sha256: entry.sha256, link: entry.link }
    hash.update(JSON.stringify(recorded))
    hash.update('\n')
  }
  return hash.digest('hex')
}

export function writeOpenDesignSourceStamp(resourcesRoot, { sha, version }) {
  if (!sha || !version) throw new Error('Open Design source stamp requires sha and version')
  if (!resourcesRoot || !path.isAbsolute(resourcesRoot)) throw new Error('Open Design Resources path must be absolute')
  const tree = openDesignResourceTreeDigest(resourcesRoot)
  const file = path.join(resourcesRoot, OD_SOURCE_STAMP_FILE)
  fs.writeFileSync(file, `${JSON.stringify({ sha, version, tree })}\n`)
  return file
}

/** Reusing a prepared Resources directory skips the source checkout. The
 * stamp must name the pinned sha and version, and its tree digest must still
 * match the files, directory entries (including the root's mode), and
 * symlink targets on disk. */
export function assertOpenDesignResourcesStamp(resourcesRoot, expectedSha = OPEN_DESIGN_SHA) {
  if (!resourcesRoot || !path.isAbsolute(resourcesRoot)) throw new Error('Open Design Resources path must be absolute')
  const file = path.join(resourcesRoot, OD_SOURCE_STAMP_FILE)
  if (!fs.existsSync(file)) {
    throw new Error(`Open Design Resources stamp is missing: ${file} (refusing to reuse a tree whose source sha was not recorded)`)
  }
  let parsed
  try {
    parsed = JSON.parse(fs.readFileSync(file, 'utf8'))
  } catch {
    throw new Error(`Open Design Resources stamp is not JSON: ${file}`)
  }
  if (parsed.sha !== expectedSha) {
    throw new Error(`Open Design Resources stamp sha ${parsed.sha ?? '(missing)'} does not match pin ${expectedSha}`)
  }
  if (parsed.version !== OPEN_DESIGN_PIN_VERSION) {
    throw new Error(`Open Design Resources stamp version ${parsed.version ?? '(missing)'} does not match pin ${OPEN_DESIGN_PIN_VERSION}`)
  }
  const tree = openDesignResourceTreeDigest(resourcesRoot)
  if (parsed.tree !== tree) {
    throw new Error('Open Design Resources tree digest does not match the stamp (a file, its mode, a directory, or a symlink target changed after prepare)')
  }
  return parsed
}

/** "Developer ID Application: Name (HYAP444FN9)" -> "HYAP444FN9"; null for ad-hoc "-". */
export function teamIdFromIdentity(identity) {
  const match = /\(([A-Z0-9]{10})\)\s*$/.exec(identity ?? '')
  return match ? match[1] : null
}

/** Designated requirement that pins the Developer ID team. Ad-hoc signatures
 * do not satisfy it, including a binary whose path or identifier merely
 * contains the text `TeamIdentifier=<TEAM>`. */
export function teamIdRequirement(teamId) {
  if (!/^[A-Z0-9]{10}$/.test(teamId ?? '')) {
    throw new Error(`refusing to build a codesign requirement for team id: ${teamId}`)
  }
  return `anchor apple generic and certificate leaf[subject.OU] = "${teamId}"`
}

export function posixRelative(root, file) {
  return path.relative(root, file).split(path.sep).join('/')
}

/** `codesign -dv` prints one `flags=0xNNNN(...runtime...)` token on the
 * CodeDirectory line. Match that token, not a substring anywhere in the text. */
export function codesignOutputHasRuntimeFlag(text) {
  return String(text).split(/\r?\n/).some((line) => /(?:^|\s)flags=0x[0-9a-fA-F]+\([^)]*\bruntime\b/.test(line))
}

export function entitlementsTextHasJit(text) {
  return /allow-jit/.test(text) || /allow-unsigned-executable-memory/.test(text)
}

export function assertExactlyOneJitPath(root, files, expected = JIT_ENTITLEMENT_RELATIVE_PATH) {
  const hits = files.filter((item) => posixRelative(root, item.file) === expected)
  if (hits.length !== 1) {
    throw new Error(`expected exactly one Mach-O at ${expected} for JIT entitlements, found ${hits.length}`)
  }
  return hits[0]
}

function commandFailureDetail(error) {
  const stderr = error && error.stderr != null ? String(error.stderr).trim() : ''
  if (stderr) return stderr
  return error instanceof Error ? error.message : String(error)
}

const THIN_MAGICS = new Set(['cffaedfe', 'cefaedfe', 'feedfacf', 'feedface'])
const FAT_MAGICS = new Set(['cafebabe', 'cafebabf', 'bebafeca', 'bfbafeca'])

function readMagic(file) {
  const fd = fs.openSync(file, 'r')
  try {
    const buf = Buffer.alloc(4)
    return fs.readSync(fd, buf, 0, 4, 0) === 4 ? buf.toString('hex') : ''
  } finally {
    fs.closeSync(fd)
  }
}

/** Returns `{ kind: 'executable' | 'library' }` for a Mach-O file, else null.
 * Fat magic collides with Java class files, so those are confirmed with file(1). */
export function classifyMachO(file) {
  const magic = readMagic(file)
  const thin = THIN_MAGICS.has(magic)
  if (!thin && !FAT_MAGICS.has(magic)) return null
  const description = execFileSync('file', ['-b', file], { encoding: 'utf8' })
  if (!/Mach-O/.test(description)) return null
  // bundle (.node) and dynamically linked shared library (.dylib) are loaded by
  // dlopen; only a pure executable can be spawned directly.
  return { kind: /executable/.test(description) && !/bundle|shared library/.test(description) ? 'executable' : 'library' }
}

export function findMachOFiles(root) {
  const found = []
  const visit = (directory) => {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const full = path.join(directory, entry.name)
      if (entry.isDirectory()) visit(full)
      else if (entry.isFile()) {
        const classified = classifyMachO(full)
        if (classified) found.push({ file: full, ...classified })
      }
    }
  }
  visit(root)
  return found.sort((a, b) => a.file.localeCompare(b.file))
}

/** codesign argv for one Mach-O. Libraries (`.dylib`, `.node`) never get an
 * entitlements plist. Executables get `--options runtime`; only
 * {@link JIT_ENTITLEMENT_RELATIVE_PATH} also gets the JIT entitlements plist.
 * `relativePath` is the path inside the stage tree (`/` separators). */
export function machOSigningArgs({ file, kind, identity, entitlements, timestamp = true, relativePath }) {
  const adhoc = identity === '-'
  const args = ['--force', '--sign', identity]
  if (kind === 'executable' || !adhoc) args.push('--options', 'runtime')
  if (timestamp && !adhoc) args.push('--timestamp')
  if (kind === 'executable' && entitlements && relativePath === JIT_ENTITLEMENT_RELATIVE_PATH) {
    args.push('--entitlements', entitlements)
  }
  args.push(file)
  return args
}

/** `--verify --strict`, plus `-R` when a team requirement applies.
 * Ad-hoc tests pass no team id (skip `-R`) or inject `requirement`. */
export function codesignVerifyArgs(file, { teamId, requirement } = {}) {
  const args = ['--verify', '--strict']
  const resolved = requirement === undefined ? (teamId ? teamIdRequirement(teamId) : null) : requirement
  if (resolved) args.push('-R', resolved)
  args.push(file)
  return args
}

/**
 * Signs every Mach-O under `root` (Developer ID, Hardened Runtime, secure
 * timestamp). Each file is then checked with
 * `codesign --verify --strict -R='anchor apple generic and certificate leaf[subject.OU] = "<TEAM>"'`.
 * `identity: '-'` (ad-hoc) derives no Team ID, so that `-R` step is skipped
 * unless `expectedTeamId` or `requirement` is injected. When a requirement is
 * checked, flags must include `runtime`, and only
 * {@link JIT_ENTITLEMENT_RELATIVE_PATH} may carry JIT entitlements.
 * `requireJitMatch` (default true) fails unless that path occurs exactly once.
 * `runCommand` and `files` are test seams (no `codesign` / no Mach-O).
 */
export async function signOpenDesignTree({
  root, identity, entitlements, timestamp = true, log = () => {},
  runCommand = execFileAsync, files, expectedTeamId, requirement, requireJitMatch = true,
}) {
  if (!root || !path.isAbsolute(root)) throw new Error('root must be an absolute path')
  if (!identity) throw new Error('identity is required')
  const derivedTeamId = teamIdFromIdentity(identity)
  const teamId = expectedTeamId === undefined ? derivedTeamId : expectedTeamId
  const adhoc = identity === '-'
  if (!adhoc && !teamId) throw new Error(`cannot derive a Team ID from signing identity: ${identity}`)
  if (entitlements && !fs.existsSync(entitlements)) throw new Error(`entitlements file is missing: ${entitlements}`)

  const discovered = files ?? findMachOFiles(root)
  if (discovered.length === 0) throw new Error(`no Mach-O files found under ${root} — refusing to continue (expected native modules)`)
  if (requireJitMatch) assertExactlyOneJitPath(root, discovered)
  const resolvedRequirement = requirement === undefined
    ? (teamId ? teamIdRequirement(teamId) : null)
    : requirement
  for (const { file, kind } of discovered) {
    const relativePath = posixRelative(root, file)
    await runCommand('codesign', machOSigningArgs({ file, kind, identity, entitlements, timestamp, relativePath }))
    try {
      await runCommand('codesign', codesignVerifyArgs(file, { requirement: resolvedRequirement ?? null }))
    } catch (error) {
      throw new Error(`${file} failed codesign --verify --strict -R for Team ID ${teamId ?? '(none)'}: ${commandFailureDetail(error)}`)
    }
    if (resolvedRequirement) {
      await assertRuntimeAndJitEntitlements({ file, root, runCommand })
    }
    log(`signed (${kind}) ${relativePath}`)
  }
  return discovered
}

async function assertRuntimeAndJitEntitlements({ file, root, runCommand }) {
  const described = await runCommand('codesign', ['-dv', file])
  const describedText = `${described?.stderr ?? ''}\n${described?.stdout ?? ''}`
  if (!codesignOutputHasRuntimeFlag(describedText)) {
    throw new Error(`${file} is missing the hardened runtime flag`)
  }
  const entitlements = await runCommand('codesign', ['-d', '--entitlements', ':-', file])
  const entitlementsText = `${entitlements?.stdout ?? ''}\n${entitlements?.stderr ?? ''}`
  if (posixRelative(root, file) !== JIT_ENTITLEMENT_RELATIVE_PATH && entitlementsTextHasJit(entitlementsText)) {
    throw new Error(`${file} has JIT entitlements but is not ${JIT_ENTITLEMENT_RELATIVE_PATH}`)
  }
}

/** After packaging (including SKIP_NOTARIZE=1): every Mach-O in the extracted
 * component tarball must satisfy
 * `codesign --verify --strict -R='anchor apple generic and certificate leaf[subject.OU] = "<TEAM>"'`,
 * carry the hardened runtime flag, and (unless it is exactly
 * {@link JIT_ENTITLEMENT_RELATIVE_PATH}) have no JIT entitlements.
 * `requirement` replaces the team requirement (ad-hoc tests). Pass
 * `requirement: null` to skip `-R`. `requireJitMatch` (default true) fails
 * unless the JIT path occurs exactly once. */
export async function verifySignedMachOTree({
  root, teamId = EXPECTED_TEAM_ID, requirement, runCommand = execFileAsync, files, requireJitMatch = true,
}) {
  if (!root || !path.isAbsolute(root)) throw new Error('root must be an absolute path')
  const discovered = files ?? findMachOFiles(root)
  if (discovered.length === 0) throw new Error(`no Mach-O files found under ${root}`)
  const resolvedRequirement = requirement === undefined
    ? (teamId ? teamIdRequirement(teamId) : null)
    : requirement
  for (const { file } of discovered) {
    try {
      await runCommand('codesign', codesignVerifyArgs(file, { requirement: resolvedRequirement ?? null }))
    } catch (error) {
      throw new Error(`${file} failed codesign --verify --strict -R for Team ID ${teamId ?? '(none)'}: ${commandFailureDetail(error)}`)
    }
    if (resolvedRequirement) {
      await assertRuntimeAndJitEntitlements({ file, root, runCommand })
    }
  }
  if (requireJitMatch) assertExactlyOneJitPath(root, discovered)
  return discovered.length
}

async function main(argv) {
  const [command, ...rest] = argv
  switch (command) {
    case 'version':
      process.stdout.write(`${versionFromReleaseTag(rest[0])}\n`)
      return
    case 'check-source':
      process.stdout.write(`${checkOpenDesignSource(rest[0])}\n`)
      return
    case 'check-resources':
      process.stdout.write(`${assertOpenDesignResourcesStamp(path.resolve(rest[0])).sha}\n`)
      return
    case 'verify-signed': {
      const count = await verifySignedMachOTree({ root: path.resolve(rest[0]) })
      process.stdout.write(`verified ${count} Mach-O files under ${rest[0]}\n`)
      return
    }
    case 'sign': {
      const [root, identity, entitlements] = rest
      const files = await signOpenDesignTree({
        root: path.resolve(root), identity, entitlements, log: (line) => process.stderr.write(`${line}\n`),
      })
      process.stdout.write(`signed ${files.length} Mach-O files under ${root}\n`)
      return
    }
    default:
      throw new Error('usage: open-design-mac-release.mjs version <tag> | check-source <dir> | check-resources <dir> | verify-signed <dir> | sign <dir> <identity> <entitlements>')
  }
}

if (process.argv[1] != null && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main(process.argv.slice(2)).catch((error) => {
    process.stderr.write(`ERROR: ${error instanceof Error ? error.message : String(error)}\n`)
    process.exit(1)
  })
}
