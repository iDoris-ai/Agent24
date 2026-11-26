import { execFileSync } from 'node:child_process'
import crypto from 'node:crypto'
import fs from 'node:fs'
import http from 'node:http'
import type { AddressInfo } from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'

import {
  WEKNORA_LITE_LOCAL_BASE_URL_ENV,
  WeknoraLiteInstaller,
  pinProblem,
  verifyInstalledWeknoraLiteBinary,
  weknoraLiteInstallDir,
  type WeknoraLiteStatus,
} from './weknora-lite-component'
import { WEKNORA_LITE_PIN, type WeknoraLitePin } from './weknora-lite-pin'

const tempDirs: string[] = []
const servers: http.Server[] = []

function makeWritable(root: string): void {
  const stat = fs.lstatSync(root, { throwIfNoEntry: false })
  if (stat == null || stat.isSymbolicLink()) return
  if (stat.isDirectory()) {
    fs.chmodSync(root, 0o700)
    for (const entry of fs.readdirSync(root)) makeWritable(path.join(root, entry))
  } else if (stat.isFile()) fs.chmodSync(root, 0o600)
}

afterEach(async () => {
  for (const server of servers.splice(0)) {
    server.closeAllConnections()
    await new Promise((resolve) => server.close(resolve))
  }
  for (const dir of tempDirs.splice(0)) {
    makeWritable(dir)
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

function tmpDir(prefix: string): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix))
  tempDirs.push(dir)
  return dir
}

const sha256 = (data: Buffer | string): string => crypto.createHash('sha256').update(data).digest('hex')
const SOURCE_SHA = 'abcdef0123456789abcdef0123456789abcdef01'

interface Fixture {
  pin: WeknoraLitePin
  archive: Buffer
  root: string
  componentsRoot: string
  dataDir: string
}

/** Builds an archive shaped like WeKnora build-lite-headless.sh output and the matching pin. */
function buildFixture(mutate?: (stageDir: string) => void): Fixture {
  const root = tmpDir('a24-lite-')
  const stageName = `WeKnora-lite-headless_0.0.0-test_${SOURCE_SHA.slice(0, 12)}_darwin_arm64`
  const stage = path.join(root, 'build', stageName)
  fs.mkdirSync(path.join(stage, 'dict'), { recursive: true })
  fs.mkdirSync(path.join(stage, 'migrations', 'sqlite'), { recursive: true })
  fs.writeFileSync(path.join(stage, 'WeKnora-lite'), '#!/bin/sh\necho lite\n', { mode: 0o755 })
  fs.writeFileSync(path.join(stage, 'LICENSE'), 'MIT License\n\ntest\n')
  fs.writeFileSync(path.join(stage, 'THIRD_PARTY_NOTICES.md'), '# notices\n')
  fs.writeFileSync(path.join(stage, 'dict', 'jieba.dict.utf8'), 'x 1 n\n')
  fs.writeFileSync(path.join(stage, 'migrations', 'sqlite', '000001_init.up.sql'), 'select 1;\n')
  mutate?.(stage)
  const archivePath = path.join(root, 'build', `${stageName}.tar.gz`)
  execFileSync('tar', ['-czf', archivePath, '-C', path.join(root, 'build'), stageName])
  const archive = fs.readFileSync(archivePath)
  const pin: WeknoraLitePin = {
    sourceSha: SOURCE_SHA,
    version: '0.0.0-test',
    os: 'darwin',
    arch: 'arm64',
    archive: `${stageName}.tar.gz`,
    archiveSha256: sha256(archive),
    archiveSize: archive.length,
    binarySha256: sha256(fs.readFileSync(path.join(stage, 'WeKnora-lite'))),
    license: 'MIT License',
    licenseSha256: sha256(fs.readFileSync(path.join(stage, 'LICENSE'))),
    noticesSha256: sha256(fs.readFileSync(path.join(stage, 'THIRD_PARTY_NOTICES.md'))),
    url: null,
  }
  const componentsRoot = path.join(root, 'home', 'components', 'weknora-lite')
  const dataDir = path.join(root, 'home', 'data', 'weknora-lite')
  fs.mkdirSync(dataDir, { recursive: true })
  fs.writeFileSync(path.join(dataDir, 'managed-secrets.json'), '{"k":"v"}')
  return { pin, archive, root, componentsRoot, dataDir }
}

