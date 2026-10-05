import { execFileSync } from 'node:child_process'
import crypto from 'node:crypto'
import fs from 'node:fs'
import http from 'node:http'
import type { AddressInfo } from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'

import {
  OpenDesignComponentInstaller,
  loadOpenDesignComponentManifest,
  openDesignComponentInstallDir,
  parseOpenDesignComponentManifest,
  quickValidateInstalledComponent,
  verifyComponentContract,
  type OpenDesignComponentManifest,
} from './open-design-component'

const tempDirs: string[] = []
const servers: http.Server[] = []

afterEach(async () => {
  for (const server of servers.splice(0)) await new Promise((resolve) => server.close(resolve))
  for (const dir of tempDirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true })
})

function tmpDir(prefix: string): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix))
  tempDirs.push(dir)
  return dir
}

function seedValidTree(root: string): void {
  for (const relative of ['app/prebundled/daemon', 'open-design', 'open-design-web-standalone']) {
    fs.mkdirSync(path.join(root, relative), { recursive: true })
  }
  fs.writeFileSync(path.join(root, 'app/package.json'), JSON.stringify({ version: '0.22.2' }))
  for (const relative of [
    'app/prebundled/agent24-headless.cjs',
    'app/prebundled/agent24-headless.mjs',
    'app/prebundled/daemon/daemon-cli.mjs',
    'app/prebundled/daemon/daemon-sidecar.mjs',
    'app/prebundled/web-sidecar.mjs',
  ]) fs.writeFileSync(path.join(root, relative), '')
  fs.writeFileSync(path.join(root, 'open-design-web-standalone/server.js'), '')
}

function buildTarball(stageDir: string): string {
  const tarPath = path.join(path.dirname(stageDir), `${path.basename(stageDir)}.tar.gz`)
  execFileSync('tar', ['-czf', tarPath, '-C', stageDir, 'app', 'open-design', 'open-design-web-standalone'])
  tempDirs.push(tarPath)
  return tarPath
}

async function startServer(filePath: string, onRequest?: () => void): Promise<{ url: string; requestCount: () => number }> {
  let requestCount = 0
  const server = http.createServer((_req, res) => {
    requestCount += 1
    onRequest?.()
    const body = fs.readFileSync(filePath)
    res.writeHead(200, { 'Content-Type': 'application/octet-stream', 'Content-Length': String(body.length) })
    res.end(body)
  })
  servers.push(server)
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
  const { port } = server.address() as AddressInfo
  return { url: `http://127.0.0.1:${port}/asset.tar.gz`, requestCount: () => requestCount }
}

// The manifest URL must declare https:// (production enforces it before any
// network call). The fetchFn below simulates "this would be objects.
// githubusercontent.com over TLS in prod" by rewriting back to the local
// plaintext fixture server — a normal shim for testing an https-only guard
// without standing up real TLS.
function httpsManifestUrl(httpUrl: string): string {
  return httpUrl.replace(/^http:/, 'https:')
}
const localFetch: typeof fetch = (input, init) =>
  fetch(String(input).replace(/^https:/, 'http:'), init)

function manifestFor(tarPath: string, overrides: Partial<OpenDesignComponentManifest> = {}): { manifest: OpenDesignComponentManifest; sha256: string; size: number } {
  const bytes = fs.readFileSync(tarPath)
  const sha256 = crypto.createHash('sha256').update(bytes).digest('hex')
  const manifest: OpenDesignComponentManifest = {
    version: '0.22.2',
    odSha: 'f84ff89656143b5fa3fe8f0891b7f2bf38769d9f',
    platform: 'darwin',
    arch: 'arm64',
    url: 'https://example.invalid/placeholder.tar.gz',
    sha256,
    size: bytes.length,
    ...overrides,
  }
  return { manifest, sha256, size: bytes.length }
}

