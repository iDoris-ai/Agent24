// Packs the staged Open Design resources (produced by
// stage-open-design-resources.mjs — `app/`, `open-design/`,
// `open-design-web-standalone/`) into a single downloadable asset and writes
// the manifest that gets baked into the Agent24 app bundle at build time.
//
// Owner decision 2026-10-05: Open Design must NOT be bundled in the desktop
// installer (it alone drove the AppImage from 146MB to 934MB, the deb from
// 101MB to 622MB). Instead electron-builder's extraResources only gets this
// manifest; the actual component is downloaded on demand by
// src/main/open-design-component.ts the first time the user opens the
// "Design" page, verified against the sha256 baked in here, then extracted.
//
// PR #670 review (C1): the tarball and the manifest are written to TWO
// different directories (`outDir` vs `tarballOutDir`). electron-builder's
// extraResources only ever points at `outDir` — if the tarball lived next to
// the manifest there, it would get bundled into the installer too, defeating
// the entire point of this change.
//
// PR #670 review (M3): the asset filename is content-addressed (includes a
// prefix of the tarball's own sha256). A later rebuild with the same
// odVersion/odSha/platform/arch but different content (e.g. an Electron
// version bump that needs a new native module build) therefore never
// collides with — and so can never silently overwrite — a previously
// published asset that an already-shipped app's baked manifest still points
// at.

import crypto from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { execFile } from 'node:child_process'
import { pathToFileURL } from 'node:url'
import { promisify } from 'node:util'

const execFileAsync = promisify(execFile)

export const OPEN_DESIGN_PIN_VERSION = '0.22.2'
export const OPEN_DESIGN_SHA = '34bc2ba810cf894daff6a7c1736d1cef6c7726b1'

function requireDirectory(target, label) {
  const stat = fs.statSync(target, { throwIfNoEntry: false })
  if (!stat?.isDirectory()) throw new Error(`${label} is missing or not a directory: ${target}`)
}

let gnuTarAvailable
async function isGnuTar() {
  if (gnuTarAvailable !== undefined) return gnuTarAvailable
  try {
    const { stdout } = await execFileAsync('tar', ['--version'])
    gnuTarAvailable = /GNU tar/.test(stdout)
  } catch {
    gnuTarAvailable = false
  }
  return gnuTarAvailable
}

function sha256File(filePath) {
  return new Promise((resolve, reject) => {
    const hash = crypto.createHash('sha256')
    const stream = fs.createReadStream(filePath)
    stream.on('data', (chunk) => hash.update(chunk))
    stream.on('end', () => resolve(hash.digest('hex')))
    stream.on('error', reject)
  })
}

/** `sha256` must be the tarball's own content hash — the filename is
 * content-addressed (see M3 note above), not just version/sha/platform/arch. */
export function assetFileName({ odVersion, odSha, platform, arch, sha256 }) {
  if (!sha256 || typeof sha256 !== 'string') throw new Error('assetFileName requires the tarball sha256')
  const shortSha = odSha.slice(0, 7)
  const contentHash = sha256.slice(0, 12)
  return `open-design-${odVersion}-${shortSha}-${platform}-${arch}-${contentHash}.tar.gz`
}

export function releaseAssetUrl({ repository, appVersion, assetName }) {
  return `https://github.com/${repository}/releases/download/v${appVersion}/${assetName}`
}

/**
 * Tars up `stageDir` (expected to directly contain `app/`, `open-design/`,
 * `open-design-web-standalone/`) into `tarballOutDir/<assetFileName>`,
 * computes its sha256 + size, and writes `outDir/open-design-component.json`
 * — the exact manifest electron-builder will ship inside the app via
 * extraResources. `outDir` and `tarballOutDir` must be different directories
 * (or at least `outDir` must never be the one electron-builder's
 * extraResources points its `from` at while also holding the tarball).
 *
 * Returns the manifest object.
 */
