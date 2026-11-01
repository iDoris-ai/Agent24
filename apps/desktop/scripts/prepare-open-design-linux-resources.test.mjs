import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'

import { hoistStandalonePnpmDependencies } from './prepare-open-design-linux-resources.mjs'

// pnpm-style standalone: apps/web/node_modules/next is a symlink into
// node_modules/.pnpm/next@1/node_modules/next, and next's dependencies are its
// siblings there (the layout `next build` emits for a pnpm workspace).
function seedPnpmStandalone(root) {
  const pnpm = path.join(root, 'node_modules', '.pnpm')
  const pkg = (dir, name, files) => {
    const target = path.join(pnpm, dir, 'node_modules', ...name.split('/'))
    fs.mkdirSync(target, { recursive: true })
    for (const [file, content] of Object.entries(files)) fs.writeFileSync(path.join(target, file), content)
    return target
  }
  pkg('next@1', 'next', {
    'package.json': '{"name":"next","main":"index.js"}',
    'index.js': "module.exports = require('@swc/helpers/cjs/x.cjs') + require('react')",
  })
  pkg('@swc+helpers@1', '@swc/helpers', { 'package.json': '{"name":"@swc/helpers"}', ...{} })
  fs.mkdirSync(path.join(pnpm, '@swc+helpers@1', 'node_modules', '@swc', 'helpers', 'cjs'), { recursive: true })
  fs.writeFileSync(path.join(pnpm, '@swc+helpers@1', 'node_modules', '@swc', 'helpers', 'cjs', 'x.cjs'), "module.exports = 'swc;'")
  pkg('react@1', 'react', { 'package.json': '{"name":"react"}', 'index.js': "module.exports = 'react'" })
  const nextSiblings = path.join(pnpm, 'next@1', 'node_modules')
  fs.mkdirSync(path.join(nextSiblings, '@swc'), { recursive: true })
  fs.symlinkSync('../../../@swc+helpers@1/node_modules/@swc/helpers', path.join(nextSiblings, '@swc', 'helpers'))
  fs.symlinkSync('../../react@1/node_modules/react', path.join(nextSiblings, 'react'))
  const webModules = path.join(root, 'apps', 'web', 'node_modules')
  fs.mkdirSync(webModules, { recursive: true })
  fs.symlinkSync('../../../node_modules/.pnpm/next@1/node_modules/next', path.join(webModules, 'next'))
  fs.writeFileSync(path.join(root, 'apps', 'web', 'server.js'), "process.stdout.write(require('next'))")
}

const run = (root) => spawnSync(process.execPath, ['--preserve-symlinks', '--preserve-symlinks-main', path.join(root, 'apps/web/server.js')], { encoding: 'utf8' })

test('hoistStandalonePnpmDependencies makes the standalone tree resolvable when the loader does not realpath symlinks', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-hoist-'))
  try {
    seedPnpmStandalone(root)
    // Reproduces the real failure: symlink paths are kept, siblings are not on the lookup path.
    const before = run(root)
    assert.notEqual(before.status, 0)
    assert.match(before.stderr, /Cannot find module '@swc\/helpers/)

    const added = await hoistStandalonePnpmDependencies(root)
    assert.equal(added, 2) // @swc/helpers + react (next itself is already linked)
    const after = run(root)
    assert.equal(after.status, 0, after.stderr)
    assert.equal(after.stdout, 'swc;react')

    // Links are relative and point straight at real directories (stage-open-design verifies this).
    const link = path.join(root, 'apps/web/node_modules/@swc/helpers')
    assert.ok(!path.isAbsolute(fs.readlinkSync(link)))
    assert.equal(fs.realpathSync(link), fs.realpathSync(path.join(root, 'node_modules/.pnpm/@swc+helpers@1/node_modules/@swc/helpers')))
    assert.equal(fs.lstatSync(fs.realpathSync(link)).isSymbolicLink(), false)

    // Idempotent.
    assert.equal(await hoistStandalonePnpmDependencies(root), 0)
  } finally {
    fs.rmSync(root, { recursive: true, force: true })
  }
})

test('hoistStandalonePnpmDependencies is a no-op for a tree that is not pnpm-layout', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-hoist-none-'))
  try {
    fs.mkdirSync(path.join(root, 'apps/web/node_modules/next'), { recursive: true })
    assert.equal(await hoistStandalonePnpmDependencies(root), 0)
  } finally {
    fs.rmSync(root, { recursive: true, force: true })
  }
})