describe('open-design-component manifest parsing', () => {
  it('round-trips a well-formed manifest file', () => {
    const resources = tmpDir('a24-od-manifest-')
    const manifest: OpenDesignComponentManifest = {
      version: '0.22.2',
      odSha: 'f84ff89656143b5fa3fe8f0891b7f2bf38769d9f',
      platform: 'linux',
      arch: 'x64',
      url: 'https://github.com/iDoris-ai/Agent24/releases/download/v0.6.0/open-design-0.22.2-f84ff89-linux-x64.tar.gz',
      sha256: 'a'.repeat(64),
      size: 123,
    }
    fs.writeFileSync(path.join(resources, 'open-design-component.json'), JSON.stringify(manifest))
    expect(loadOpenDesignComponentManifest(resources)).toEqual(manifest)
  })

  it('returns null (unavailable) when no manifest was baked', () => {
    const resources = tmpDir('a24-od-manifest-missing-')
    expect(loadOpenDesignComponentManifest(resources)).toBeNull()
    expect(loadOpenDesignComponentManifest(undefined)).toBeNull()
  })

  it('rejects a malformed manifest', () => {
    expect(() => parseOpenDesignComponentManifest({ version: '0.22.2' })).toThrow('odSha')
    expect(() => parseOpenDesignComponentManifest({
      version: '0.22.2', odSha: 'f84ff89656143b5fa3fe8f0891b7f2bf38769d9f', platform: 'linux', arch: 'x64',
      url: 'https://x', sha256: 'not-a-hash', size: 1,
    })).toThrow('sha256')
  })
})

describe('verifyComponentContract', () => {
  it('accepts a valid extracted tree', () => {
    const root = tmpDir('a24-od-contract-valid-')
    seedValidTree(root)
    expect(() => verifyComponentContract(root, '0.22.2')).not.toThrow()
  })

  it('rejects a version mismatch', () => {
    const root = tmpDir('a24-od-contract-version-')
    seedValidTree(root)
    expect(() => verifyComponentContract(root, '9.9.9')).toThrow('version mismatch')
  })

  it('rejects an absolute symlink', () => {
    const root = tmpDir('a24-od-contract-abs-link-')
    seedValidTree(root)
    fs.symlinkSync('/etc/passwd', path.join(root, 'open-design/escape'))
    expect(() => verifyComponentContract(root, '0.22.2')).toThrow('absolute symlink')
  })

  it('rejects a symlink that resolves outside the resource trees', () => {
    const root = tmpDir('a24-od-contract-escape-')
    seedValidTree(root)
    fs.writeFileSync(path.join(root, 'outside.txt'), 'nope')
    fs.symlinkSync('../outside.txt', path.join(root, 'open-design/escape'))
    expect(() => verifyComponentContract(root, '0.22.2')).toThrow('escapes')
  })

  it('quickValidateInstalledComponent only checks required files, cheaply', () => {
    const root = tmpDir('a24-od-quick-')
    expect(quickValidateInstalledComponent(root)).toBe(false)
    seedValidTree(root)
    expect(quickValidateInstalledComponent(root)).toBe(true)
  })
})

