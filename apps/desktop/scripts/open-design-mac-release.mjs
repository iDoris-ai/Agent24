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
import fs from 'node:fs'
import path from 'node:path'
import { pathToFileURL } from 'node:url'
import { promisify } from 'node:util'

import { OPEN_DESIGN_SHA } from './pack-open-design-component.mjs'

const execFileAsync = promisify(execFile)

/** `v0.6.0-preview.20261028.10` -> `0.6.0-preview.20261028.10`. The result is
 * baked into the manifest URL `releases/download/v<version>/...`, so it MUST be
 * the real GitHub release tag minus the `v` — never package.json's version. */
export function versionFromReleaseTag(tag) {
  if (typeof tag !== 'string' || tag.trim() === '') {
    throw new Error(
      'A24_RELEASE_TAG is required (the GitHub release tag this build will be published under, e.g. v0.6.0-preview.20261028.10); ' +
      "refusing to guess from package.json because the manifest's download URL must match a tag that really exists",
    )
  }
  const match = /^v(\d+\.\d+\.\d+(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?)$/.exec(tag.trim())
  if (!match) throw new Error(`A24_RELEASE_TAG must look like v<semver> (e.g. v0.6.0-preview.20261028.10); got: ${tag}`)
  return match[1]
}

export function assertPinnedSha(actualSha, expected = OPEN_DESIGN_SHA) {
  if (String(actualSha).trim() !== expected) {
    throw new Error(`Open Design checkout must be exact ${expected}; found ${String(actualSha).trim() || '(empty)'}`)
  }
}

export function checkOpenDesignSource(dir) {
  if (!dir || !path.isAbsolute(dir)) throw new Error('Open Design checkout path must be absolute')
  const sha = execFileSync('git', ['-C', dir, 'rev-parse', 'HEAD'], { encoding: 'utf8' })
  assertPinnedSha(sha)
  return sha.trim()
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

/**
 * Signs every Mach-O under `root` (Developer ID, Hardened Runtime, secure
 * timestamp) and verifies each with `codesign --verify --strict`, plus that the
 * Team ID matches the identity's. `identity: '-'` (ad-hoc) is for tests only.
 */
export async function signOpenDesignTree({ root, identity, entitlements, timestamp = true, log = () => {} }) {
  if (!root || !path.isAbsolute(root)) throw new Error('root must be an absolute path')
  if (!identity) throw new Error('identity is required')
  const teamId = teamIdFromIdentity(identity)
  const adhoc = identity === '-'
  if (!adhoc && !teamId) throw new Error(`cannot derive a Team ID from signing identity: ${identity}`)
  if (entitlements && !fs.existsSync(entitlements)) throw new Error(`entitlements file is missing: ${entitlements}`)

  const files = findMachOFiles(root)
  if (files.length === 0) throw new Error(`no Mach-O files found under ${root} — refusing to continue (expected native modules)`)
  for (const { file, kind } of files) {
    const args = ['--force', '--sign', identity]
    if (!adhoc) args.push('--options', 'runtime')
    if (timestamp && !adhoc) args.push('--timestamp')
    // Same entitlements as the app for spawned executables: some (e.g. JS/JIT
    // runtimes) need JIT under Hardened Runtime; harmless for the rest.
    if (kind === 'executable' && entitlements) args.push('--entitlements', entitlements)
    args.push(file)
    await execFileAsync('codesign', args)
    await execFileAsync('codesign', ['--verify', '--strict', file])
    if (teamId) {
      const { stderr } = await execFileAsync('codesign', ['-dv', file])
      if (!stderr.includes(`TeamIdentifier=${teamId}`)) {
        throw new Error(`${file} was not signed with Team ID ${teamId}`)
      }
    }
    log(`signed (${kind}) ${path.relative(root, file)}`)
  }
  return files
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
    case 'sign': {
      const [root, identity, entitlements] = rest
      const files = await signOpenDesignTree({
        root: path.resolve(root), identity, entitlements, log: (line) => process.stderr.write(`${line}\n`),
      })
      process.stdout.write(`signed ${files.length} Mach-O files under ${root}\n`)
      return
    }
    default:
      throw new Error('usage: open-design-mac-release.mjs version <tag> | check-source <dir> | sign <dir> <identity> <entitlements>')
  }
}

if (process.argv[1] != null && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main(process.argv.slice(2)).catch((error) => {
    process.stderr.write(`ERROR: ${error instanceof Error ? error.message : String(error)}\n`)
    process.exit(1)
  })
}
