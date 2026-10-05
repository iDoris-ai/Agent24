import assert from 'node:assert/strict'
import crypto from 'node:crypto'
import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { fileURLToPath } from 'node:url'

import { assetFileName, packOpenDesignComponent, releaseAssetUrl } from './pack-open-design-component.mjs'

const scriptDir = path.dirname(fileURLToPath(import.meta.url))

function seedStageDir(root) {
  for (const relative of ['app/prebundled', 'open-design', 'open-design-web-standalone']) {
    fs.mkdirSync(path.join(root, relative), { recursive: true })
  }
  fs.writeFileSync(path.join(root, 'app/package.json'), JSON.stringify({ version: '0.22.2' }))
  fs.writeFileSync(path.join(root, 'app/prebundled/agent24-headless.cjs'), 'module.exports = {}\n')
  fs.writeFileSync(path.join(root, 'open-design/marker.txt'), 'open-design\n')
  fs.writeFileSync(path.join(root, 'open-design-web-standalone/server.js'), '// standalone\n')
  return root
}

function findAsset(dir) {
  const [name] = fs.readdirSync(dir).filter((entry) => entry.endsWith('.tar.gz'))
  assert.ok(name, `no .tar.gz found in ${dir}`)
  return path.join(dir, name)
}

test('packs a content-addressed tarball (separate dir) and a matching manifest', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-pack-'))
  try {
    const stageDir = seedStageDir(path.join(tmp, 'stage'))
    const outDir = path.join(tmp, 'out')
    const tarballOutDir = path.join(tmp, 'tarball-out')

    const manifest = await packOpenDesignComponent({
      stageDir,
      outDir,
      tarballOutDir,
      repository: 'iDoris-ai/Agent24',
      appVersion: '0.6.0',
      platform: 'linux',
      arch: 'x64',
    })

    assert.equal(manifest.version, '0.22.2')
    assert.equal(manifest.platform, 'linux')
    assert.equal(manifest.arch, 'x64')
    assert.match(
      manifest.url,
      /^https:\/\/github\.com\/iDoris-ai\/Agent24\/releases\/download\/v0\.6\.0\/open-design-0\.22\.2-34bc2ba-linux-x64-[0-9a-f]{12}\.tar\.gz$/,
    )
    assert.equal(typeof manifest.sha256, 'string')
    assert.equal(manifest.sha256.length, 64)
    assert.ok(manifest.size > 0)

    // C1: the manifest dir (what extraResources bakes into the app) must
    // NEVER contain the tarball itself.
    assert.deepEqual(fs.readdirSync(outDir), ['open-design-component.json'])

    const assetPath = findAsset(tarballOutDir)
    assert.ok(path.basename(assetPath).includes(manifest.sha256.slice(0, 12)))
    const bytes = fs.readFileSync(assetPath)
    assert.equal(bytes.length, manifest.size)
    const digest = crypto.createHash('sha256').update(bytes).digest('hex')
    assert.equal(digest, manifest.sha256)
    // No leftover .tmp-* staging file next to the final asset.
    assert.deepEqual(fs.readdirSync(tarballOutDir), [path.basename(assetPath)])

    const manifestOnDisk = JSON.parse(fs.readFileSync(path.join(outDir, 'open-design-component.json'), 'utf8'))
    assert.deepEqual(manifestOnDisk, manifest)

    // The tarball layout must match the staged dir exactly (same top-level
    // trees), so the runtime installer can extract it straight into place.
    const listing = execFileSync('tar', ['-tzf', assetPath], { encoding: 'utf8' })
      .split('\n').filter(Boolean).sort()
    assert.ok(listing.includes('app/package.json'))
    assert.ok(listing.includes('open-design/marker.txt'))
    assert.ok(listing.includes('open-design-web-standalone/server.js'))
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true })
  }
})

test('rejects outDir === tarballOutDir (the manifest dir must never also hold the tarball)', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-pack-samedir-'))
  try {
    const stageDir = seedStageDir(path.join(tmp, 'stage'))
    const sharedDir = path.join(tmp, 'shared')
    await assert.rejects(
      packOpenDesignComponent({
        stageDir, outDir: sharedDir, tarballOutDir: sharedDir,
        repository: 'iDoris-ai/Agent24', appVersion: '0.6.0', platform: 'linux', arch: 'x64',
      }),
      /tarballOutDir must differ from outDir/,
    )
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true })
  }
})