describe('OpenDesignComponentInstaller', () => {
  it('reports unavailable when no manifest is baked (dev / unreleased build)', () => {
    const installer = new OpenDesignComponentInstaller({ manifest: null })
    expect(installer.status()).toEqual({
      state: 'unavailable',
      reason: 'This build does not bundle an Open Design component manifest',
    })
  })

  it('reports installed immediately when the component is already present and valid', () => {
    const componentsRoot = tmpDir('a24-od-root-')
    const { manifest } = manifestFor(buildTarball(seedTreeFor('a24-od-stage-already-')))
    const dir = openDesignComponentInstallDir(componentsRoot, manifest)
    fs.mkdirSync(dir, { recursive: true })
    seedValidTree(dir)
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot })
    expect(installer.status()).toEqual({ state: 'installed', dir })
  })

  it('downloads, verifies, and installs a good tarball', async () => {
    const stage = seedTreeFor('a24-od-stage-good-')
    const tarPath = buildTarball(stage)
    const { manifest } = manifestFor(tarPath)
    const { url } = await startServer(tarPath)
    manifest.url = httpsManifestUrl(url)

    const componentsRoot = tmpDir('a24-od-root-good-')
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot }, localFetch)
    expect(installer.status()).toEqual({ state: 'not-installed', size: manifest.size })

    const progressStates: string[] = []
    installer.onProgress((status) => progressStates.push(status.state))

    const result = await installer.install()
    const expectedDir = openDesignComponentInstallDir(componentsRoot, manifest)
    expect(result).toEqual({ state: 'installed', dir: expectedDir })
    expect(quickValidateInstalledComponent(expectedDir)).toBe(true)
    expect(progressStates).toContain('downloading')
    expect(progressStates).toContain('verifying')
    expect(progressStates[progressStates.length - 1]).toBe('installed')

    // No leftover partial directories/files.
    const leftovers = fs.readdirSync(componentsRoot).filter((name) => name.startsWith('.partial-'))
    expect(leftovers).toEqual([])
  })

  it('rejects a wrong sha256 and leaves nothing behind', async () => {
    const stage = seedTreeFor('a24-od-stage-badsha-')
    const tarPath = buildTarball(stage)
    const { manifest } = manifestFor(tarPath, { sha256: 'b'.repeat(64) })
    const { url } = await startServer(tarPath)
    manifest.url = httpsManifestUrl(url)

    const componentsRoot = tmpDir('a24-od-root-badsha-')
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot }, localFetch)
    const result = await installer.install()
    expect(result.state).toBe('failed')
    expect(result.error).toContain('sha256 mismatch')
    expect(fs.readdirSync(componentsRoot)).toEqual([])
  })

  it('aborts a download that exceeds the manifest-declared size', async () => {
    const stage = seedTreeFor('a24-od-stage-oversize-')
    const tarPath = buildTarball(stage)
    const { manifest, size } = manifestFor(tarPath, { size: 10 })
    void size
    const { url } = await startServer(tarPath)
    manifest.url = httpsManifestUrl(url)

    const componentsRoot = tmpDir('a24-od-root-oversize-')
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot }, localFetch)
    const result = await installer.install()
    expect(result.state).toBe('failed')
    expect(result.error).toContain('exceeded the manifest-declared size')
    expect(fs.readdirSync(componentsRoot)).toEqual([])
  })

  it('rejects a tarball containing a symlink that escapes the resource trees', async () => {
    const stage = seedTreeFor('a24-od-stage-escape-')
    // An absolute symlink target is unambiguously forbidden regardless of extraction layout.
    fs.symlinkSync('/etc/passwd', path.join(stage, 'open-design/escape'))
    const tarPath = buildTarball(stage)
    const { manifest } = manifestFor(tarPath)
    const { url } = await startServer(tarPath)
    manifest.url = httpsManifestUrl(url)

    const componentsRoot = tmpDir('a24-od-root-escape-')
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot }, localFetch)
    const result = await installer.install()
    expect(result.state).toBe('failed')
    expect(result.error).toContain('absolute symlink')
    expect(fs.readdirSync(componentsRoot)).toEqual([])
  })

  it('shares a single in-flight download across concurrent install() calls', async () => {
    const stage = seedTreeFor('a24-od-stage-concurrent-')
    const tarPath = buildTarball(stage)
    const { manifest } = manifestFor(tarPath)
    const { url, requestCount } = await startServer(tarPath)
    manifest.url = httpsManifestUrl(url)

    const componentsRoot = tmpDir('a24-od-root-concurrent-')
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot }, localFetch)
    const [first, second] = await Promise.all([installer.install(), installer.install()])
    expect(first).toEqual(second)
    expect(first.state).toBe('installed')
    expect(requestCount()).toBe(1)
  })

  it('rejects a non-https manifest URL before making any network call', async () => {
    const stage = seedTreeFor('a24-od-stage-insecure-')
    const tarPath = buildTarball(stage)
    const { manifest } = manifestFor(tarPath, { url: 'http://example.invalid/asset.tar.gz' })
    const componentsRoot = tmpDir('a24-od-root-insecure-')
    let called = false
    const trackedFetch: typeof fetch = (...args) => { called = true; return localFetch(...args) }
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot }, trackedFetch)
    const result = await installer.install()
    expect(result.state).toBe('failed')
    expect(result.error).toContain('HTTPS')
    expect(called).toBe(false)
  })
})

function seedTreeFor(prefix: string): string {
  const root = tmpDir(prefix)
  seedValidTree(root)
  return root
}
