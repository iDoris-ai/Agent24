// T1-1: installing the documents module the mac app carries, at launch.
import { createHash } from 'node:crypto'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  MARKER_FILE,
  bundleMatchesSums,
  ensureBundledDocumentsModule,
  execCli,
  manifestVersion,
  packagesRootFor,
  type BundledModuleDeps,
  type CliResult,
} from './documents-module'

let root: string
beforeEach(() => {
  root = fs.mkdtempSync(path.join(process.env['TMPDIR'] ?? os.tmpdir(), 'docmod-'))
})
afterEach(() => {
  fs.rmSync(root, { recursive: true, force: true })
})

const manifest = (version: string): string =>
  `# comment\nname: documents\nversion: "${version}"\nspawn:\n  command: bin/agent24-documents\n`

/** A staged package as scripts/stage-documents-module.sh lays it out. */
function bundle(version = '0.2.0'): string {
  const dir = path.join(root, 'bundle')
  fs.mkdirSync(path.join(dir, 'bin'), { recursive: true })
  const files: Record<string, string> = {
    'domain-os.yml': manifest(version),
    'bin/agent24-documents': 'module program',
    'bin/agent24-documents-pdfkit': 'engine program',
  }
  let sums = ''
  for (const [rel, body] of Object.entries(files)) {
    fs.writeFileSync(path.join(dir, rel), body)
    sums += `${createHash('sha256').update(body).digest('hex')}  ${rel}\n`
  }
  fs.writeFileSync(path.join(dir, 'SHA256SUMS'), sums)
  return dir
}

function installed(version: string | null): string {
  const pkgs = path.join(root, 'data', 'packages')
  fs.mkdirSync(path.join(pkgs, 'documents'), { recursive: true })
  if (version !== null) fs.writeFileSync(path.join(pkgs, 'documents', 'domain-os.yml'), manifest(version))
  return pkgs
}

function deps(over: Partial<BundledModuleDeps> & { results?: CliResult[] } = {}) {
  const results = [...(over.results ?? [])]
  const runCli = vi.fn(async (_args: string[]) => results.shift() ?? { code: 0, output: 'installed /x/documents\n' })
  const warn = vi.fn()
  const d: BundledModuleDeps = {
    bundleDir: over.bundleDir === undefined ? bundle() : over.bundleDir,
    packagesRoot: over.packagesRoot === undefined ? path.join(root, 'data', 'packages') : over.packagesRoot,
    userDataDir: over.userDataDir ?? root,
    runCli,
    warn,
    info: vi.fn(),
  }
  return { d, runCli, warn }
}

describe('ensureBundledDocumentsModule', () => {
  it('not installed → os install <bundle>', async () => {
    const { d, runCli, warn } = deps()
    expect(await ensureBundledDocumentsModule(d)).toBe('installed')
    expect(runCli.mock.calls).toEqual([[['os', 'install', d.bundleDir]]])
    expect(warn).not.toHaveBeenCalled()
  })

  it('installed at the same version → nothing runs', async () => {
    const { d, runCli } = deps({ packagesRoot: installed('0.2.0') })
    expect(await ensureBundledDocumentsModule(d)).toBe('up_to_date')
    expect(runCli).not.toHaveBeenCalled()
  })

  it('installed at another version → os uninstall, then os install', async () => {
    const { d, runCli } = deps({ packagesRoot: installed('0.1.0') })
    expect(await ensureBundledDocumentsModule(d)).toBe('upgraded')
    expect(runCli.mock.calls).toEqual([[['os', 'uninstall', 'documents']], [['os', 'install', d.bundleDir]]])
  })

  it('the uninstall of an upgrade fails → no install, a warning, no throw', async () => {
    const { d, runCli, warn } = deps({ packagesRoot: installed('0.1.0'), results: [{ code: 1, output: 'consent store unavailable\n' }] })
    expect(await ensureBundledDocumentsModule(d)).toBe('install_failed')
    expect(runCli).toHaveBeenCalledTimes(1)
    expect(warn.mock.calls[0]![0]).toMatch(/os uninstall failed \(rc=1\): consent store unavailable/)
  })

  it('the user uninstalled it → left uninstalled', async () => {
    fs.writeFileSync(path.join(root, MARKER_FILE), JSON.stringify({ uninstalledByUser: true, at: '2026-11-03T00:00:00Z' }))
    const { d, runCli } = deps()
    expect(await ensureBundledDocumentsModule(d)).toBe('skipped_user_uninstalled')
    expect(runCli).not.toHaveBeenCalled()
  })

  it('a marker that does not say uninstalledByUser: true does not stop the install', async () => {
    fs.writeFileSync(path.join(root, MARKER_FILE), '{"uninstalledByUser": false}')
    const { d } = deps()
    expect(await ensureBundledDocumentsModule(d)).toBe('installed')
    fs.writeFileSync(path.join(root, MARKER_FILE), 'not json')
    expect(await ensureBundledDocumentsModule(deps().d)).toBe('installed')
  })

  it('CLI rc≠0 → install_failed with the last line of its output, never throws', async () => {
    const { d, warn } = deps({ results: [{ code: 2, output: 'line one\nrefusing: packages root is group-writable\n' }] })
    expect(await ensureBundledDocumentsModule(d)).toBe('install_failed')
    expect(warn.mock.calls[0]![0]).toMatch(/os install failed \(rc=2\): refusing: packages root is group-writable/)
  })

  it('a CLI that rejects → install_failed, never throws', async () => {
    const { d, runCli } = deps()
    runCli.mockRejectedValueOnce(new Error('spawn ENOENT'))
    expect(await ensureBundledDocumentsModule(d)).toBe('install_failed')
  })

  it('no bundle (not packaged, or not staged) → nothing runs', async () => {
    expect(await ensureBundledDocumentsModule(deps({ bundleDir: null }).d)).toBe('no_bundle')
    const empty = path.join(root, 'empty')
    fs.mkdirSync(empty)
    const { d, runCli } = deps({ bundleDir: empty })
    expect(await ensureBundledDocumentsModule(d)).toBe('no_bundle')
    expect(runCli).not.toHaveBeenCalled()
  })

  it('a bundle that does not match SHA256SUMS is not installed', async () => {
    const dir = bundle()
    fs.appendFileSync(path.join(dir, 'bin', 'agent24-documents'), 'tampered')
    const { d, runCli, warn } = deps({ bundleDir: dir })
    expect(await ensureBundledDocumentsModule(d)).toBe('bundle_invalid')
    expect(runCli).not.toHaveBeenCalled()
    expect(warn).toHaveBeenCalledTimes(1)
  })

  it('an installed directory without a readable manifest is left alone', async () => {
    const { d, runCli } = deps({ packagesRoot: installed(null) })
    expect(await ensureBundledDocumentsModule(d)).toBe('installed_unreadable')
    expect(runCli).not.toHaveBeenCalled()
  })

  it('no packages root → nothing runs', async () => {
    const { d, runCli } = deps({ packagesRoot: null })
    expect(await ensureBundledDocumentsModule(d)).toBe('no_packages_root')
    expect(runCli).not.toHaveBeenCalled()
  })
})

