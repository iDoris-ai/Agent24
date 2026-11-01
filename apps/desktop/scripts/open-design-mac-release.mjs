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

/** Basenames that get allow-jit + allow-unsigned-executable-memory.
 * Only opencode (bun / JavaScriptCore). ffmpeg, esbuild, spawn-helper, and
 * vela are signed with Hardened Runtime and no entitlements: a local smoke
 * of the staged darwin-arm64 tree showed they still start. */
export const JIT_ENTITLEMENT_BASENAMES = new Set(['opencode'])

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
 * each symlink contributes a sha256 of its target text plus that target.
 * The stamp file itself is excluded (it records this digest). Directory
 * symlinks are not followed, so a link cannot pull outside files into the hash.
 */
export function openDesignResourceTreeDigest(resourcesRoot) {
  const entries = []
  const visit = (directory) => {
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
      entries.push({
        path: rel,
        kind: 'other',
        mode,
        sha256: crypto.createHash('sha256').update('').digest('hex'),
        link: null,
      })
    }
  }
  visit(resourcesRoot)
  entries.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0))
  const hash = crypto.createHash('sha256')
  for (const entry of entries) {
    hash.update(JSON.stringify({
      path: entry.path,
      kind: entry.kind,
      mode: entry.mode,
      sha256: entry.sha256,
      link: entry.link,
    }))
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
 * match the files, modes, and symlink targets on disk. */
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
    throw new Error('Open Design Resources tree digest does not match the stamp (a file, its mode, or a symlink target changed after prepare)')
  }
  return parsed
}

/** "Developer ID Application: Name (HYAP444FN9)" -> "HYAP444FN9"; null for ad-hoc "-". */
export function teamIdFromIdentity(identity) {
  const match = /\(([A-Z0-9]{10})\)\s*$/.exec(identity ?? '')
  return match ? match[1] : null
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
 * {@link JIT_ENTITLEMENT_BASENAMES} also get the JIT entitlements plist. */
export function machOSigningArgs({ file, kind, identity, entitlements, timestamp = true }) {
  const adhoc = identity === '-'
  const args = ['--force', '--sign', identity]
  if (kind === 'executable' || !adhoc) args.push('--options', 'runtime')
  if (timestamp && !adhoc) args.push('--timestamp')
  if (kind === 'executable' && entitlements && JIT_ENTITLEMENT_BASENAMES.has(path.basename(file))) {
    args.push('--entitlements', entitlements)
  }
  args.push(file)
  return args
}

/**
 * Signs every Mach-O under `root` (Developer ID, Hardened Runtime, secure
 * timestamp) and verifies each with `codesign --verify --strict`, plus that the
 * Team ID matches the identity's. `identity: '-'` (ad-hoc) is for tests only:
 * its derived Team ID is empty, so pass `expectedTeamId` to exercise the
 * check. `runCommand` and `files` are test seams (no `codesign` / no Mach-O).
 */
export async function signOpenDesignTree({
  root, identity, entitlements, timestamp = true, log = () => {},
  runCommand = execFileAsync, files, expectedTeamId,
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
  for (const { file, kind } of discovered) {
    await runCommand('codesign', machOSigningArgs({ file, kind, identity, entitlements, timestamp }))
    await runCommand('codesign', ['--verify', '--strict', file])
    if (teamId) {
      const { stderr = '' } = await runCommand('codesign', ['-dv', file])
      if (!stderr.includes(`TeamIdentifier=${teamId}`)) {
        throw new Error(`${file} was not signed with Team ID ${teamId}`)
      }
    }
    log(`signed (${kind}) ${path.relative(root, file)}`)
  }
  return discovered
}

/** After packaging (including SKIP_NOTARIZE=1): every Mach-O in the extracted
 * component tarball must `codesign --verify --strict` and carry Team ID
 * HYAP444FN9. */
export async function verifySignedMachOTree({ root, teamId = EXPECTED_TEAM_ID, runCommand = execFileAsync, files }) {
  if (!root || !path.isAbsolute(root)) throw new Error('root must be an absolute path')
  const discovered = files ?? findMachOFiles(root)
  if (discovered.length === 0) throw new Error(`no Mach-O files found under ${root}`)
  for (const { file } of discovered) {
    await runCommand('codesign', ['--verify', '--strict', file])
    const { stderr = '' } = await runCommand('codesign', ['-dv', file])
    if (!stderr.includes(`TeamIdentifier=${teamId}`)) {
      throw new Error(`${file} TeamIdentifier is not ${teamId}`)
    }
  }
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
