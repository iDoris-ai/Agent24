import crypto from 'node:crypto'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import zlib from 'node:zlib'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { performOsPackageInstall, type InstallOsPackageDeps } from './os-package-install'

const dirs: string[] = []
function tempDir(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'os-install-'))
  dirs.push(dir)
  return dir
}
afterEach(() => {
  for (const dir of dirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true })
})

function header(name: string, size: number, type: string): Buffer {
  const block = Buffer.alloc(512)
  block.write(name)
  block.write('0000644\0', 100)
  block.write('0000000\0', 108)
  block.write('0000000\0', 116)
  block.write(`${size.toString(8).padStart(11, '0')}\0`, 124)
  block.write('00000000000\0', 136)
  block.write('        ', 148)
  block.write(type, 156)
  block.write('ustar\0', 257)
  block.write('00', 263)
  let sum = 0
  for (const byte of block) sum += byte
  block.write(`${sum.toString(8).padStart(6, '0')}\0 `, 148)
  return block
}

function tarGz(entries: { name: string; type: string; body?: string }[]): Buffer {
  const parts: Buffer[] = []
  for (const entry of entries) {
    const body = Buffer.from(entry.body ?? '')
    const size = entry.type === '5' ? 0 : body.length
    parts.push(header(entry.name, size, entry.type))
    if (size > 0) parts.push(body, Buffer.alloc((512 - (size % 512)) % 512))
  }
  parts.push(Buffer.alloc(1024))
  return zlib.gzipSync(Buffer.concat(parts))
}

const ASSET = 'sin90-0.5.2-macos-arm64.tar.gz'
const release = {
  tag_name: 'v0.5.2',
  assets: [
    { name: ASSET, browser_download_url: `https://github.com/iDoris-ai/Sin90/releases/download/v0.5.2/${ASSET}`, size: 10 },
    { name: 'SHA256SUMS', browser_download_url: 'https://github.com/iDoris-ai/Sin90/releases/download/v0.5.2/SHA256SUMS', size: 80 },
  ],
}

function deps(over: Partial<InstallOsPackageDeps> & { root: string }): InstallOsPackageDeps {
  return {
    platform: 'darwin',
    arch: 'arm64',
    userDataDir: over.root,
    packagesRoot: path.join(over.root, 'packages'),
    runCli: vi.fn(async () => ({ code: 0, output: 'installed sin90' })),
    withDaemonStopped: async (work) => work(),
    leavesDaemonRunning: () => false,
    confirm: async () => true,
    onProgress: () => {},
    fetchJson: async () => release,
    download: async () => {},
    downloadText: async () => '',
    ...over,
  }
}