/** Serves `body(n)` for the n-th request (0-based) at /<archive>. */
async function serve(handler: (req: http.IncomingMessage, res: http.ServerResponse, n: number) => void): Promise<{ base: string; hits: () => number }> {
  let n = 0
  const server = http.createServer((req, res) => handler(req, res, n++))
  servers.push(server)
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
  const { port } = server.address() as AddressInfo
  return { base: `http://127.0.0.1:${port}/lite/`, hits: () => n }
}

function installer(f: Fixture, base: string | null, extra: Partial<ConstructorParameters<typeof WeknoraLiteInstaller>[0]> = {}): WeknoraLiteInstaller {
  return new WeknoraLiteInstaller({
    pin: f.pin,
    componentsRoot: f.componentsRoot,
    platform: 'darwin',
    arch: 'arm64',
    env: base ? { [WEKNORA_LITE_LOCAL_BASE_URL_ENV]: base } : {},
    idleTimeoutMs: 2_000,
    ...extra,
  })
}

describe('pin', () => {
  it('the shipped pin is a real, well-formed pin for darwin/arm64 with no public URL', () => {
    expect(pinProblem(WEKNORA_LITE_PIN)).toBeNull()
    expect(WEKNORA_LITE_PIN.os).toBe('darwin')
    expect(WEKNORA_LITE_PIN.arch).toBe('arm64')
    expect(WEKNORA_LITE_PIN.url).toBeNull()
  })

  it('rejects placeholder and malformed pins', () => {
    const f = buildFixture()
    expect(pinProblem(f.pin)).toBeNull()
    expect(pinProblem({ ...f.pin, archiveSha256: 'PIN_PENDING' })).not.toBeNull()
    expect(pinProblem({ ...f.pin, sourceSha: 'abc' })).not.toBeNull()
    expect(pinProblem({ ...f.pin, archiveSize: 0 })).not.toBeNull()
    expect(pinProblem({ ...f.pin, archive: '../x.tar.gz' })).not.toBeNull()
    expect(pinProblem({ ...f.pin, archive: 'x.zip' })).not.toBeNull()
  })
})

describe('WeknoraLiteInstaller availability', () => {
  it('is unavailable when the pin has no URL and no local override is set', () => {
    const f = buildFixture()
    const status = installer(f, null).status()
    expect(status.state).toBe('unavailable')
    expect(status.reason).toBeTruthy()
  })

  it('is unavailable when the pin is for another platform/arch', () => {
    const f = buildFixture()
    expect(installer(f, 'http://127.0.0.1:1/', { arch: 'x64' }).status().state).toBe('unavailable')
    expect(installer(f, 'http://127.0.0.1:1/', { platform: 'linux' }).status().state).toBe('unavailable')
  })

  it('ignores a non-loopback override (stays unavailable without a pin URL)', () => {
    const f = buildFixture()
    expect(installer(f, 'http://localhost:8765/').status().state).toBe('unavailable')
    expect(installer(f, 'https://example.com/').status().state).toBe('unavailable')
  })

  it('starts as not-installed with the archive size when a source is configured', () => {
    const f = buildFixture()
    const status = installer(f, 'http://127.0.0.1:1/').status()
    expect(status).toMatchObject({ state: 'not-installed', total: f.pin.archiveSize, overrideActive: true })
  })
})