test('repeated packs of the same tree contain the same files with the same content', async () => {
  // On GNU tar (every CI runner) this is also byte-identical; bsdtar (macOS)
  // doesn't support the determinism flags, so we only assert on content here
  // and let the GNU-tar-specific byte-identity be an incidental extra on CI.
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-pack-repeat-'))
  try {
    const stageDir = seedStageDir(path.join(tmp, 'stage'))
    const outDir1 = path.join(tmp, 'out1')
    const outDir2 = path.join(tmp, 'out2')
    const tarballOutDir1 = path.join(tmp, 'tarball-out1')
    const tarballOutDir2 = path.join(tmp, 'tarball-out2')

    const manifest1 = await packOpenDesignComponent({
      stageDir, outDir: outDir1, tarballOutDir: tarballOutDir1,
      repository: 'iDoris-ai/Agent24', appVersion: '0.6.0', platform: 'linux', arch: 'x64',
    })
    const manifest2 = await packOpenDesignComponent({
      stageDir, outDir: outDir2, tarballOutDir: tarballOutDir2,
      repository: 'iDoris-ai/Agent24', appVersion: '0.6.0', platform: 'linux', arch: 'x64',
    })

    const extract1 = path.join(tmp, 'extract1')
    const extract2 = path.join(tmp, 'extract2')
    fs.mkdirSync(extract1, { recursive: true })
    fs.mkdirSync(extract2, { recursive: true })
    execFileSync('tar', ['-xzf', findAsset(tarballOutDir1), '-C', extract1])
    execFileSync('tar', ['-xzf', findAsset(tarballOutDir2), '-C', extract2])

    assert.equal(
      fs.readFileSync(path.join(extract1, 'open-design/marker.txt'), 'utf8'),
      fs.readFileSync(path.join(extract2, 'open-design/marker.txt'), 'utf8'),
    )
    // Same content -> same sha256 -> same content-addressed filename.
    assert.equal(manifest1.sha256, manifest2.sha256)
    assert.equal(path.basename(findAsset(tarballOutDir1)), path.basename(findAsset(tarballOutDir2)))
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true })
  }
})

test('rejects a stage directory missing one of the three required trees', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-pack-missing-'))
  try {
    const stageDir = path.join(tmp, 'stage')
    fs.mkdirSync(path.join(stageDir, 'app'), { recursive: true })
    fs.mkdirSync(path.join(stageDir, 'open-design'), { recursive: true })
    // open-design-web-standalone intentionally missing

    await assert.rejects(
      packOpenDesignComponent({
        stageDir, outDir: path.join(tmp, 'out'), tarballOutDir: path.join(tmp, 'tarball-out'),
        repository: 'iDoris-ai/Agent24', appVersion: '0.6.0',
      }),
      /open-design-web-standalone/,
    )
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true })
  }
})

test('assetFileName requires a sha256 and composes a content-addressed name; releaseAssetUrl composes the download URL', () => {
  const sha256 = 'a'.repeat(64)
  assert.equal(
    assetFileName({ odVersion: '0.22.2', odSha: 'f84ff89656143b5fa3fe8f0891b7f2bf38769d9f', platform: 'darwin', arch: 'arm64', sha256 }),
    'open-design-0.22.2-f84ff89-darwin-arm64-aaaaaaaaaaaa.tar.gz',
  )
  assert.throws(
    () => assetFileName({ odVersion: '0.22.2', odSha: 'f84ff89656143b5fa3fe8f0891b7f2bf38769d9f', platform: 'darwin', arch: 'arm64' }),
    /requires the tarball sha256/,
  )
  assert.equal(
    releaseAssetUrl({ repository: 'iDoris-ai/Agent24', appVersion: '0.6.0', assetName: 'open-design-0.22.2-f84ff89-darwin-arm64-aaaaaaaaaaaa.tar.gz' }),
    'https://github.com/iDoris-ai/Agent24/releases/download/v0.6.0/open-design-0.22.2-f84ff89-darwin-arm64-aaaaaaaaaaaa.tar.gz',
  )
})

test('the pack script CLI entry fails loudly with a usage message when env vars are missing', () => {
  assert.throws(() => execFileSync(process.execPath, [path.join(scriptDir, 'pack-open-design-component.mjs')], {
    env: { ...process.env, A24_OD_STAGE_DIR: '', A24_OD_PACK_OUT_DIR: '', A24_OD_TARBALL_OUT_DIR: '', A24_OD_APP_VERSION: '' },
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  }), /usage:/)
})
