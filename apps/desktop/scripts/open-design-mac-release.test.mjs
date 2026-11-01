import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'

import { OPEN_DESIGN_SHA } from './pack-open-design-component.mjs'
import {
  assertPinnedSha,
  checkOpenDesignSource,
  classifyMachO,
  findMachOFiles,
  signOpenDesignTree,
  teamIdFromIdentity,
  versionFromReleaseTag,
} from './open-design-mac-release.mjs'

test('versionFromReleaseTag strips the leading v and keeps the prerelease suffix', () => {
  assert.equal(versionFromReleaseTag('v0.6.0-preview.20261028.10'), '0.6.0-preview.20261028.10')
  assert.equal(versionFromReleaseTag('v1.2.3'), '1.2.3')
})

test('versionFromReleaseTag refuses a missing or malformed tag (never falls back to package.json)', () => {
  for (const bad of [undefined, null, '', '   ']) {
    assert.throws(() => versionFromReleaseTag(bad), /A24_RELEASE_TAG is required/)
  }
  for (const bad of ['0.6.0', 'v0.6', 'release-1', 'v0.6.0/../x', 'v0.6.0 -x']) {
    assert.throws(() => versionFromReleaseTag(bad), /must look like v<semver>/)
  }
})

test('assertPinnedSha accepts only the exact pinned sha', () => {
  assert.doesNotThrow(() => assertPinnedSha(`${OPEN_DESIGN_SHA}\n`))
  assert.throws(() => assertPinnedSha('0'.repeat(40)), /must be exact/)
  assert.throws(() => assertPinnedSha(OPEN_DESIGN_SHA.slice(0, 10)), /must be exact/)
  assert.throws(() => assertPinnedSha(''), /found \(empty\)/)
})

test('checkOpenDesignSource rejects a checkout that is not at the pin', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-src-'))
  try {
    const git = (...args) => execFileSync('git', ['-C', dir, '-c', 'user.name=t', '-c', 'user.email=t@t', ...args], { stdio: 'pipe' })
    git('init', '-q')
    git('commit', '--allow-empty', '-q', '-m', 'x')
    assert.throws(() => checkOpenDesignSource(dir), /must be exact/)
    assert.throws(() => checkOpenDesignSource('relative/path'), /absolute/)
  } finally {
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

test('teamIdFromIdentity', () => {
  assert.equal(teamIdFromIdentity('Developer ID Application: Huifeng Jiao (HYAP444FN9)'), 'HYAP444FN9')
  assert.equal(teamIdFromIdentity('-'), null)
  assert.equal(teamIdFromIdentity('no team'), null)
})

const macOnly = { skip: process.platform !== 'darwin' }

test('findMachOFiles finds real Mach-O (exec + library), skips text and java-class-magic files', macOnly, () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-macho-'))
  try {
    fs.mkdirSync(path.join(root, 'nested'))
    fs.copyFileSync('/usr/bin/true', path.join(root, 'nested', 'tool'))
    fs.writeFileSync(path.join(root, 'readme.txt'), 'hello')
    // 0xCAFEBABE is also the Java class-file magic: must NOT be treated as Mach-O.
    fs.writeFileSync(path.join(root, 'Foo.class'), Buffer.from('cafebabe0000003400', 'hex'))
    const found = findMachOFiles(root)
    assert.deepEqual(found.map((f) => path.relative(root, f.file)), [path.join('nested', 'tool')])
    assert.equal(classifyMachO(path.join(root, 'nested', 'tool')).kind, 'executable')
    assert.equal(classifyMachO(path.join(root, 'readme.txt')), null)
  } finally {
    fs.rmSync(root, { recursive: true, force: true })
  }
})

test('signOpenDesignTree signs and verifies every Mach-O (ad-hoc stand-in for the Developer ID)', macOnly, async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-sign-'))
  try {
    fs.copyFileSync('/usr/bin/true', path.join(root, 'tool'))
    fs.writeFileSync(path.join(root, 'x.js'), '//')
    const signed = await signOpenDesignTree({ root, identity: '-', timestamp: false })
    assert.equal(signed.length, 1)
    execFileSync('codesign', ['--verify', '--strict', path.join(root, 'tool')])
  } finally {
    fs.rmSync(root, { recursive: true, force: true })
  }
})

test('signOpenDesignTree fails when there is nothing to sign, or the identity has no Team ID', macOnly, async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-sign-empty-'))
  try {
    fs.writeFileSync(path.join(root, 'x.js'), '//')
    await assert.rejects(signOpenDesignTree({ root, identity: '-' }), /no Mach-O files/)
    await assert.rejects(signOpenDesignTree({ root, identity: 'Some Cert' }), /Team ID/)
  } finally {
    fs.rmSync(root, { recursive: true, force: true })
  }
})