export async function packOpenDesignComponent({
  stageDir,
  outDir,
  tarballOutDir,
  repository,
  appVersion,
  platform = process.platform,
  arch = process.arch,
  odVersion = OPEN_DESIGN_PIN_VERSION,
  odSha = OPEN_DESIGN_SHA,
}) {
  if (!stageDir || !path.isAbsolute(stageDir)) throw new Error('stageDir must be an absolute path')
  if (!outDir || !path.isAbsolute(outDir)) throw new Error('outDir must be an absolute path')
  if (!tarballOutDir || !path.isAbsolute(tarballOutDir)) throw new Error('tarballOutDir must be an absolute path')
  if (path.resolve(tarballOutDir) === path.resolve(outDir)) {
    throw new Error('tarballOutDir must differ from outDir — outDir is baked into the app via extraResources')
  }
  if (!repository) throw new Error('repository is required (e.g. iDoris-ai/Agent24)')
  if (!appVersion) throw new Error('appVersion is required')

  for (const relative of ['app', 'open-design', 'open-design-web-standalone']) {
    requireDirectory(path.join(stageDir, relative), `staged Open Design resource tree ${relative}`)
  }

  fs.mkdirSync(outDir, { recursive: true })
  fs.mkdirSync(tarballOutDir, { recursive: true })

  // The asset filename is content-addressed (includes the tarball's own
  // sha256 — see M3 above), so build to a temp name first, hash it, then
  // move it to its final, content-addressed name.
  const tempAssetPath = path.join(tarballOutDir, `.tmp-${crypto.randomBytes(6).toString('hex')}.tar.gz`)

  // GNU tar (the CI runners' /bin/tar) supports deterministic member order +
  // ownership, so repeated packs of the same staged tree are byte-identical.
  // macOS ships bsdtar, which lacks these flags — fall back to a plain
  // archive there; its content (and sha256 for THAT build) is still correct,
  // only cross-run byte-identity is not guaranteed.
  const gnuFlags = await isGnuTar()
    ? ['--sort=name', '--mtime=UTC 2020-01-01', '--owner=0', '--group=0', '--numeric-owner']
    : []
  try {
    await execFileAsync('tar', [
      ...gnuFlags,
      '-czf', tempAssetPath,
      '-C', stageDir,
      'app', 'open-design', 'open-design-web-standalone',
    ])

    const sha256 = await sha256File(tempAssetPath)
    const size = fs.statSync(tempAssetPath).size
    const assetName = assetFileName({ odVersion, odSha, platform, arch, sha256 })
    const assetPath = path.join(tarballOutDir, assetName)
    fs.renameSync(tempAssetPath, assetPath)

    const manifest = {
      version: odVersion,
      odSha,
      platform,
      arch,
      url: releaseAssetUrl({ repository, appVersion, assetName }),
      sha256,
      size,
    }

    fs.writeFileSync(path.join(outDir, 'open-design-component.json'), `${JSON.stringify(manifest, null, 2)}\n`)
    process.stdout.write(
      `Packed Open Design component ${assetName} (${size} bytes, sha256 ${sha256}) -> ${manifest.url}\n`,
    )
    return manifest
  } finally {
    fs.rmSync(tempAssetPath, { force: true })
  }
}

if (process.argv[1] != null && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const stageDir = process.env.A24_OD_STAGE_DIR?.trim()
  const outDir = process.env.A24_OD_PACK_OUT_DIR?.trim()
  const tarballOutDir = process.env.A24_OD_TARBALL_OUT_DIR?.trim()
  const repository = process.env.A24_OD_REPOSITORY?.trim() || 'iDoris-ai/Agent24'
  const appVersion = process.env.A24_OD_APP_VERSION?.trim()
  const platform = process.env.A24_OD_PLATFORM?.trim() || process.platform
  const arch = process.env.A24_OD_ARCH?.trim() || process.arch
  if (!stageDir || !outDir || !tarballOutDir || !appVersion) {
    throw new Error(
      'usage: A24_OD_STAGE_DIR=<staged dir> A24_OD_PACK_OUT_DIR=<manifest-only out dir> ' +
      'A24_OD_TARBALL_OUT_DIR=<separate tarball out dir> A24_OD_APP_VERSION=<version> ' +
      '[A24_OD_REPOSITORY=owner/repo] [A24_OD_PLATFORM=linux] [A24_OD_ARCH=x64] node pack-open-design-component.mjs',
    )
  }
  await packOpenDesignComponent({
    stageDir: path.resolve(stageDir),
    outDir: path.resolve(outDir),
    tarballOutDir: path.resolve(tarballOutDir),
    repository,
    appVersion,
    platform,
    arch,
  })
}
