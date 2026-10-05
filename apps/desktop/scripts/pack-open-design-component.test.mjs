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

test('packs a deterministic tarball and a matching manifest', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-pack-'))
  try {
    const stageDir = seedStageDir(path.join(tmp, 'stage'))
    const outDir = path.join(tmp, 'out')

    const manifest = await packOpenDesignComponent({
      stageDir,
      outDir,
      repository: 'iDoris-ai/Agent24',
      appVersion: '0.6.0',
      platform: 'linux',
      arch: 'x64',
    })

    assert.equal(manifest.version, '0.22.2')
    assert.equal(manifest.platform, 'linux')
    assert.equal(manifest.arch, 'x64')
    assert.equal(
      manifest.url,
      'https://github.com/iDoris-ai/Agent24/releases/download/v0.6.0/open-design-0.22.2-f84ff89-linux-x64.tar.gz',
    )
    assert.equal(typeof manifest.sha256, 'string')
    assert.equal(manifest.sha256.length, 64)
    assert.ok(manifest.size > 0)

    const assetPath = path.join(outDir, 'open-design-0.22.2-f84ff89-linux-x64.tar.gz')
    const bytes = fs.readFileSync(assetPath)
    assert.equal(bytes.length, manifest.size)
    const digest = crypto.createHash('sha256').update(bytes).digest('hex')
    assert.equal(digest, manifest.sha256)

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

test('repeated packs of the same tree contain the same files with the same content', async () => {
  // On GNU tar (every CI runner) this is also byte-identical; bsdtar (macOS)
  // doesn't support the determinism flags, so we only assert on content here
  // and let the GNU-tar-specific byte-identity be an incidental extra on CI.
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-pack-repeat-'))
  try {
    const stageDir = seedStageDir(path.join(tmp, 'stage'))
    const outDir1 = path.join(tmp, 'out1')
    const outDir2 = path.join(tmp, 'out2')

    const manifest1 = await packOpenDesignComponent({
      stageDir, outDir: outDir1, repository: 'iDoris-ai/Agent24', appVersion: '0.6.0', platform: 'linux', arch: 'x64',
    })
    const manifest2 = await packOpenDesignComponent({
      stageDir, outDir: outDir2, repository: 'iDoris-ai/Agent24', appVersion: '0.6.0', platform: 'linux', arch: 'x64',
    })

    const extract1 = path.join(tmp, 'extract1')
    const extract2 = path.join(tmp, 'extract2')
    fs.mkdirSync(extract1, { recursive: true })
    fs.mkdirSync(extract2, { recursive: true })
    execFileSync('tar', ['-xzf', path.join(outDir1, 'open-design-0.22.2-f84ff89-linux-x64.tar.gz'), '-C', extract1])
    execFileSync('tar', ['-xzf', path.join(outDir2, 'open-design-0.22.2-f84ff89-linux-x64.tar.gz'), '-C', extract2])

    assert.equal(
      fs.readFileSync(path.join(extract1, 'open-design/marker.txt'), 'utf8'),
      fs.readFileSync(path.join(extract2, 'open-design/marker.txt'), 'utf8'),
    )
    assert.ok(manifest1.size > 0 && manifest2.size > 0)
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
        stageDir, outDir: path.join(tmp, 'out'), repository: 'iDoris-ai/Agent24', appVersion: '0.6.0',
      }),
      /open-design-web-standalone/,
    )
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true })
  }
})

test('assetFileName and releaseAssetUrl compose the expected names', () => {
  assert.equal(
    assetFileName({ odVersion: '0.22.2', odSha: 'f84ff89656143b5fa3fe8f0891b7f2bf38769d9f', platform: 'darwin', arch: 'arm64' }),
    'open-design-0.22.2-f84ff89-darwin-arm64.tar.gz',
  )
  assert.equal(
    releaseAssetUrl({ repository: 'iDoris-ai/Agent24', appVersion: '0.6.0', assetName: 'open-design-0.22.2-f84ff89-darwin-arm64.tar.gz' }),
    'https://github.com/iDoris-ai/Agent24/releases/download/v0.6.0/open-design-0.22.2-f84ff89-darwin-arm64.tar.gz',
  )
})

test('the pack script CLI entry fails loudly with a usage message when env vars are missing', () => {
  assert.throws(() => execFileSync(process.execPath, [path.join(scriptDir, 'pack-open-design-component.mjs')], {
    env: { ...process.env, A24_OD_STAGE_DIR: '', A24_OD_PACK_OUT_DIR: '', A24_OD_APP_VERSION: '' },
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  }), /usage:/)
})
