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

import crypto from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { execFile } from 'node:child_process'
import { pathToFileURL } from 'node:url'
import { promisify } from 'node:util'

const execFileAsync = promisify(execFile)

export const OPEN_DESIGN_PIN_VERSION = '0.22.2'
export const OPEN_DESIGN_SHA = 'f84ff89656143b5fa3fe8f0891b7f2bf38769d9f'

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

export function assetFileName({ odVersion, odSha, platform, arch }) {
  const shortSha = odSha.slice(0, 7)
  return `open-design-${odVersion}-${shortSha}-${platform}-${arch}.tar.gz`
}

export function releaseAssetUrl({ repository, appVersion, assetName }) {
  return `https://github.com/${repository}/releases/download/v${appVersion}/${assetName}`
}

/**
 * Tars up `stageDir` (expected to directly contain `app/`, `open-design/`,
 * `open-design-web-standalone/`) into `outDir/<assetFileName>`, computes its
 * sha256 + size, and writes `outDir/open-design-component.json` — the exact
 * manifest electron-builder will ship inside the app via extraResources.
 *
 * Returns the manifest object.
 */
export async function packOpenDesignComponent({
  stageDir,
  outDir,
  repository,
  appVersion,
  platform = process.platform,
  arch = process.arch,
  odVersion = OPEN_DESIGN_PIN_VERSION,
  odSha = OPEN_DESIGN_SHA,
}) {
  if (!stageDir || !path.isAbsolute(stageDir)) throw new Error('stageDir must be an absolute path')
  if (!outDir || !path.isAbsolute(outDir)) throw new Error('outDir must be an absolute path')
  if (!repository) throw new Error('repository is required (e.g. iDoris-ai/Agent24)')
  if (!appVersion) throw new Error('appVersion is required')

  for (const relative of ['app', 'open-design', 'open-design-web-standalone']) {
    requireDirectory(path.join(stageDir, relative), `staged Open Design resource tree ${relative}`)
  }

  fs.mkdirSync(outDir, { recursive: true })
  const assetName = assetFileName({ odVersion, odSha, platform, arch })
  const assetPath = path.join(outDir, assetName)
  fs.rmSync(assetPath, { force: true })

  // GNU tar (the CI runners' /bin/tar) supports deterministic member order +
  // ownership, so repeated packs of the same staged tree are byte-identical.
  // macOS ships bsdtar, which lacks these flags — fall back to a plain
  // archive there; its content (and sha256 for THAT build) is still correct,
  // only cross-run byte-identity is not guaranteed.
  const gnuFlags = await isGnuTar()
    ? ['--sort=name', '--mtime=UTC 2020-01-01', '--owner=0', '--group=0', '--numeric-owner']
    : []
  await execFileAsync('tar', [
    ...gnuFlags,
    '-czf', assetPath,
    '-C', stageDir,
    'app', 'open-design', 'open-design-web-standalone',
  ])

  const sha256 = await sha256File(assetPath)
  const size = fs.statSync(assetPath).size
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
}

if (process.argv[1] != null && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const stageDir = process.env.A24_OD_STAGE_DIR?.trim()
  const outDir = process.env.A24_OD_PACK_OUT_DIR?.trim()
  const repository = process.env.A24_OD_REPOSITORY?.trim() || 'iDoris-ai/Agent24'
  const appVersion = process.env.A24_OD_APP_VERSION?.trim()
  const platform = process.env.A24_OD_PLATFORM?.trim() || process.platform
  const arch = process.env.A24_OD_ARCH?.trim() || process.arch
  if (!stageDir || !outDir || !appVersion) {
    throw new Error(
      'usage: A24_OD_STAGE_DIR=<staged dir> A24_OD_PACK_OUT_DIR=<out dir> A24_OD_APP_VERSION=<version> ' +
      '[A24_OD_REPOSITORY=owner/repo] [A24_OD_PLATFORM=linux] [A24_OD_ARCH=x64] node pack-open-design-component.mjs',
    )
  }
  await packOpenDesignComponent({
    stageDir: path.resolve(stageDir),
    outDir: path.resolve(outDir),
    repository,
    appVersion,
    platform,
    arch,
  })
}
