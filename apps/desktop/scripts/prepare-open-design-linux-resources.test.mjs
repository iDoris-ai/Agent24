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

    const added = await hoistStandalonePnpmDependencies(root, undefined, { platform: 'darwin' })
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
    assert.equal(await hoistStandalonePnpmDependencies(root, undefined, { platform: 'darwin' }), 0)
  } finally {
    fs.rmSync(root, { recursive: true, force: true })
  }
})

test('hoistStandalonePnpmDependencies is a no-op for a tree that is not pnpm-layout', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-hoist-none-'))
  try {
    fs.mkdirSync(path.join(root, 'apps/web/node_modules/next'), { recursive: true })
    assert.equal(await hoistStandalonePnpmDependencies(root, undefined, { platform: 'darwin' }), 0)
  } finally {
    fs.rmSync(root, { recursive: true, force: true })
  }
})

function seedRealPackage(root, id, name, version) {
  const target = path.join(root, 'node_modules', '.pnpm', id, 'node_modules', ...name.split('/'))
  fs.mkdirSync(target, { recursive: true })
  fs.writeFileSync(path.join(target, 'package.json'), JSON.stringify({ name, version }))
  fs.writeFileSync(path.join(target, 'index.js'), `module.exports = ${JSON.stringify(version)}`)
  return target
}

test('hoist fails when the same package name has two realpaths and web has no direct dependency', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-hoist-conflict-'))
  const lines = []
  try {
    seedRealPackage(root, 'next@1', 'next', '1.0.0')
    seedRealPackage(root, 'left@1', 'left', '1.0.0')
    seedRealPackage(root, 'right@1', 'right', '1.0.0')
    const sharpA = seedRealPackage(root, 'sharp@0.34.5', 'sharp', '0.34.5')
    const sharpB = seedRealPackage(root, 'sharp@0.35.3', 'sharp', '0.35.3')
    const link = (fromId, name, toId) => {
      const dest = path.join(root, 'node_modules', '.pnpm', fromId, 'node_modules', ...name.split('/'))
      fs.mkdirSync(path.dirname(dest), { recursive: true })
      const target = path.join(root, 'node_modules', '.pnpm', toId, 'node_modules', ...name.split('/'))
      fs.symlinkSync(path.relative(path.dirname(dest), target), dest)
    }
    link('next@1', 'left', 'left@1')
    link('next@1', 'right', 'right@1')
    link('left@1', 'sharp', 'sharp@0.34.5')
    link('right@1', 'sharp', 'sharp@0.35.3')
    const webModules = path.join(root, 'apps', 'web', 'node_modules')
    fs.mkdirSync(webModules, { recursive: true })
    fs.writeFileSync(path.join(root, 'apps', 'web', 'package.json'), JSON.stringify({ dependencies: { next: '1.0.0' } }))
    fs.symlinkSync('../../../node_modules/.pnpm/next@1/node_modules/next', path.join(webModules, 'next'))

    await assert.rejects(
      () => hoistStandalonePnpmDependencies(root, undefined, { platform: 'darwin', log: (line) => lines.push(line) }),
      (error) => {
        assert.match(error.message, /hoist conflict/)
        assert.match(error.message, /sharp/)
        assert.match(error.message, /0\.34\.5/)
        assert.match(error.message, /0\.35\.3/)
        assert.ok(error.message.includes(fs.realpathSync(sharpA)))
        assert.ok(error.message.includes(fs.realpathSync(sharpB)))
        return true
      },
    )
    assert.match(lines.join('\n'), /hoist conflict/)
  } finally {
    fs.rmSync(root, { recursive: true, force: true })
  }
})

test('hoist keeps the realpath already selected by an apps/web direct dependency', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-hoist-direct-'))
  const lines = []
  try {
    seedRealPackage(root, 'next@1', 'next', '1.0.0')
    const chosen = seedRealPackage(root, 'sharp@0.34.5', 'sharp', '0.34.5')
    seedRealPackage(root, 'sharp@0.35.3', 'sharp', '0.35.3')
    const nextSiblings = path.join(root, 'node_modules', '.pnpm', 'next@1', 'node_modules')
    fs.symlinkSync(
      path.relative(nextSiblings, path.join(root, 'node_modules', '.pnpm', 'sharp@0.35.3', 'node_modules', 'sharp')),
      path.join(nextSiblings, 'sharp'),
    )
    const webModules = path.join(root, 'apps', 'web', 'node_modules')
    fs.mkdirSync(webModules, { recursive: true })
    fs.writeFileSync(path.join(root, 'apps', 'web', 'package.json'), JSON.stringify({
      dependencies: { next: '1.0.0', sharp: '0.34.5' },
    }))
    fs.symlinkSync('../../../node_modules/.pnpm/next@1/node_modules/next', path.join(webModules, 'next'))
    fs.symlinkSync('../../../node_modules/.pnpm/sharp@0.34.5/node_modules/sharp', path.join(webModules, 'sharp'))

    const added = await hoistStandalonePnpmDependencies(root, undefined, { platform: 'darwin', log: (line) => lines.push(line) })
    assert.equal(added, 0)
    assert.equal(fs.realpathSync(path.join(webModules, 'sharp')), fs.realpathSync(chosen))
    const printed = lines.join('\n')
    assert.match(printed, /direct dependency selects sharp/)
    assert.match(printed, /0\.34\.5/)
    assert.match(printed, /0\.35\.3/)
  } finally {
    fs.rmSync(root, { recursive: true, force: true })
  }
})

test('hoist refuses a link target whose realpath is outside the standalone root', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-hoist-escape-'))
  const outside = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-hoist-outside-'))
  try {
    seedRealPackage(root, 'next@1', 'next', '1.0.0')
    fs.mkdirSync(path.join(outside, 'escaped'), { recursive: true })
    fs.writeFileSync(path.join(outside, 'escaped', 'package.json'), JSON.stringify({ name: 'escaped', version: '1.0.0' }))
    const nextSiblings = path.join(root, 'node_modules', '.pnpm', 'next@1', 'node_modules')
    fs.symlinkSync(path.join(outside, 'escaped'), path.join(nextSiblings, 'escaped'))
    const webModules = path.join(root, 'apps', 'web', 'node_modules')
    fs.mkdirSync(webModules, { recursive: true })
    fs.symlinkSync('../../../node_modules/.pnpm/next@1/node_modules/next', path.join(webModules, 'next'))

    await assert.rejects(
      () => hoistStandalonePnpmDependencies(root, undefined, { platform: 'darwin' }),
      /outside the standalone root/,
    )
  } finally {
    fs.rmSync(root, { recursive: true, force: true })
    fs.rmSync(outside, { recursive: true, force: true })
  }
})

test('hoist does not run on linux', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-hoist-linux-'))
  try {
    seedPnpmStandalone(root)
    const before = run(root)
    assert.notEqual(before.status, 0)
    assert.equal(await hoistStandalonePnpmDependencies(root, undefined, { platform: 'linux' }), 0)
    assert.equal(fs.existsSync(path.join(root, 'apps/web/node_modules/@swc/helpers')), false)
    const after = run(root)
    assert.notEqual(after.status, 0)
    assert.match(after.stderr, /Cannot find module '@swc\/helpers/)
  } finally {
    fs.rmSync(root, { recursive: true, force: true })
  }
})
