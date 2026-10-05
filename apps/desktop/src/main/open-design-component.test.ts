import { execFileSync } from 'node:child_process'
import crypto from 'node:crypto'
import fs from 'node:fs'
import http from 'node:http'
import type { AddressInfo } from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'

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
  vi.restoreAllMocks()
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

function writeMarker(dir: string, sha256: string): void {
  fs.writeFileSync(path.join(dir, '.agent24-component.json'), JSON.stringify({ sha256 }))
}

function buildTarball(stageDir: string): string {
  const tarPath = path.join(path.dirname(stageDir), `${path.basename(stageDir)}.tar.gz`)
  execFileSync('tar', ['-czf', tarPath, '-C', stageDir, 'app', 'open-design', 'open-design-web-standalone'])
  tempDirs.push(tarPath)
  return tarPath
}

/** Builds a tarball that is otherwise valid but also carries one member
 * renamed (via tar's own `-s`/transform option, portable across GNU tar and
 * macOS's bsdtar) to a path-traversal name — exercises the pre-extraction
 * `tar -tzf` safety check (hardening on top of the post-extraction symlink
 * walk, which only inspects symlinks, not regular-file path traversal). */
function buildTarballWithTraversalEntry(stageDir: string): string {
  const extraDir = tmpDir('a24-od-traversal-extra-')
  fs.writeFileSync(path.join(extraDir, 'evil.txt'), 'nope')
  const tarPath = path.join(path.dirname(stageDir), `${path.basename(stageDir)}-traversal.tar.gz`)
  execFileSync('tar', [
    '-czf', tarPath,
    '-s', '#^evil.txt$#../../escape.txt#',
    '-C', stageDir, 'app', 'open-design', 'open-design-web-standalone',
    '-C', extraDir, 'evil.txt',
  ])
  tempDirs.push(tarPath)
  return tarPath
}

function seedTreeFor(prefix: string): string {
  const root = tmpDir(prefix)
  seedValidTree(root)
  return root
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

/** Server that 302-redirects `/redirect` to `/asset.tar.gz` on the same
 * host/port (for the L3 manual-redirect-following tests), or to an
 * arbitrary `Location` override when provided. */
async function startRedirectingServer(filePath: string, locationOverride?: (selfOrigin: string) => string): Promise<{ url: string }> {
  const server = http.createServer((req, res) => {
    if (req.url === '/redirect') {
      const { port: selfPort } = server.address() as AddressInfo
      const location = locationOverride
        ? locationOverride(`http://127.0.0.1:${selfPort}`)
        : `https://127.0.0.1:${selfPort}/asset.tar.gz`
      res.writeHead(302, { Location: location })
      res.end()
      return
    }
    const body = fs.readFileSync(filePath)
    res.writeHead(200, { 'Content-Type': 'application/octet-stream', 'Content-Length': String(body.length) })
    res.end(body)
  })
  servers.push(server)
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
  const { port } = server.address() as AddressInfo
  return { url: `http://127.0.0.1:${port}/redirect` }
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
      url: 'https://github.com/iDoris-ai/Agent24/releases/download/v0.6.0/open-design-0.22.2-f84ff89-linux-x64-abc123456789.tar.gz',
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

  it('L1: degrades a corrupt (truncated/unparsable) baked manifest to null instead of throwing', () => {
    const resources = tmpDir('a24-od-manifest-corrupt-')
    fs.writeFileSync(path.join(resources, 'open-design-component.json'), '{ this is not json')
    vi.spyOn(console, 'error').mockImplementation(() => {})
    expect(() => loadOpenDesignComponentManifest(resources)).not.toThrow()
    expect(loadOpenDesignComponentManifest(resources)).toBeNull()
  })
})