describe('performOsPackageInstall', () => {
  it('rejects an id that is not registered', async () => {
    const result = await performOsPackageInstall('cos72', deps({ root: tempDir() }))
    expect(result).toEqual({ ok: false, code: 'not_registered', message: 'cos72' })
  })

  it('refuses to install when the archive hash does not match SHA256SUMS', async () => {
    const bytes = tarGz([{ name: 'sin90-0.5.2-macos-arm64/domain-os.yml', type: '0', body: 'version: 0.5.2\n' }])
    const runCli = vi.fn(async () => ({ code: 0, output: '' }))
    const result = await performOsPackageInstall('sin90', deps({
      root: tempDir(),
      runCli,
      download: async (_url, dest) => { fs.writeFileSync(dest, bytes) },
      downloadText: async () => `${'ab'.repeat(32)}  ${ASSET}\n`,
    }))
    expect(result).toMatchObject({ ok: false, code: 'checksum_mismatch' })
    expect(runCli).not.toHaveBeenCalled()
  })

  it('does not download when the same version is already installed', async () => {
    const root = tempDir()
    const pkg = path.join(root, 'packages', 'sin90')
    fs.mkdirSync(pkg, { recursive: true })
    fs.writeFileSync(path.join(pkg, 'domain-os.yml'), 'version: 0.5.2\n')
    const download = vi.fn()
    const result = await performOsPackageInstall('sin90', deps({ root, download }))
    expect(result).toMatchObject({ ok: false, code: 'already_installed' })
    expect(download).not.toHaveBeenCalled()
  })

  it('uninstalls an older package, then installs, and does not write os.json', async () => {
    const root = tempDir()
    const pkg = path.join(root, 'packages', 'sin90')
    fs.mkdirSync(pkg, { recursive: true })
    fs.writeFileSync(path.join(pkg, 'domain-os.yml'), 'version: 0.5.1\n')
    const bytes = tarGz([
      { name: 'sin90-0.5.2-macos-arm64', type: '5' },
      { name: 'sin90-0.5.2-macos-arm64/domain-os.yml', type: '0', body: 'name: sin90\nversion: 0.5.2\n' },
    ])
    const hash = crypto.createHash('sha256').update(bytes).digest('hex')
    const calls: string[][] = []
    const result = await performOsPackageInstall('sin90', deps({
      root,
      runCli: async (args) => {
        calls.push(args)
        if (args[0] === 'os' && args[1] === 'install') {
          expect(fs.readFileSync(path.join(args[2]!, 'domain-os.yml'), 'utf8')).toContain('version: 0.5.2')
        }
        return { code: 0, output: 'ok' }
      },
      download: async (_url, dest) => { fs.writeFileSync(dest, bytes) },
      downloadText: async () => `${hash}  ${ASSET}\n`,
    }))
    expect(result).toEqual({ ok: true, version: '0.5.2', upgraded: true })
    expect(calls.map((args) => args.slice(0, 2))).toEqual([
      ['package-integrity', 'verify'],
      ['os', 'uninstall'],
      ['os', 'install'],
    ])
    expect(calls[0]?.slice(1, 3)).toEqual(['verify', '--root'])
    expect(fs.existsSync(path.join(root, 'os.json'))).toBe(false)
  })

  it.each([
    { label: 'a different name', manifest: 'name: other\nversion: 0.5.2\n' },
    { label: 'a different version', manifest: 'name: sin90\nversion: 0.5.1\n' },
  ])('rejects $label and does not touch the installed package', async ({ manifest }) => {
    const root = tempDir()
    const pkg = path.join(root, 'packages', 'sin90')
    fs.mkdirSync(pkg, { recursive: true })
    fs.writeFileSync(path.join(pkg, 'domain-os.yml'), 'name: sin90\nversion: 0.5.1\n')
    const packed = packageArchive(manifest)
    const runCli = vi.fn(async () => ({ code: 0, output: '' }))
    const result = await performOsPackageInstall('sin90', deps({
      root,
      runCli,
      download: async (_url, dest) => { fs.writeFileSync(dest, packed.bytes) },
      downloadText: async () => packed.sums,
    }))
    expect(result).toMatchObject({ ok: false, code: 'manifest_mismatch' })
    expect(runCli).not.toHaveBeenCalled()
    expect(fs.readFileSync(path.join(pkg, 'domain-os.yml'), 'utf8')).toBe('name: sin90\nversion: 0.5.1\n')
  })

  it('treats a leading v on the manifest version as the release version', async () => {
    const root = tempDir()
    const pkg = path.join(root, 'packages', 'sin90')
    fs.mkdirSync(pkg, { recursive: true })
    fs.writeFileSync(path.join(pkg, 'domain-os.yml'), 'name: sin90\nversion: 0.5.1\n')
    const packed = packageArchive('name: sin90\nversion: "v0.5.2"\n')
    const calls: string[][] = []
    const result = await performOsPackageInstall('sin90', deps({
      root,
      runCli: async (args) => {
        calls.push(args)
        return { code: 0, output: 'ok' }
      },
      download: async (_url, dest) => { fs.writeFileSync(dest, packed.bytes) },
      downloadText: async () => packed.sums,
    }))
    expect(result).toEqual({ ok: true, version: '0.5.2', upgraded: true })
    expect(calls.map((args) => args.slice(0, 2))).toEqual([
      ['package-integrity', 'verify'],
      ['os', 'uninstall'],
      ['os', 'install'],
    ])
  })

  it('does not uninstall when package-integrity verify fails', async () => {
    const root = tempDir()
    const pkg = path.join(root, 'packages', 'sin90')
    fs.mkdirSync(pkg, { recursive: true })
    fs.writeFileSync(path.join(pkg, 'domain-os.yml'), 'name: sin90\nversion: 0.5.1\n')
    const packed = packageArchive('name: sin90\nversion: 0.5.2\n')
    const calls: string[][] = []
    const result = await performOsPackageInstall('sin90', deps({
      root,
      runCli: async (args) => {
        calls.push(args)
        return { code: 1, output: 'hash_mismatch: bin/sin90' }
      },
      download: async (_url, dest) => { fs.writeFileSync(dest, packed.bytes) },
      downloadText: async () => packed.sums,
    }))
    expect(result).toMatchObject({ ok: false, code: 'cli_failed', message: 'hash_mismatch: bin/sin90' })
    expect(calls.map((args) => args.slice(0, 2))).toEqual([['package-integrity', 'verify']])
    expect(fs.readFileSync(path.join(pkg, 'domain-os.yml'), 'utf8')).toContain('version: 0.5.1')
  })

  it('installs the backup when the new package fails, and the old version remains', async () => {
    const root = tempDir()
    const pkg = path.join(root, 'packages', 'sin90')
    fs.mkdirSync(pkg, { recursive: true })
    fs.writeFileSync(path.join(pkg, 'domain-os.yml'), 'name: sin90\nversion: 0.5.1\n')
    const packed = packageArchive('name: sin90\nversion: 0.5.2\n')
    const calls: string[][] = []
    const result = await performOsPackageInstall('sin90', deps({
      root,
      runCli: async (args) => {
        calls.push(args)
        if (args[0] === 'package-integrity') return { code: 0, output: '' }
        if (args[1] === 'uninstall') {
          fs.rmSync(pkg, { recursive: true, force: true })
          return { code: 0, output: 'removed' }
        }
        const manifest = fs.readFileSync(path.join(args[2]!, 'domain-os.yml'), 'utf8')
        if (manifest.includes('version: 0.5.2')) return { code: 1, output: 'signature rejected' }
        fs.cpSync(args[2]!, pkg, { recursive: true })
        return { code: 0, output: 'installed' }
      },
      download: async (_url, dest) => { fs.writeFileSync(dest, packed.bytes) },
      downloadText: async () => packed.sums,
    }))
    expect(result).toMatchObject({ ok: false, code: 'upgrade_failed_rolled_back', message: 'signature rejected' })
    expect(calls.map((args) => args.slice(0, 2))).toEqual([
      ['package-integrity', 'verify'],
      ['os', 'uninstall'],
      ['os', 'install'],
      ['os', 'install'],
    ])
    expect(calls[3]?.[2]).toMatch(/backup$/)
    expect(fs.readFileSync(path.join(pkg, 'domain-os.yml'), 'utf8')).toContain('version: 0.5.1')
  })

  it('reports upgrade_failed_rollback_failed when the backup install fails', async () => {
    const root = tempDir()
    const pkg = path.join(root, 'packages', 'sin90')
    fs.mkdirSync(pkg, { recursive: true })
    fs.writeFileSync(path.join(pkg, 'domain-os.yml'), 'name: sin90\nversion: 0.5.1\n')
    const packed = packageArchive('name: sin90\nversion: 0.5.2\n')
    const result = await performOsPackageInstall('sin90', deps({
      root,
      runCli: async (args) => {
        if (args[0] === 'package-integrity') return { code: 0, output: '' }
        if (args[1] === 'uninstall') {
          fs.rmSync(pkg, { recursive: true, force: true })
          return { code: 0, output: 'removed' }
        }
        const manifest = fs.readFileSync(path.join(args[2]!, 'domain-os.yml'), 'utf8')
        if (manifest.includes('version: 0.5.2')) return { code: 1, output: 'signature rejected' }
        return { code: 1, output: 'backup install failed' }
      },
      download: async (_url, dest) => { fs.writeFileSync(dest, packed.bytes) },
      downloadText: async () => packed.sums,
    }))
    expect(result).toEqual({
      ok: false,
      code: 'upgrade_failed_rollback_failed',
      message: 'signature rejected; backup install failed',
    })
    expect(fs.existsSync(pkg)).toBe(false)
  })
})

function packageArchive(manifest: string): { bytes: Buffer; sums: string } {
  const bytes = tarGz([
    { name: 'sin90-0.5.2-macos-arm64', type: '5' },
    { name: 'sin90-0.5.2-macos-arm64/domain-os.yml', type: '0', body: manifest },
  ])
  const hash = crypto.createHash('sha256').update(bytes).digest('hex')
  return { bytes, sums: `${hash}  ${ASSET}\n` }
}
