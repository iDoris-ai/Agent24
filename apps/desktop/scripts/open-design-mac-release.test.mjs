import assert from 'node:assert/strict'
import { execFile, execFileSync, spawnSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { promisify } from 'node:util'
import { fileURLToPath } from 'node:url'

const execFileAsync = promisify(execFile)

import { OPEN_DESIGN_PIN_VERSION, OPEN_DESIGN_SHA } from './pack-open-design-component.mjs'
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
    writeOpenDesignSourceStamp(dir, { sha: OPEN_DESIGN_SHA, version: OPEN_DESIGN_PIN_VERSION })
    assert.equal(assertOpenDesignResourcesStamp(dir).sha, OPEN_DESIGN_SHA)
    assert.equal(assertOpenDesignResourcesStamp(dir).version, OPEN_DESIGN_PIN_VERSION)
    assert.throws(() => assertOpenDesignResourcesStamp('relative/resources'), /absolute/)
  } finally {
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

test('reused Resources stamp rejects a file changed after it was written', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-stamp-body-'))
  try {
    const file = path.join(dir, 'pkg.js')
    // Longer than the digest's read chunk, so a hasher that keeps only the
    // first buffer would miss this tail and the stamp would still match.
    const bytes = Buffer.alloc(1024 * 1024 + 1, 7)
    fs.writeFileSync(file, bytes)
    writeOpenDesignSourceStamp(dir, { sha: OPEN_DESIGN_SHA, version: OPEN_DESIGN_PIN_VERSION })
    assert.equal(assertOpenDesignResourcesStamp(dir).sha, OPEN_DESIGN_SHA)
    const fd = fs.openSync(file, 'r+')
    try {
      fs.writeSync(fd, Buffer.from([8]), 0, 1, bytes.length - 1)
    } finally {
      fs.closeSync(fd)
    }
    assert.throws(() => assertOpenDesignResourcesStamp(dir), /tree digest/)
  } finally {
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

test('reused Resources stamp rejects a mode change after it was written', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-stamp-mode-'))
  try {
    const file = path.join(dir, 'pkg.js')
    fs.writeFileSync(file, 'same')
    fs.chmodSync(file, 0o644)
    writeOpenDesignSourceStamp(dir, { sha: OPEN_DESIGN_SHA, version: OPEN_DESIGN_PIN_VERSION })
    fs.chmodSync(file, 0o755)
    assert.throws(() => assertOpenDesignResourcesStamp(dir), /tree digest/)
  } finally {
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

test('reused Resources stamp rejects a version other than the pin', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-stamp-ver-'))
  try {
    fs.writeFileSync(path.join(dir, 'pkg.js'), 'x')
    writeOpenDesignSourceStamp(dir, { sha: OPEN_DESIGN_SHA, version: '9.9.9' })
    assert.throws(() => assertOpenDesignResourcesStamp(dir), /stamp version 9\.9\.9 does not match pin/)
  } finally {
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

test('reused Resources stamp rejects a symlink retargeted after it was written', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-stamp-link-'))
  try {
    fs.writeFileSync(path.join(dir, 'real.txt'), 'x')
    fs.symlinkSync('real.txt', path.join(dir, 'link.txt'))
    writeOpenDesignSourceStamp(dir, { sha: OPEN_DESIGN_SHA, version: OPEN_DESIGN_PIN_VERSION })
    fs.unlinkSync(path.join(dir, 'link.txt'))
    fs.symlinkSync('other.txt', path.join(dir, 'link.txt'))
    assert.throws(() => assertOpenDesignResourcesStamp(dir), /tree digest/)
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

function dirtyGitCheckout(prefix) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix))
  const git = (...args) => execFileSync('git', ['-C', dir, ...args], { stdio: 'pipe' })
  git('init', '-q')
  fs.writeFileSync(path.join(dir, 'local.txt'), 'base')
  git('-c', 'user.email=t@t', '-c', 'user.name=t', 'add', 'local.txt')
  git('-c', 'user.email=t@t', '-c', 'user.name=t', 'commit', '-q', '-m', 'x')
  fs.writeFileSync(path.join(dir, 'local.txt'), 'dirty')
  return dir
}

test('release-mac.sh refuses a dirty checkout when Open Design resources are reused', () => {
  const checkout = dirtyGitCheckout('a24-od-release-dirty-')
  const resources = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-release-res-'))
  try {
    const result = spawnSync('bash', [fileURLToPath(new URL('./release-mac.sh', import.meta.url))], {
      encoding: 'utf8',
      timeout: 20000,
      env: {
        ...process.env,
        A24_RELEASE_TAG: 'v0.6.0',
        A24_OPEN_DESIGN_SOURCE_DIR: checkout,
        A24_OPEN_DESIGN_RESOURCES_DIR: resources,
        SKIP_NOTARIZE: '1',
      },
    })
    const output = `${result.stdout}\n${result.stderr}`
    assert.notEqual(result.status, 0, output)
    assert.match(output, /uncommitted changes/)
    assert.doesNotMatch(output, /构建 Rust|electron-builder|只能在 macOS/)
  } finally {
    fs.rmSync(checkout, { recursive: true, force: true })
    fs.rmSync(resources, { recursive: true, force: true })
  }
})

test('rebuild-open-design-native rejects a dirty checkout before looking at the staged tree', () => {
  const checkout = dirtyGitCheckout('a24-od-rebuild-dirty-')
  try {
    const result = spawnSync(process.execPath, [fileURLToPath(new URL('./rebuild-open-design-native.mjs', import.meta.url))], {
      encoding: 'utf8',
      env: { ...process.env, A24_OPEN_DESIGN_SOURCE_DIR: checkout },
    })
    const output = `${result.stdout}\n${result.stderr}`
    assert.notEqual(result.status, 0, output)
    assert.match(output, /uncommitted changes/)
    assert.doesNotMatch(output, /staged Open Design package manifest is missing/)
  } finally {
    fs.rmSync(checkout, { recursive: true, force: true })
  }
})

// codesign 替身不走真签名：夹具 tarball 里没有已签名的 Mach-O，默认的
// `verify-signed`（codesign --verify --strict + Team ID）会因为夹具失败，
// 盖住清单、包内目录和 sha 这几条断言。A24_OD_SKIP_CODESIGN=1 只跳过这一步；
// release-mac.sh 不设置它，发布路径仍跑真实验证。
const verifyScript = fileURLToPath(new URL('./verify-mac-open-design.sh', import.meta.url))

function verifyFixture(dir, mutate) {
  const resources = path.join(dir, 'Agent24.app', 'Contents', 'Resources')
  fs.mkdirSync(resources, { recursive: true })
  const payload = path.join(dir, 'payload')
  fs.mkdirSync(payload)
  fs.writeFileSync(path.join(payload, 'hello.txt'), 'hello')
  const tarballDir = path.join(dir, 'tarballs')
  fs.mkdirSync(tarballDir)
  const asset = 'open-design-fixture.tar.gz'
  const tarball = path.join(tarballDir, asset)
  execFileSync('tar', ['-czf', tarball, '-C', payload, 'hello.txt'], { stdio: 'pipe' })
  const sha256 = execFileSync('shasum', ['-a', '256', tarball], { encoding: 'utf8' }).split(/\s+/)[0]
  const size = fs.statSync(tarball).size
  const manifest = {
    version: OPEN_DESIGN_PIN_VERSION,
    odSha: OPEN_DESIGN_SHA,
    platform: 'darwin',
    arch: 'arm64',
    url: `https://github.com/iDoris-ai/Agent24/releases/download/v0.6.0/${asset}`,
    sha256,
    size,
  }
  if (mutate) mutate({ manifest, resources })
  fs.writeFileSync(path.join(resources, 'open-design-component.json'), `${JSON.stringify(manifest)}\n`)
  const dmg = path.join(dir, 'Agent24.dmg')
  fs.writeFileSync(dmg, 'not-a-real-dmg')
  return { app: path.join(dir, 'Agent24.app'), dmg, tarballDir }
}

function runVerify(dir, mutate) {
  const fixture = verifyFixture(dir, mutate)
  return spawnSync('bash', [verifyScript, fixture.app, fixture.dmg, fixture.tarballDir, '0.6.0'], {
    encoding: 'utf8',
    env: { ...process.env, A24_OD_SKIP_CODESIGN: '1' },
  })
}

test('verify-mac-open-design accepts a manifest-only app and a matching tarball', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-verify-ok-'))
  try {
    const result = runVerify(dir)
    assert.equal(result.status, 0, `${result.stderr}\n${result.stdout}`)
    assert.match(result.stdout, /OK:/)
    assert.match(result.stdout, /A24_OD_SKIP_CODESIGN/)
  } finally {
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

test('verify-mac-open-design fails when the app bundle contains an Open Design directory', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-verify-tree-'))
  try {
    const result = runVerify(dir, ({ resources }) => {
      fs.mkdirSync(path.join(resources, 'open-design'))
      fs.writeFileSync(path.join(resources, 'open-design', 'extra.txt'), 'bundled')
    })
    const output = `${result.stdout}\n${result.stderr}`
    assert.notEqual(result.status, 0, output)
    assert.match(output, /Open Design 目录被打进了安装包/)
  } finally {
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

test('verify-mac-open-design fails when the manifest sha256 does not match the tarball', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-od-verify-sha-'))
  try {
    const result = runVerify(dir, ({ manifest }) => {
      manifest.sha256 = 'b'.repeat(64)
    })
    const output = `${result.stdout}\n${result.stderr}`
    assert.notEqual(result.status, 0, output)
    assert.match(output, /资产 sha256 与清单不一致/)
  } finally {
    fs.rmSync(dir, { recursive: true, force: true })
  }
})
