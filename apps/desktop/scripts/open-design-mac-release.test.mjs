import assert from 'node:assert/strict'
import { execFile, execFileSync, spawnSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { promisify } from 'node:util'

const execFileAsync = promisify(execFile)

import { OPEN_DESIGN_SHA } from './pack-open-design-component.mjs'
import {
  assertOpenDesignResourcesStamp,
  assertPinnedSha,
  checkOpenDesignSource,
  classifyMachO,
  desktopPackageVersion,
  findMachOFiles,
  JIT_ENTITLEMENT_BASENAMES,
  machOSigningArgs,
  signOpenDesignTree,
  teamIdFromIdentity,
  verifySignedMachOTree,
  versionFromReleaseTag,
  writeOpenDesignSourceStamp,
} from './open-design-mac-release.mjs'

test('versionFromReleaseTag strips the leading v and keeps the prerelease suffix', () => {
  assert.equal(desktopPackageVersion(), '0.6.0')
  assert.equal(versionFromReleaseTag('v0.6.0-preview.20261028.10'), '0.6.0-preview.20261028.10')
  assert.equal(versionFromReleaseTag('v0.6.0'), '0.6.0')
  assert.equal(versionFromReleaseTag('v0.6.0-rc.1'), '0.6.0-rc.1')
})

test('versionFromReleaseTag refuses a tag that does not start with the desktop package version', () => {
  assert.throws(() => versionFromReleaseTag('v1.2.3'), /package\.json version 0\.6\.0/)
  assert.throws(() => versionFromReleaseTag('v0.6.1'), /package\.json version 0\.6\.0/)
  assert.throws(() => versionFromReleaseTag('v0.6.1-preview.1'), /package\.json version 0\.6\.0/)
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

test('checkOpenDesignSource rejects a dirty checkout before comparing the sha', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-dirty-'))
  try {
    const git = (...args) => execFileSync('git', ['-C', dir, '-c', 'user.name=t', '-c', 'user.email=t@t', ...args], { stdio: 'pipe' })
    git('init', '-q')
    git('commit', '--allow-empty', '-q', '-m', 'x')
    fs.writeFileSync(path.join(dir, 'local.txt'), 'dirty')
    assert.throws(() => checkOpenDesignSource(dir), /uncommitted changes/)
  } finally {
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

test('reused Resources must carry a stamp whose sha matches the pin', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-stamp-'))
  try {
    assert.throws(() => assertOpenDesignResourcesStamp(dir), /stamp is missing/)
    writeOpenDesignSourceStamp(dir, { sha: '0'.repeat(40), version: '0.22.2' })
    assert.throws(() => assertOpenDesignResourcesStamp(dir), /does not match pin/)
    writeOpenDesignSourceStamp(dir, { sha: OPEN_DESIGN_SHA, version: '0.22.2' })
    assert.equal(assertOpenDesignResourcesStamp(dir).sha, OPEN_DESIGN_SHA)
    assert.throws(() => assertOpenDesignResourcesStamp('relative/resources'), /absolute/)
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

const DEVELOPER_ID = 'Developer ID Application: Huifeng Jiao (HYAP444FN9)'

test('libraries get no entitlements; only opencode gets the JIT plist; executables use runtime', () => {
  const entitlements = '/tmp/entitlements.mac.plist'
  const identity = DEVELOPER_ID
  for (const name of ['libfoo.dylib', 'addon.node']) {
    const args = machOSigningArgs({ file: `/tree/${name}`, kind: 'library', identity, entitlements })
    assert.equal(args.includes('--entitlements'), false)
    assert.deepEqual(args.slice(args.indexOf('--options'), args.indexOf('--options') + 2), ['--options', 'runtime'])
  }
  for (const name of ['ffmpeg', 'esbuild', 'spawn-helper', 'vela']) {
    const args = machOSigningArgs({ file: `/tree/${name}`, kind: 'executable', identity, entitlements })
    assert.equal(args.includes('--entitlements'), false)
    assert.deepEqual(args.slice(args.indexOf('--options'), args.indexOf('--options') + 2), ['--options', 'runtime'])
  }
  const opencode = machOSigningArgs({ file: '/tree/opencode', kind: 'executable', identity, entitlements })
  assert.equal(JIT_ENTITLEMENT_BASENAMES.has('opencode'), true)
  assert.equal(opencode[opencode.indexOf('--entitlements') + 1], entitlements)
  assert.deepEqual(opencode.slice(opencode.indexOf('--options'), opencode.indexOf('--options') + 2), ['--options', 'runtime'])
})

test('signOpenDesignTree records codesign --verify and checks an injected Team ID (ad-hoc derives none)', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-sign-plan-'))
  try {
    const tool = path.join(root, 'ffmpeg')
    const calls = []
    const runCommand = async (cmd, args) => {
      calls.push([cmd, [...args]])
      if (args[0] === '-dv') return { stdout: '', stderr: 'TeamIdentifier=HYAP444FN9\n' }
      return { stdout: '', stderr: '' }
    }
    await signOpenDesignTree({
      root,
      identity: '-',
      timestamp: false,
      expectedTeamId: 'HYAP444FN9',
      runCommand,
      files: [{ file: tool, kind: 'executable' }],
    })
    assert.ok(calls.some(([cmd, args]) => cmd === 'codesign' && args[0] === '--verify' && args[1] === '--strict' && args[2] === tool))
    assert.ok(calls.some(([cmd, args]) => cmd === 'codesign' && args[0] === '-dv' && args[1] === tool))
    const signArgs = calls.find(([cmd, args]) => cmd === 'codesign' && args.includes('--sign'))[1]
    assert.deepEqual(signArgs.slice(signArgs.indexOf('--options'), signArgs.indexOf('--options') + 2), ['--options', 'runtime'])
    assert.equal(signArgs.includes('--entitlements'), false)

    const skipped = []
    await signOpenDesignTree({
      root,
      identity: '-',
      timestamp: false,
      runCommand: async (cmd, args) => { skipped.push(args); return { stdout: '', stderr: '' } },
      files: [{ file: tool, kind: 'executable' }],
    })
    assert.equal(skipped.some((args) => args[0] === '-dv'), false)

    await assert.rejects(
      signOpenDesignTree({
        root,
        identity: '-',
        timestamp: false,
        expectedTeamId: 'HYAP444FN9',
        runCommand: async (cmd, args) => ({ stdout: '', stderr: args[0] === '-dv' ? 'TeamIdentifier=not set\n' : '' }),
        files: [{ file: tool, kind: 'executable' }],
      }),
      /Team ID HYAP444FN9/,
    )
  } finally {
    fs.rmSync(root, { recursive: true, force: true })
  }
})

test('verifySignedMachOTree requires codesign --verify and the expected Team ID', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-verify-signed-'))
  try {
    const file = path.join(root, 'tool')
    const calls = []
    const count = await verifySignedMachOTree({
      root,
      files: [{ file, kind: 'executable' }],
      runCommand: async (cmd, args) => {
        calls.push(args)
        return { stdout: '', stderr: args[0] === '-dv' ? 'TeamIdentifier=HYAP444FN9\n' : '' }
      },
    })
    assert.equal(count, 1)
    assert.deepEqual(calls[0], ['--verify', '--strict', file])
    await assert.rejects(
      verifySignedMachOTree({
        root,
        files: [{ file, kind: 'executable' }],
        runCommand: async (cmd, args) => ({ stdout: '', stderr: args[0] === '-dv' ? 'TeamIdentifier=\n' : '' }),
      }),
      /TeamIdentifier is not HYAP444FN9/,
    )
  } finally {
    fs.rmSync(root, { recursive: true, force: true })
  }
})

test('dylib and node samples are signed without entitlements', macOnly, async () => {
  const candidates = ['/usr/lib/libobjc-trampolines.dylib', '/usr/lib/libc++.1.dylib']
  const source = candidates.find((item) => {
    try { return fs.statSync(item).isFile() } catch { return false }
  })
  assert.ok(source, `no sample dylib; tried ${candidates.join(', ')}`)
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-lib-'))
  try {
    fs.copyFileSync(source, path.join(root, 'libfoo.dylib'))
    fs.copyFileSync(source, path.join(root, 'addon.node'))
    fs.copyFileSync('/usr/bin/true', path.join(root, 'ffmpeg'))
    const entitlements = path.join(root, 'entitlements.plist')
    fs.writeFileSync(entitlements, `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>com.apple.security.cs.allow-jit</key><true/>
  <key>com.apple.security.cs.allow-unsigned-executable-memory</key><true/>
</dict></plist>
`)
    const calls = []
    await signOpenDesignTree({
      root,
      identity: '-',
      timestamp: false,
      entitlements,
      runCommand: async (cmd, args) => {
        calls.push([...args])
        return execFileAsync(cmd, args)
      },
    })
    const signed = calls.filter((args) => args.includes('--sign'))
    for (const name of ['libfoo.dylib', 'addon.node']) {
      const args = signed.find((item) => item.at(-1).endsWith(name))
      assert.ok(args, name)
      assert.equal(args.includes('--entitlements'), false)
      const described = spawnSync('codesign', ['-d', '--entitlements', ':-', path.join(root, name)], { encoding: 'utf8' })
      const text = `${described.stdout}\n${described.stderr}`
      assert.equal(text.includes('allow-jit'), false)
      assert.equal(text.includes('allow-unsigned-executable-memory'), false)
    }
    const ffmpeg = signed.find((item) => item.at(-1).endsWith('ffmpeg'))
    assert.deepEqual(ffmpeg.slice(ffmpeg.indexOf('--options'), ffmpeg.indexOf('--options') + 2), ['--options', 'runtime'])
    assert.equal(ffmpeg.includes('--entitlements'), false)
    assert.equal(classifyMachO(path.join(root, 'libfoo.dylib')).kind, 'library')
    assert.equal(classifyMachO(path.join(root, 'addon.node')).kind, 'library')
  } finally {
    fs.rmSync(root, { recursive: true, force: true })
  }
})

test('release-mac.sh pins the repository and prints the upload command; verify reads the pin version', () => {
  const release = fs.readFileSync(new URL('./release-mac.sh', import.meta.url), 'utf8')
  assert.match(release, /A24_OD_REPOSITORY=iDoris-ai\/Agent24/)
  assert.match(release, /check-resources/)
  assert.match(release, /gh release upload \\"\$\{A24_RELEASE_TAG\}\\" \\"\$\{OD_TARBALL\}\\" -R iDoris-ai\/Agent24/)
  const verify = fs.readFileSync(new URL('./verify-mac-open-design.sh', import.meta.url), 'utf8')
  assert.equal(verify.includes("'0.22.2'"), false)
  assert.match(verify, /OPEN_DESIGN_PIN_VERSION/)
  assert.match(verify, /https:\/\/github.com\/iDoris-ai\/Agent24\/releases\/download\//)
  assert.match(verify, /verify-signed/)
})