describe('bundleMatchesSums', () => {
  it('needs the manifest and the module program listed', () => {
    const dir = bundle()
    expect(bundleMatchesSums(dir)).toBe(true)
    const sums = fs.readFileSync(path.join(dir, 'SHA256SUMS'), 'utf8').split('\n')
    fs.writeFileSync(path.join(dir, 'SHA256SUMS'), sums.filter((l) => !l.endsWith('bin/agent24-documents')).join('\n'))
    expect(bundleMatchesSums(dir)).toBe(false)
  })

  it('refuses paths outside the package and malformed lines', () => {
    const dir = bundle()
    const good = fs.readFileSync(path.join(dir, 'SHA256SUMS'), 'utf8')
    fs.writeFileSync(path.join(dir, 'SHA256SUMS'), `${good}${'0'.repeat(64)}  ../outside\n`)
    expect(bundleMatchesSums(dir)).toBe(false)
    fs.writeFileSync(path.join(dir, 'SHA256SUMS'), `${good}garbage\n`)
    expect(bundleMatchesSums(dir)).toBe(false)
  })
})

describe('manifestVersion', () => {
  it('reads the top-level version, quoted or not', () => {
    expect(manifestVersion('name: documents\nversion: "0.1.0"\n')).toBe('0.1.0')
    expect(manifestVersion("version: '1.2.3' # pinned\n")).toBe('1.2.3')
    expect(manifestVersion('version: 2.0.0\n')).toBe('2.0.0')
    expect(manifestVersion('spawn:\n  version: 9\n')).toBeNull()
    expect(manifestVersion('version: ""\n')).toBeNull()
  })
})

describe('packagesRootFor', () => {
  it('A24_OS_PACKAGES wins, as for the CLI and the daemon; else <data dir>/packages', () => {
    expect(packagesRootFor({ A24_OS_PACKAGES: '/p' }, '/d')).toBe('/p')
    expect(packagesRootFor({}, '/d')).toBe(path.join('/d', 'packages'))
    expect(packagesRootFor({}, null)).toBeNull()
    expect(packagesRootFor({ A24_OS_PACKAGES: '' }, '/d')).toBeNull()
  })
})

describe('execCli', () => {
  it('gives the exit code and output, and never rejects', async () => {
    const sh = '/bin/sh'
    expect(await execCli(sh)(['-c', 'echo hi'])).toEqual({ code: 0, output: 'hi\n' })
    const failed = await execCli(sh)(['-c', 'echo bad >&2; exit 3'])
    expect(failed.code).toBe(3)
    expect(failed.output).toBe('bad\n')
    const missing = await execCli(path.join(root, 'no-such-binary'))([])
    expect(missing.code).toBeNull()
  })

  it('stops a call past its timeout', async () => {
    const slow = await execCli('/bin/sh', 100)(['-c', 'sleep 5'])
    expect(slow.code).toBeNull()
  })
})