describe('WeknoraLiteInstaller install', () => {
  it('downloads, verifies and installs into <sha7>-darwin-arm64 with progress, leaving the data dir alone', async () => {
    const f = buildFixture()
    const { base } = await serve((req, res) => {
      expect(req.url).toBe(`/lite/${f.pin.archive}`)
      res.writeHead(200, { 'content-length': String(f.archive.length) })
      res.end(f.archive)
    })
    const inst = installer(f, base)
    const seen: WeknoraLiteStatus[] = []
    inst.onProgress((s) => seen.push(s))
    const final = await inst.install()

    const dir = weknoraLiteInstallDir(f.componentsRoot, f.pin)
    expect(dir.endsWith(path.join('weknora-lite', 'abcdef0-darwin-arm64'))).toBe(true)
    expect(final).toMatchObject({ state: 'installed', dir, binaryPath: path.join(dir, 'WeKnora-lite') })
    expect(seen.some((s) => s.state === 'downloading' && s.total === f.pin.archiveSize)).toBe(true)
    expect(seen.some((s) => s.state === 'verifying')).toBe(true)
    // program files flattened into the install dir, executable kept, tree read-only
    expect(fs.readFileSync(path.join(dir, 'LICENSE'), 'utf8')).toContain('MIT')
    expect(fs.existsSync(path.join(dir, 'dict', 'jieba.dict.utf8'))).toBe(true)
    expect(fs.statSync(path.join(dir, 'WeKnora-lite')).mode & 0o100).toBe(0o100)
    expect(fs.statSync(path.join(dir, 'WeKnora-lite')).mode & 0o222).toBe(0)
    expect(fs.statSync(dir).mode & 0o222).toBe(0)
    // no scratch left behind, data dir untouched
    expect(fs.readdirSync(f.componentsRoot).filter((e) => e.startsWith('.'))).toEqual([])
    expect(fs.readFileSync(path.join(f.dataDir, 'managed-secrets.json'), 'utf8')).toBe('{"k":"v"}')
    expect(verifyInstalledWeknoraLiteBinary(dir, f.pin)).toBe(path.join(dir, 'WeKnora-lite'))
  })

  it('a new installer instance sees the existing install and does not download again', async () => {
    const f = buildFixture()
    const { base, hits } = await serve((_req, res) => res.end(f.archive))
    expect((await installer(f, base).install()).state).toBe('installed')
    const again = installer(f, base)
    expect(again.status().state).toBe('installed')
    expect((await again.install()).state).toBe('installed')
    expect(hits()).toBe(1)
  })

  it('a different pinned archive sha256 treats the old install as not installed', async () => {
    const f = buildFixture()
    const { base } = await serve((_req, res) => res.end(f.archive))
    await installer(f, base).install()
    const other = new WeknoraLiteInstaller({
      pin: { ...f.pin, archiveSha256: 'f'.repeat(64) },
      componentsRoot: f.componentsRoot, platform: 'darwin', arch: 'arm64',
      env: { [WEKNORA_LITE_LOCAL_BASE_URL_ENV]: base },
    })
    expect(other.status().state).toBe('not-installed')
  })

  it('fails as network when the connection drops mid-download, then a retry installs', async () => {
    const f = buildFixture()
    const { base, hits } = await serve((_req, res, n) => {
      res.writeHead(200, { 'content-length': String(f.archive.length) })
      if (n === 0) {
        res.write(f.archive.subarray(0, Math.floor(f.archive.length / 2)))
        setTimeout(() => res.socket?.destroy(), 20)
        return
      }
      res.end(f.archive)
    })
    const inst = installer(f, base)
    const first = await inst.install()
    expect(first.state).toBe('failed')
    expect(first.failure).toBe('network')
    expect(fs.existsSync(weknoraLiteInstallDir(f.componentsRoot, f.pin))).toBe(false)
    const second = await inst.install()
    expect(second.state).toBe('installed')
    expect(hits()).toBe(2)
  })

  it('fails as network on an HTTP error and when the source is unreachable', async () => {
    const f = buildFixture()
    const { base } = await serve((_req, res) => { res.writeHead(404); res.end() })
    expect(await installer(f, base).install()).toMatchObject({ state: 'failed', failure: 'network' })
    expect(await installer(f, 'http://127.0.0.1:9/').install()).toMatchObject({ state: 'failed', failure: 'network' })
  })

  it('fails as network after the idle timeout when the server stalls', async () => {
    const f = buildFixture()
    const { base } = await serve((_req, res) => {
      res.writeHead(200, { 'content-length': String(f.archive.length) })
      res.write(f.archive.subarray(0, 10))
    })
    const result = await installer(f, base, { idleTimeoutMs: 300 }).install()
    expect(result).toMatchObject({ state: 'failed', failure: 'network' })
  })

  it('rejects a swapped archive of the same size as integrity and installs nothing', async () => {
    const f = buildFixture()
    const tampered = Buffer.from(f.archive)
    tampered[tampered.length - 1] ^= 0xff
    const { base } = await serve((_req, res) => res.end(tampered))
    const result = await installer(f, base).install()
    expect(result).toMatchObject({ state: 'failed', failure: 'integrity' })
    expect(fs.existsSync(weknoraLiteInstallDir(f.componentsRoot, f.pin))).toBe(false)
    expect(fs.readdirSync(f.componentsRoot).filter((e) => e.startsWith('.'))).toEqual([])
  })

  it('rejects a body larger than the pinned size as integrity', async () => {
    const f = buildFixture()
    const { base } = await serve((_req, res) => res.end(Buffer.concat([f.archive, Buffer.from('extra')])))
    expect(await installer(f, base).install()).toMatchObject({ state: 'failed', failure: 'integrity' })
  })

  it('binds the program, LICENSE and notices hashes to the pin', async () => {
    for (const field of ['binarySha256', 'licenseSha256', 'noticesSha256'] as const) {
      const f = buildFixture()
      const pin = { ...f.pin, [field]: '0'.repeat(64) }
      const { base } = await serve((_req, res) => res.end(f.archive))
      const result = await new WeknoraLiteInstaller({
        pin, componentsRoot: f.componentsRoot, platform: 'darwin', arch: 'arm64',
        env: { [WEKNORA_LITE_LOCAL_BASE_URL_ENV]: base },
      }).install()
      expect(result, field).toMatchObject({ state: 'failed', failure: 'integrity' })
      expect(fs.existsSync(weknoraLiteInstallDir(f.componentsRoot, pin))).toBe(false)
    }
  })

  it('rejects an archive whose top-level dir does not match the pinned archive name (other os/arch/sha)', async () => {
    const f = buildFixture()
    const renamed = { ...f.pin, archive: f.pin.archive.replace('darwin_arm64', 'linux_amd64') }
    const { base } = await serve((_req, res) => res.end(f.archive))
    const result = await new WeknoraLiteInstaller({
      pin: renamed, componentsRoot: f.componentsRoot, platform: 'darwin', arch: 'arm64',
      env: { [WEKNORA_LITE_LOCAL_BASE_URL_ENV]: base },
    }).install()
    expect(result).toMatchObject({ state: 'failed', failure: 'integrity' })
  })

  it('rejects an archive containing a symlink', async () => {
    const f = buildFixture((stage) => fs.symlinkSync('/etc/passwd', path.join(stage, 'dict', 'evil')))
    const { base } = await serve((_req, res) => res.end(f.archive))
    expect(await installer(f, base).install()).toMatchObject({ state: 'failed', failure: 'integrity' })
  })

  it('does not follow a redirect away from the loopback override', async () => {
    const f = buildFixture()
    const { base } = await serve((_req, res) => { res.writeHead(302, { location: 'http://example.com/x.tar.gz' }); res.end() })
    const result = await installer(f, base).install()
    expect(result.state).toBe('failed')
  })

  it('refuses a non-https pinned URL', async () => {
    const f = buildFixture()
    const result = await new WeknoraLiteInstaller({
      pin: { ...f.pin, url: `http://example.com/${f.pin.archive}` },
      componentsRoot: f.componentsRoot, platform: 'darwin', arch: 'arm64', env: {},
    }).install()
    expect(result.state).not.toBe('installed')
  })

  it('abort() ends an in-flight install as failed and a later install() succeeds', async () => {
    const f = buildFixture()
    const { base } = await serve((_req, res, n) => {
      res.writeHead(200, { 'content-length': String(f.archive.length) })
      if (n === 0) { res.write(f.archive.subarray(0, 10)); return }
      res.end(f.archive)
    })
    const inst = installer(f, base)
    const pending = inst.install()
    await new Promise((r) => setTimeout(r, 100))
    inst.abort('quit')
    expect((await pending).state).toBe('failed')
    expect((await inst.install()).state).toBe('installed')
  })

  it('concurrent install() calls share one job', async () => {
    const f = buildFixture()
    const { base, hits } = await serve((_req, res) => res.end(f.archive))
    const inst = installer(f, base)
    const [a, b] = await Promise.all([inst.install(), inst.install()])
    expect(a.state).toBe('installed')
    expect(b.state).toBe('installed')
    expect(hits()).toBe(1)
  })

  it('status() demotes to not-installed when the install dir is deleted', async () => {
    const f = buildFixture()
    const { base } = await serve((_req, res) => res.end(f.archive))
    const inst = installer(f, base)
    await inst.install()
    const dir = weknoraLiteInstallDir(f.componentsRoot, f.pin)
    makeWritable(dir)
    fs.rmSync(dir, { recursive: true, force: true })
    expect(inst.status().state).toBe('not-installed')
  })

  it('verifyInstalledWeknoraLiteBinary throws when the program changed after install', async () => {
    const f = buildFixture()
    const { base } = await serve((_req, res) => res.end(f.archive))
    const final = await installer(f, base).install()
    const bin = final.binaryPath!
    fs.chmodSync(path.dirname(bin), 0o700)
    fs.chmodSync(bin, 0o700)
    fs.writeFileSync(bin, '#!/bin/sh\necho changed\n')
    expect(() => verifyInstalledWeknoraLiteBinary(final.dir!, f.pin)).toThrow()
  })
})