describe('verifyComponentContract', () => {
  it('accepts a valid extracted tree', () => {
    const root = tmpDir('a24-od-contract-valid-')
    seedValidTree(root)
    expect(() => verifyComponentContract(root, '0.22.2')).not.toThrow()
  })

  it('M1: accepts a legitimate relative symlink when root is reached through a symlink (os.tmpdir() is one on macOS)', () => {
    const root = tmpDir('a24-od-contract-tmpdir-symlink-')
    seedValidTree(root)
    fs.writeFileSync(path.join(root, 'open-design/target.txt'), 'ok')
    fs.symlinkSync('target.txt', path.join(root, 'open-design/link.txt'))
    expect(() => verifyComponentContract(root, '0.22.2')).not.toThrow()
    expect(fs.readlinkSync(path.join(root, 'open-design/link.txt'))).toBe('target.txt')
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

  it('M1: rejects a symlink whose immediate target is itself another symlink ("crosses another symlink")', () => {
    const root = tmpDir('a24-od-contract-chain-')
    seedValidTree(root)
    fs.mkdirSync(path.join(root, 'open-design/real'))
    fs.writeFileSync(path.join(root, 'open-design/real/target.txt'), 'ok')
    fs.symlinkSync('real', path.join(root, 'open-design/alias'))
    fs.symlinkSync('alias/target.txt', path.join(root, 'open-design/link.txt'))
    expect(() => verifyComponentContract(root, '0.22.2')).toThrow('crosses another symlink')
  })

  it('quickValidateInstalledComponent checks required files AND the marker sha256 (H2)', () => {
    const root = tmpDir('a24-od-quick-')
    expect(quickValidateInstalledComponent(root, 'a'.repeat(64))).toBe(false)
    seedValidTree(root)
    expect(quickValidateInstalledComponent(root, 'a'.repeat(64))).toBe(false) // no marker yet
    writeMarker(root, 'a'.repeat(64))
    expect(quickValidateInstalledComponent(root, 'a'.repeat(64))).toBe(true)
    expect(quickValidateInstalledComponent(root, 'b'.repeat(64))).toBe(false) // sha changed (ABI bump)
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

  it('L2: treats a manifest built for a different platform/arch as unavailable', () => {
    const { manifest } = manifestFor(buildTarball(seedTreeFor('a24-od-stage-abimismatch-')), { platform: 'win32', arch: 'x64' })
    vi.spyOn(console, 'error').mockImplementation(() => {})
    const installer = new OpenDesignComponentInstaller({ manifest, platform: 'darwin', arch: 'arm64' })
    expect(installer.status()).toEqual({
      state: 'unavailable',
      reason: 'This build does not bundle an Open Design component manifest',
    })
  })

  it('reports installed immediately when the component is already present, valid, and the marker sha256 matches', () => {
    const componentsRoot = tmpDir('a24-od-root-')
    const { manifest } = manifestFor(buildTarball(seedTreeFor('a24-od-stage-already-')))
    const dir = openDesignComponentInstallDir(componentsRoot, manifest)
    fs.mkdirSync(dir, { recursive: true })
    seedValidTree(dir)
    writeMarker(dir, manifest.sha256)
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot })
    expect(installer.status()).toEqual({ state: 'installed', dir })
  })

  it('H2: demotes a stale install (marker sha256 mismatch, e.g. an Electron ABI bump) to not-installed', () => {
    const componentsRoot = tmpDir('a24-od-root-stale-')
    const { manifest } = manifestFor(buildTarball(seedTreeFor('a24-od-stage-stale-')))
    const dir = openDesignComponentInstallDir(componentsRoot, manifest)
    fs.mkdirSync(dir, { recursive: true })
    seedValidTree(dir)
    writeMarker(dir, 'f'.repeat(64)) // some other, older content's sha256
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot })
    expect(installer.status()).toEqual({ state: 'not-installed', size: manifest.size })
  })

  it('H2: a previously-installed dir deleted out from under the installer self-heals to not-installed on status()', () => {
    const componentsRoot = tmpDir('a24-od-root-deleted-')
    const { manifest } = manifestFor(buildTarball(seedTreeFor('a24-od-stage-deleted-')))
    const dir = openDesignComponentInstallDir(componentsRoot, manifest)
    fs.mkdirSync(dir, { recursive: true })
    seedValidTree(dir)
    writeMarker(dir, manifest.sha256)
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot })
    expect(installer.status().state).toBe('installed')

    fs.rmSync(dir, { recursive: true, force: true })
    expect(installer.status()).toEqual({ state: 'not-installed', size: manifest.size })
  })

  it('M2: sweeps stale .partial-* leftovers from a crashed previous run at construction', () => {
    const componentsRoot = tmpDir('a24-od-root-partials-')
    fs.mkdirSync(path.join(componentsRoot, '.partial-deadbeef'), { recursive: true })
    fs.writeFileSync(path.join(componentsRoot, '.partial-deadbeef.tar.gz'), '')
    const { manifest } = manifestFor(buildTarball(seedTreeFor('a24-od-stage-partials-')))
    // eslint-disable-next-line no-new
    new OpenDesignComponentInstaller({ manifest, componentsRoot })
    expect(fs.readdirSync(componentsRoot)).toEqual([])
  })

  it('downloads, verifies, and installs a good tarball', async () => {
    const stage = seedTreeFor('a24-od-stage-good-')
    const tarPath = buildTarball(stage)
    const { manifest } = manifestFor(tarPath)
    const { url } = await startServer(tarPath)
    manifest.url = httpsManifestUrl(url)

    const componentsRoot = tmpDir('a24-od-root-good-')
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot, allowedHosts: ['127.0.0.1'] }, localFetch)
    expect(installer.status()).toEqual({ state: 'not-installed', size: manifest.size })

    const progressStates: string[] = []
    installer.onProgress((status) => progressStates.push(status.state))

    const result = await installer.install()
    const expectedDir = openDesignComponentInstallDir(componentsRoot, manifest)
    expect(result).toEqual({ state: 'installed', dir: expectedDir })
    expect(quickValidateInstalledComponent(expectedDir, manifest.sha256)).toBe(true)
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
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot, allowedHosts: ['127.0.0.1'] }, localFetch)
    const result = await installer.install()
    expect(result.state).toBe('failed')
    expect(result.error).toContain('sha256 mismatch')
    expect(fs.readdirSync(componentsRoot)).toEqual([])
  })

  it('aborts a download that exceeds the manifest-declared size', async () => {
    const stage = seedTreeFor('a24-od-stage-oversize-')
    const tarPath = buildTarball(stage)
    const { manifest } = manifestFor(tarPath, { size: 10 })
    const { url } = await startServer(tarPath)
    manifest.url = httpsManifestUrl(url)

    const componentsRoot = tmpDir('a24-od-root-oversize-')
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot, allowedHosts: ['127.0.0.1'] }, localFetch)
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
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot, allowedHosts: ['127.0.0.1'] }, localFetch)
    const result = await installer.install()
    expect(result.state).toBe('failed')
    expect(result.error).toContain('absolute symlink')
    expect(fs.readdirSync(componentsRoot)).toEqual([])
  })

  it('hardening: rejects a tarball member whose path contains ".." before extracting anything', async () => {
    const stage = seedTreeFor('a24-od-stage-traversal-')
    const tarPath = buildTarballWithTraversalEntry(stage)
    const { manifest } = manifestFor(tarPath)
    const { url } = await startServer(tarPath)
    manifest.url = httpsManifestUrl(url)

    const componentsRoot = tmpDir('a24-od-root-traversal-')
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot, allowedHosts: ['127.0.0.1'] }, localFetch)
    const result = await installer.install()
    expect(result.state).toBe('failed')
    expect(result.error).toContain('unsafe path entry')
    expect(fs.readdirSync(componentsRoot)).toEqual([])
  })

  it('shares a single in-flight download across concurrent install() calls', async () => {
    const stage = seedTreeFor('a24-od-stage-concurrent-')
    const tarPath = buildTarball(stage)
    const { manifest } = manifestFor(tarPath)
    const { url, requestCount } = await startServer(tarPath)
    manifest.url = httpsManifestUrl(url)

    const componentsRoot = tmpDir('a24-od-root-concurrent-')
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot, allowedHosts: ['127.0.0.1'] }, localFetch)
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
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot, allowedHosts: ['127.0.0.1'] }, trackedFetch)
    const result = await installer.install()
    expect(result.state).toBe('failed')
    expect(result.error).toContain('HTTPS')
    expect(called).toBe(false)
  })

  it('L3: rejects a host that is not on the (real, default) allowlist before any network call', async () => {
    const stage = seedTreeFor('a24-od-stage-disallowed-host-')
    const tarPath = buildTarball(stage)
    const { manifest } = manifestFor(tarPath)
    const { url } = await startServer(tarPath)
    manifest.url = httpsManifestUrl(url) // https://127.0.0.1:<port>/asset.tar.gz — not allow-listed by default

    const componentsRoot = tmpDir('a24-od-root-disallowed-host-')
    let called = false
    const trackedFetch: typeof fetch = (...args) => { called = true; return localFetch(...args) }
    // No `allowedHosts` override here — exercises the real production default.
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot }, trackedFetch)
    const result = await installer.install()
    expect(result.state).toBe('failed')
    expect(result.error).toContain('not allow-listed')
    expect(called).toBe(false)
  })

  it('L3: follows an allow-listed https redirect hop before downloading', async () => {
    const stage = seedTreeFor('a24-od-stage-redirect-ok-')
    const tarPath = buildTarball(stage)
    const { manifest } = manifestFor(tarPath)
    const { url } = await startRedirectingServer(tarPath)
    manifest.url = httpsManifestUrl(url)

    const componentsRoot = tmpDir('a24-od-root-redirect-ok-')
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot, allowedHosts: ['127.0.0.1'] }, localFetch)
    const result = await installer.install()
    expect(result.state).toBe('installed')
  })

  it('L3: rejects a redirect to a non-HTTPS target', async () => {
    const stage = seedTreeFor('a24-od-stage-redirect-http-')
    const tarPath = buildTarball(stage)
    const { manifest } = manifestFor(tarPath)
    const { url } = await startRedirectingServer(tarPath, () => 'http://127.0.0.1:9/asset.tar.gz')
    manifest.url = httpsManifestUrl(url)

    const componentsRoot = tmpDir('a24-od-root-redirect-http-')
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot, allowedHosts: ['127.0.0.1'] }, localFetch)
    const result = await installer.install()
    expect(result.state).toBe('failed')
    expect(result.error).toContain('HTTPS')
  })

  it('L3: rejects a redirect to a disallowed host', async () => {
    const stage = seedTreeFor('a24-od-stage-redirect-host-')
    const tarPath = buildTarball(stage)
    const { manifest } = manifestFor(tarPath)
    const { url } = await startRedirectingServer(tarPath, () => 'https://evil.example/asset.tar.gz')
    manifest.url = httpsManifestUrl(url)

    const componentsRoot = tmpDir('a24-od-root-redirect-host-')
    const installer = new OpenDesignComponentInstaller({ manifest, componentsRoot, allowedHosts: ['127.0.0.1'] }, localFetch)
    const result = await installer.install()
    expect(result.state).toBe('failed')
    expect(result.error).toContain('not allow-listed')
  })

  it('M5: idle-timeout aborts a stalled download and leaves it retryable', async () => {
    const stage = seedTreeFor('a24-od-stage-idle-')
    const tarPath = buildTarball(stage)
    const { manifest } = manifestFor(tarPath)
    const bytes = fs.readFileSync(tarPath)

    let hangRequest = true
    const server = http.createServer((_req, res) => {
      res.writeHead(200, { 'Content-Type': 'application/octet-stream', 'Content-Length': String(bytes.length) })
      if (hangRequest) {
        // Write a prefix, then just never send the rest for this request —
        // the installer's idle timer (set very small below) must fire.
        res.write(bytes.subarray(0, 1))
        return
      }
      res.end(bytes)
    })
    servers.push(server)
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
    const { port } = server.address() as AddressInfo
    manifest.url = httpsManifestUrl(`http://127.0.0.1:${port}/asset.tar.gz`)

    const componentsRoot = tmpDir('a24-od-root-idle-')
    const installer = new OpenDesignComponentInstaller(
      { manifest, componentsRoot, allowedHosts: ['127.0.0.1'], idleTimeoutMs: 50 },
      localFetch,
    )
    const result = await installer.install()
    expect(result.state).toBe('failed')
    expect(result.error).toContain('idle-timed-out')
    expect(fs.readdirSync(componentsRoot)).toEqual([])

    // Retryable: a fresh install() against a server that now responds fully succeeds.
    hangRequest = false
    const retried = await installer.install()
    expect(retried.state).toBe('installed')
  })

  it('M5: abort() cancels an in-flight install and leaves it retryable', async () => {
    const stage = seedTreeFor('a24-od-stage-abort-')
    const tarPath = buildTarball(stage)
    const { manifest } = manifestFor(tarPath)
    const bytes = fs.readFileSync(tarPath)

    const server = http.createServer((_req, res) => {
      res.writeHead(200, { 'Content-Type': 'application/octet-stream', 'Content-Length': String(bytes.length) })
      // Write one byte, then just hold the connection open — the test only
      // needs the download to be observably in flight before calling abort().
      res.write(bytes.subarray(0, 1))
    })
    servers.push(server)
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
    const { port } = server.address() as AddressInfo
    manifest.url = httpsManifestUrl(`http://127.0.0.1:${port}/asset.tar.gz`)

    const componentsRoot = tmpDir('a24-od-root-abort-')
    const installer = new OpenDesignComponentInstaller(
      { manifest, componentsRoot, allowedHosts: ['127.0.0.1'] },
      localFetch,
    )
    const pending = installer.install()
    await vi.waitFor(() => expect(installer.status().state).toBe('downloading'))
    installer.abort('test aborted it')
    const result = await pending
    expect(result.state).toBe('failed')
    expect(result.error).toContain('test aborted it')
    expect(fs.readdirSync(componentsRoot)).toEqual([])
  })
})
