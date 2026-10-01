import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { fileURLToPath } from 'node:url'

const scriptDir = path.dirname(fileURLToPath(import.meta.url))
const stageScript = path.join(scriptDir, 'stage-open-design-resources.mjs')

function seedValidSource(root) {
  for (const relative of [
    'app/prebundled/daemon',
    'open-design',
    'open-design-web-standalone',
  ]) fs.mkdirSync(path.join(root, relative), { recursive: true })
  fs.writeFileSync(path.join(root, 'app/package.json'), JSON.stringify({ version: '0.22.2' }))
  for (const relative of [
    'app/prebundled/agent24-headless.cjs',
    'app/prebundled/agent24-headless.mjs',
    'app/prebundled/daemon/daemon-cli.mjs',
    'app/prebundled/daemon/daemon-sidecar.mjs',
    'app/prebundled/web-sidecar.mjs',
  ]) fs.writeFileSync(path.join(root, relative), '')
}

function runStage(sourceRoot) {
  return execFileSync(process.execPath, [stageScript], {
    env: { ...process.env, A24_OPEN_DESIGN_RESOURCES_DIR: sourceRoot },
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  })
}

test('stages a valid tree with a direct in-tree relative symlink', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-stage-valid-'))
  try {
    seedValidSource(tmp)
    fs.writeFileSync(path.join(tmp, 'open-design/target.txt'), 'ok')
    fs.symlinkSync('target.txt', path.join(tmp, 'open-design/link.txt'))
    assert.match(runStage(tmp), /Staged Open Design 0\.22\.2 resources/)
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true })
  }
})

test('rejects a symlink whose text leaves the staged resource trees', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-stage-lexical-'))
  try {
    seedValidSource(tmp)
    fs.mkdirSync(path.join(tmp, 'shortcut'))
    fs.writeFileSync(path.join(tmp, 'shortcut/target.txt'), 'outside')
    fs.symlinkSync('../../shortcut/target.txt', path.join(tmp, 'app/prebundled/escaped'))
    assert.throws(() => runStage(tmp), /symlink text escapes staged trees/)
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true })
  }
})

test('rejects a resource-tree root that is itself a symlink', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-stage-root-link-'))
  try {
    const outside = path.join(tmp, 'outside-app')
    seedValidSource(tmp)
    fs.renameSync(path.join(tmp, 'app'), outside)
    fs.symlinkSync(outside, path.join(tmp, 'app'))
    assert.throws(() => runStage(tmp), /tree root must not be a symlink: app/)
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true })
  }
})

test('rejects a link that reaches its target through another symlink', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-stage-chain-'))
  try {
    seedValidSource(tmp)
    fs.mkdirSync(path.join(tmp, 'open-design/real'))
    fs.writeFileSync(path.join(tmp, 'open-design/real/target.txt'), 'ok')
    fs.symlinkSync('real', path.join(tmp, 'open-design/alias'))
    fs.symlinkSync('alias/target.txt', path.join(tmp, 'open-design/link.txt'))
    assert.throws(() => runStage(tmp), /symlink crosses another symlink/)
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true })
  }
})
