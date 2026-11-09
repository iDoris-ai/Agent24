// T1-1: installing the documents module the mac app carries, at launch.
import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  CODESIGN,
  MARKER_FILE,
  documentsModuleOnDisk,
  ensureBundledDocumentsModule,
  installBundledDocumentsModule,
  uninstallDocumentsModule,
  userUninstalled,
  execCli,
  manifestVersion,
  packagesRootFor,
  parseIdentity,
  sameFiles,
  signatureProblem,
  type BundledModuleDeps,
  type CliResult,
} from './documents-module'

let root: string
beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'docmod-'))
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
  fs.writeFileSync(path.join(dir, 'domain-os.yml'), manifest(version))
  fs.writeFileSync(path.join(dir, 'bin', 'agent24-documents'), 'module program')
  fs.writeFileSync(path.join(dir, 'bin', 'agent24-documents-pdfkit'), 'engine program')
  return dir
}

function installed(version: string | null): string {
  const pkgs = path.join(root, 'data', 'packages')
  fs.mkdirSync(path.join(pkgs, 'documents'), { recursive: true })
  if (version !== null) fs.writeFileSync(path.join(pkgs, 'documents', 'domain-os.yml'), manifest(version))
  return pkgs
}

/** What `os install` leaves: the bundle copied file by file. */
function installedCopy(src: string): string {
  const pkgs = path.join(root, 'data', 'packages')
  fs.mkdirSync(pkgs, { recursive: true })
  fs.cpSync(src, path.join(pkgs, 'documents'), { recursive: true })
  return pkgs
}

const TEAM_APP = 'Executable=/A/Agent24.app/Contents/MacOS/Agent24\nTeamIdentifier=HYAP444FN9\n'
const ADHOC_APP = 'Executable=/A/Agent24.app/Contents/MacOS/Agent24\nSignature=adhoc\nTeamIdentifier=not set\n'
const RUNTIME = 'CodeDirectory v=20500 size=1 flags=0x10000(runtime) hashes=1+7 location=embedded\n'

/** A codesign that answers `-dv` for the app with `app`, `-dv` for a program
 * with `program`, and every --verify with `verify` (in that order of calls). */
function fakeCodesign(app = TEAM_APP, program = RUNTIME, verify: CliResult = { code: 0, output: '' }) {
  return vi.fn(async (args: string[]): Promise<CliResult> => {
    if (args[0] === '-dv') return { code: 0, output: args[1]!.endsWith('.app') ? app : program }
    return verify
  })
}

function deps(over: Partial<BundledModuleDeps> & { results?: CliResult[] } = {}) {
  const results = [...(over.results ?? [])]
  const runCli = vi.fn(async (args: string[]) => args[0] === 'package-integrity'
    ? { code: 0, output: '' }
    : results.shift() ?? { code: 0, output: 'installed /x/documents\n' })
  const codesign = over.codesign ?? fakeCodesign()
  const warn = vi.fn()
  const d: BundledModuleDeps = {
    bundleDir: over.bundleDir === undefined ? bundle() : over.bundleDir,
    appBundle: over.appBundle === undefined ? '/A/Agent24.app' : over.appBundle,
    codesign,
    packagesRoot: over.packagesRoot === undefined ? path.join(root, 'data', 'packages') : over.packagesRoot,
    userDataDir: over.userDataDir ?? root,
    runCli,
    warn,
    info: vi.fn(),
  }
  return { d, runCli, warn, codesign }
}

function expectInstallCalls(calls: string[][][], upgrading = false) {
  const tmp = calls[0]?.[0]?.[3]
  expect(tmp).toEqual(expect.any(String))
  expect(calls).toEqual([
    [['package-integrity', 'generate', '--root', tmp, '--executable', 'bin/agent24-documents', '--executable', 'bin/agent24-documents-pdfkit']],
    ...(upgrading ? [[['os', 'uninstall', 'documents']]] : []),
    [['os', 'install', tmp]],
  ])
  expect(fs.existsSync(tmp!)).toBe(false)
}

describe('ensureBundledDocumentsModule', () => {
  it('not installed → signatures checked, generate, then os install <copy>', async () => {
    const { d, runCli, warn, codesign } = deps()
    expect(await ensureBundledDocumentsModule(d)).toBe('installed')
    expectInstallCalls(runCli.mock.calls)
    expect(codesign).toHaveBeenCalled()
    expect(warn).not.toHaveBeenCalled()
  })

  it('copies into a private temporary directory after signature checks, then cleans up', async () => {
    const { d, runCli, codesign } = deps()
    runCli.mockImplementationOnce(async (args) => {
      expect(codesign).toHaveBeenCalledTimes(6)
      const tmp = args[3]!
      expect(tmp).not.toBe(d.bundleDir)
      expect(fs.statSync(tmp).mode & 0o777).toBe(0o700)
      expect(sameFiles(d.bundleDir!, tmp)).toBe(true)
      fs.writeFileSync(path.join(tmp, 'files.json'), '{}')
      return { code: 0, output: '' }
    })
    expect(await ensureBundledDocumentsModule(d)).toBe('installed')
    expectInstallCalls(runCli.mock.calls)
    expect(fs.existsSync(path.join(d.bundleDir!, 'files.json'))).toBe(false)
  })

  it.each([false, true])('generate fails → no install or uninstall; cleanup (upgrade=%s)', async (upgrade) => {
    const { d, runCli, warn } = deps(upgrade ? { packagesRoot: installed('0.1.0') } : {})
    runCli.mockResolvedValueOnce({ code: 1, output: 'generation failed' })
    expect(await ensureBundledDocumentsModule(d)).toBe('install_failed')
    expect(runCli).toHaveBeenCalledTimes(1)
    expect(runCli.mock.calls[0]![0].slice(0, 2)).toEqual(['package-integrity', 'generate'])
    expect(fs.existsSync(runCli.mock.calls[0]![0][3]!)).toBe(false)
    expect(warn.mock.calls[0]![0]).toContain('generation failed')
  })

  it('generated metadata and YAML formatting do not trigger reinstall; payload bytes still do', async () => {
    const b = bundle()
    const pkgs = installedCopy(b)
    const copy = path.join(pkgs, 'documents')
    fs.writeFileSync(path.join(copy, 'files.json'), '{}')
    fs.writeFileSync(path.join(copy, 'domain-os.yml'), "name: documents\nversion: 0.2.0\nintegrity:\n  manifest: files.json\n  sha256: abc\nspawn: {command: bin/agent24-documents}\n")
    const { d, runCli } = deps({ bundleDir: b, packagesRoot: pkgs })
    expect(await ensureBundledDocumentsModule(d)).toBe('up_to_date')
    expect(runCli).not.toHaveBeenCalled()
    fs.writeFileSync(path.join(copy, 'bin/agent24-documents-pdfkit'), 'Engine program')
    expect(await ensureBundledDocumentsModule(d)).toBe('upgraded')
    expectInstallCalls(runCli.mock.calls, true)
  })

  it("logs the CLI's `installed <dir>` line, not its trailing hint", async () => {
    const out = 'installed /d/packages/documents\n  it takes effect at the next daemon start\n  (a daemon started implicitly…)\n'
    const { d } = deps({ results: [{ code: 0, output: out }] })
    await ensureBundledDocumentsModule(d)
    expect(vi.mocked(d.info).mock.calls[0]![0]).toBe('[documents-module] installed documents 0.2.0: installed /d/packages/documents')
  })

  it('installed and the same files → nothing runs, not even codesign', async () => {
    const b = bundle()
    const { d, runCli, codesign } = deps({ bundleDir: b, packagesRoot: installedCopy(b) })
    expect(await ensureBundledDocumentsModule(d)).toBe('up_to_date')
    expect(runCli).not.toHaveBeenCalled()
    expect(codesign).not.toHaveBeenCalled()
  })

  it('the same version but a program differs (the 11-02 candidate) → os uninstall, then os install', async () => {
    const b = bundle('0.1.0')
    const pkgs = installedCopy(b)
    fs.writeFileSync(path.join(b, 'bin', 'agent24-documents'), 'module program, rebuilt')
    const { d, runCli } = deps({ bundleDir: b, packagesRoot: pkgs })
    expect(await ensureBundledDocumentsModule(d)).toBe('upgraded')
    expectInstallCalls(runCli.mock.calls, true)
  })

  it('compares the directory the CLI installs into: `..` after a symlink is not folded', async () => {
    const b = bundle()
    fs.mkdirSync(path.join(root, 'real', 'sub'), { recursive: true })
    fs.symlinkSync(path.join(root, 'real', 'sub'), path.join(root, 'link'))
    // As PathBuf::join: <root>/link/../packages is <root>/real/packages.
    fs.cpSync(b, path.join(root, 'real', 'packages', 'documents'), { recursive: true })
    const { d, runCli } = deps({ bundleDir: b, packagesRoot: `${root}/link/../packages` })
    expect(await ensureBundledDocumentsModule(d)).toBe('up_to_date')
    expect(runCli).not.toHaveBeenCalled()
  })

  it('a file only on one side → replaced', async () => {
    const b = bundle()
    const pkgs = installedCopy(b)
    fs.writeFileSync(path.join(pkgs, 'documents', 'bin', 'extra'), 'x')
    expect(await ensureBundledDocumentsModule(deps({ bundleDir: b, packagesRoot: pkgs }).d)).toBe('upgraded')
    fs.rmSync(path.join(pkgs, 'documents', 'bin', 'extra'))
    fs.rmSync(path.join(pkgs, 'documents', 'bin', 'agent24-documents-pdfkit'))
    expect(await ensureBundledDocumentsModule(deps({ bundleDir: b, packagesRoot: pkgs }).d)).toBe('upgraded')
  })

  it('installed at another version → os uninstall, then os install', async () => {
    const { d, runCli } = deps({ packagesRoot: installed('0.1.0') })
    expect(await ensureBundledDocumentsModule(d)).toBe('upgraded')
    expectInstallCalls(runCli.mock.calls, true)
  })

  it('a bad signature on an upgrade → the old module is not uninstalled', async () => {
    const codesign = fakeCodesign(TEAM_APP, RUNTIME, { code: 3, output: 'test-requirement: code failed to satisfy specified code requirement(s)\n' })
    const { d, runCli, warn } = deps({ packagesRoot: installed('0.1.0'), codesign })
    expect(await ensureBundledDocumentsModule(d)).toBe('signature_invalid')
    expect(runCli).not.toHaveBeenCalled()
    expect(warn.mock.calls[0]![0]).toMatch(/not installing the bundled documents 0\.2\.0: .*test-requirement/)
  })

  it('the uninstall of an upgrade fails → no install, a warning, no throw', async () => {
    const { d, runCli, warn } = deps({ packagesRoot: installed('0.1.0'), results: [{ code: 1, output: 'consent store unavailable\n' }] })
    expect(await ensureBundledDocumentsModule(d)).toBe('install_failed')
    expect(runCli).toHaveBeenCalledTimes(2)
    expect(warn.mock.calls[0]![0]).toMatch(/replacing 0\.1\.0 with 0\.2\.0: os uninstall failed \(rc=1\): consent store unavailable/)
  })

  it('the user uninstalled it → left uninstalled', async () => {
    fs.writeFileSync(path.join(root, MARKER_FILE), JSON.stringify({ uninstalledByUser: true, at: '2026-11-03T00:00:00Z' }))
    const { d, runCli } = deps()
    expect(await ensureBundledDocumentsModule(d)).toBe('skipped_user_uninstalled')
    expect(runCli).not.toHaveBeenCalled()
  })

  it('a marker that does not say uninstalledByUser: true does not stop the install', async () => {
    fs.writeFileSync(path.join(root, MARKER_FILE), '{"uninstalledByUser": false}')
    expect(await ensureBundledDocumentsModule(deps().d)).toBe('installed')
    fs.writeFileSync(path.join(root, MARKER_FILE), 'not json')
    expect(await ensureBundledDocumentsModule(deps().d)).toBe('installed')
  })

  it('CLI rc≠0 → install_failed with the last line of its output, never throws', async () => {
    const { d, warn } = deps({ results: [{ code: 2, output: 'line one\nrefusing: packages root is group-writable\n' }] })
    expect(await ensureBundledDocumentsModule(d)).toBe('install_failed')
    expect(fs.existsSync(vi.mocked(d.runCli).mock.calls[0]![0][3]!)).toBe(false)
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

  it('a bundled manifest without a version is not installed', async () => {
    const dir = bundle()
    fs.writeFileSync(path.join(dir, 'domain-os.yml'), 'name: documents\n')
    const { d, runCli } = deps({ bundleDir: dir })
    expect(await ensureBundledDocumentsModule(d)).toBe('bundle_invalid')
    expect(runCli).not.toHaveBeenCalled()
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

describe('signatureProblem', () => {
  const dir = '/A/Agent24.app/Contents/Resources/documents-module'

  it('a team-signed app: each program must satisfy the team requirement and carry the runtime flag', async () => {
    const codesign = fakeCodesign()
    expect(await signatureProblem(dir, '/A/Agent24.app', codesign)).toBeNull()
    const verifies = codesign.mock.calls.map(([a]) => a).filter((a) => a[0] === '--verify')
    expect(verifies).toEqual([
      ['--verify', '--strict', '-R', '=anchor apple generic and certificate leaf[subject.OU] = "HYAP444FN9"', '/A/Agent24.app'],
      ['--verify', '--strict', '-R', '=anchor apple generic and certificate leaf[subject.OU] = "HYAP444FN9"', `${dir}/bin/agent24-documents`],
      ['--verify', '--strict', '-R', '=anchor apple generic and certificate leaf[subject.OU] = "HYAP444FN9"', `${dir}/bin/agent24-documents-pdfkit`],
    ])
  })

  it('a team-signed app: a program without the Hardened Runtime is refused', async () => {
    const noRuntime = 'CodeDirectory v=20400 size=1 flags=0x20002(adhoc,linker-signed) hashes=1+0 location=embedded\n'
    expect(await signatureProblem(dir, '/A/Agent24.app', fakeCodesign(TEAM_APP, noRuntime))).toMatch(/Hardened Runtime/)
  })

  it('an ad-hoc app: a valid signature is enough, with no team requirement', async () => {
    const codesign = fakeCodesign(ADHOC_APP)
    expect(await signatureProblem(dir, '/A/Agent24.app', codesign)).toBeNull()
    const verifies = codesign.mock.calls.map(([a]) => a).filter((a) => a[0] === '--verify')
    expect(verifies).toEqual([
      ['--verify', '--strict', '/A/Agent24.app'],
      ['--verify', '--strict', `${dir}/bin/agent24-documents`],
      ['--verify', '--strict', `${dir}/bin/agent24-documents-pdfkit`],
    ])
  })

  it('an ad-hoc app: an invalid signature on a program is refused', async () => {
    const codesign = vi.fn(async (args: string[]): Promise<CliResult> => {
      if (args[0] === '-dv') return { code: 0, output: ADHOC_APP }
      if (args[args.length - 1]!.endsWith('.app')) return { code: 0, output: '' }
      return { code: 1, output: 'invalid signature (code or signature have been modified)\n' }
    })
    expect(await signatureProblem(dir, '/A/Agent24.app', codesign)).toMatch(/agent24-documents: invalid signature/)
  })

  it("the app's own seal is checked first: a modified manifest beside valid programs is refused", async () => {
    const codesign = vi.fn(async (args: string[]): Promise<CliResult> => {
      if (args[0] === '-dv') return { code: 0, output: TEAM_APP }
      if (args[args.length - 1]!.endsWith('.app')) {
        return { code: 1, output: '/A/Agent24.app: a sealed resource is missing or invalid\nfile modified: …/documents-module/domain-os.yml\n' }
      }
      return { code: 0, output: '' }
    })
    expect(await signatureProblem(dir, '/A/Agent24.app', codesign)).toMatch(/app's signature does not verify: file modified: .*domain-os\.yml/)
    expect(codesign.mock.calls.filter(([a]) => a[0] === '--verify')).toHaveLength(1)
  })

  it("an app whose identity cannot be read installs nothing (not taken for ad-hoc)", async () => {
    const unsigned = vi.fn(async (): Promise<CliResult> => ({ code: 1, output: 'code object is not signed at all\n' }))
    expect(await signatureProblem(dir, '/A/Agent24.app', unsigned)).toMatch(/could not be read: code object is not signed at all/)
    expect(unsigned).toHaveBeenCalledTimes(1)
    expect(await signatureProblem(dir, null, fakeCodesign())).toMatch(/unknown/)
  })
})

describe('parseIdentity', () => {
  it('reads a team ID or ad-hoc; anything else is unknown', () => {
    expect(parseIdentity(TEAM_APP)).toEqual({ kind: 'team', team: 'HYAP444FN9' })
    expect(parseIdentity(ADHOC_APP)).toEqual({ kind: 'adhoc' })
    expect(parseIdentity('Signature=adhoc\n')).toBeNull()
    expect(parseIdentity('TeamIdentifier=not set\n')).toBeNull()
    expect(parseIdentity('')).toBeNull()
  })
})

describe('signatureProblem against the real codesign (macOS)', () => {
  it.runIf(process.platform === 'darwin')('a sealed ad-hoc app passes; a modified program or manifest does not', async () => {
    const app = path.join(root, 'X.app')
    const dir = path.join(app, 'Contents', 'Resources', 'documents-module')
    fs.mkdirSync(path.join(dir, 'bin'), { recursive: true })
    fs.writeFileSync(path.join(dir, 'domain-os.yml'), manifest('0.2.0'))
    for (const name of ['agent24-documents', 'agent24-documents-pdfkit']) {
      fs.copyFileSync('/usr/bin/true', path.join(dir, 'bin', name))
      execFileSync(CODESIGN, ['--force', '--sign', '-', path.join(dir, 'bin', name)])
    }
    fs.mkdirSync(path.join(app, 'Contents', 'MacOS'), { recursive: true })
    fs.copyFileSync('/usr/bin/true', path.join(app, 'Contents', 'MacOS', 'X'))
    fs.writeFileSync(
      path.join(app, 'Contents', 'Info.plist'),
      '<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>X</string><key>CFBundleIdentifier</key><string>t.x</string></dict></plist>',
    )
    execFileSync(CODESIGN, ['--force', '--sign', '-', app])
    expect(await signatureProblem(dir, app, execCli(CODESIGN))).toBeNull()
    fs.appendFileSync(path.join(dir, 'domain-os.yml'), 'args: ["-c", "evil"]\n')
    expect(await signatureProblem(dir, app, execCli(CODESIGN))).toMatch(/app's signature does not verify/)
    execFileSync(CODESIGN, ['--force', '--sign', '-', app]) // reseal with the new manifest
    expect(await signatureProblem(dir, app, execCli(CODESIGN))).toBeNull()
    fs.appendFileSync(path.join(dir, 'bin', 'agent24-documents-pdfkit'), 'x')
    expect(await signatureProblem(dir, app, execCli(CODESIGN))).toMatch(/does not verify|agent24-documents-pdfkit/)
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
    expect(packagesRootFor({}, '/d')).toBe('/d/packages')
    // Joined literally like Rust's PathBuf::join: `..` is not folded.
    expect(packagesRootFor({}, '/x/link/../d')).toBe('/x/link/../d/packages')
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

// T1-2: uninstall / install again from the Modules page.
describe('uninstallDocumentsModule', () => {
  const NOW = new Date('2026-11-03T12:00:00.000Z')
  function udeps(result: CliResult = { code: 0, output: 'removed documents\n  it takes effect at the next daemon start\n' }) {
    const runCli = vi.fn(async (_args: string[]) => result)
    const warn = vi.fn()
    return { u: { userDataDir: root, runCli, warn, info: vi.fn(), now: () => NOW }, runCli, warn }
  }
  const marker = (): unknown => JSON.parse(fs.readFileSync(path.join(root, MARKER_FILE), 'utf8'))

  it('writes the marker, then runs os uninstall documents', async () => {
    const { u, runCli } = udeps()
    runCli.mockImplementationOnce(async () => {
      // Marked before anything is removed.
      expect(userUninstalled(root)).toBe(true)
      return { code: 0, output: 'removed documents\n' }
    })
    expect(await uninstallDocumentsModule(u)).toEqual({ ok: true })
    expect(runCli.mock.calls).toEqual([[['os', 'uninstall', 'documents']]])
    expect(marker()).toEqual({ uninstalledByUser: true, at: '2026-11-03T12:00:00.000Z' })
  })

  it('the next launch then leaves it uninstalled', async () => {
    const { u } = udeps()
    await uninstallDocumentsModule(u)
    const { d, runCli } = deps()
    expect(await ensureBundledDocumentsModule(d)).toBe('skipped_user_uninstalled')
    expect(runCli).not.toHaveBeenCalled()
  })

  it('CLI fails → its rc and last line come back, and the marker is taken back', async () => {
    const { u, warn } = udeps({ code: 2, output: 'error: consent revocation failed: store locked\n' })
    const r = await uninstallDocumentsModule(u)
    expect(r).toEqual({ ok: false, code: 'cli_failed', message: 'agent24 os uninstall documents (rc=2): error: consent revocation failed: store locked' })
    expect(fs.existsSync(path.join(root, MARKER_FILE))).toBe(false)
    expect(warn).toHaveBeenCalledOnce()
  })

  it('CLI fails with a marker already there → the marker stays', async () => {
    fs.writeFileSync(path.join(root, MARKER_FILE), '{"uninstalledByUser": true}')
    const { u } = udeps({ code: 1, output: 'busy\n' })
    expect((await uninstallDocumentsModule(u)).ok).toBe(false)
    expect(userUninstalled(root)).toBe(true)
  })

  it('the marker cannot be written → nothing is uninstalled', async () => {
    const blocked = path.join(root, 'not-a-dir')
    fs.writeFileSync(blocked, 'a file where userData should be')
    const { u, runCli } = udeps()
    const r = await uninstallDocumentsModule({ ...u, userDataDir: blocked })
    expect(r).toMatchObject({ ok: false, code: 'marker_failed' })
    expect(runCli).not.toHaveBeenCalled()
  })
})

describe('installBundledDocumentsModule', () => {
  it('checks signatures, generates integrity, installs the copy, then clears the marker', async () => {
    fs.writeFileSync(path.join(root, MARKER_FILE), '{"uninstalledByUser": true}')
    const { d, runCli, codesign } = deps()
    expect(await installBundledDocumentsModule(d)).toEqual({ ok: true })
    expect(codesign).toHaveBeenCalled()
    expectInstallCalls(runCli.mock.calls)
    expect(fs.existsSync(path.join(root, MARKER_FILE))).toBe(false)
  })

  it('a signature that does not pass → no install, marker kept', async () => {
    fs.writeFileSync(path.join(root, MARKER_FILE), '{"uninstalledByUser": true}')
    const { d, runCli } = deps({ codesign: fakeCodesign(TEAM_APP, RUNTIME, { code: 1, output: 'a sealed resource is missing or invalid' }) })
    expect(await installBundledDocumentsModule(d)).toMatchObject({ ok: false, code: 'signature_invalid' })
    expect(runCli).not.toHaveBeenCalled()
    expect(userUninstalled(root)).toBe(true)
  })

  it('CLI fails → its rc and last line come back, marker kept', async () => {
    fs.writeFileSync(path.join(root, MARKER_FILE), '{"uninstalledByUser": true}')
    const { d } = deps({ results: [{ code: 1, output: 'error: consent revocation failed\n' }] })
    expect(await installBundledDocumentsModule(d)).toEqual({ ok: false, code: 'cli_failed', message: 'agent24 os install (rc=1): error: consent revocation failed' })
    expect(userUninstalled(root)).toBe(true)
  })

  it('generate fails → marker kept, no install, temporary copy removed', async () => {
    fs.writeFileSync(path.join(root, MARKER_FILE), '{"uninstalledByUser": true}')
    const { d, runCli } = deps()
    runCli.mockRejectedValueOnce(new Error('generate could not run'))
    expect(await installBundledDocumentsModule(d)).toEqual({ ok: false, code: 'cli_failed', message: 'generate could not run' })
    expect(runCli).toHaveBeenCalledTimes(1)
    expect(fs.existsSync(runCli.mock.calls[0]![0][3]!)).toBe(false)
    expect(userUninstalled(root)).toBe(true)
  })

  it('already installed → refused before any check', async () => {
    const pkgs = installed('0.1.0')
    const { d, runCli, codesign } = deps({ packagesRoot: pkgs })
    expect(await installBundledDocumentsModule(d)).toMatchObject({ ok: false, code: 'already_installed' })
    expect(codesign).not.toHaveBeenCalled()
    expect(runCli).not.toHaveBeenCalled()
  })

  it('no bundle, or no packages root → refused', async () => {
    expect(await installBundledDocumentsModule(deps({ bundleDir: null }).d)).toMatchObject({ ok: false, code: 'no_bundle' })
    expect(await installBundledDocumentsModule(deps({ packagesRoot: null }).d)).toMatchObject({ ok: false, code: 'no_packages_root' })
  })
})

describe('documentsModuleOnDisk', () => {
  it('bundled needs a readable version; installed is any entry under the name', () => {
    expect(documentsModuleOnDisk(null, null)).toEqual({ bundled: false, installed: false })
    const b = bundle()
    const pkgs = path.join(root, 'data', 'packages')
    expect(documentsModuleOnDisk(b, pkgs)).toEqual({ bundled: true, installed: false })
    installed(null)
    expect(documentsModuleOnDisk(b, pkgs)).toEqual({ bundled: true, installed: true })
    fs.writeFileSync(path.join(b, 'domain-os.yml'), 'name: documents\n')
    expect(documentsModuleOnDisk(b, pkgs).bundled).toBe(false)
  })

  it('a dangling symlink counts as installed, as the CLI counts it', () => {
    const pkgs = path.join(root, 'pk')
    fs.mkdirSync(pkgs)
    fs.symlinkSync(path.join(root, 'gone'), path.join(pkgs, 'documents'))
    expect(documentsModuleOnDisk(null, pkgs).installed).toBe(true)
  })
})

describe('sameFiles', () => {
  it.each([
    'name: documents\nversion: 0.2.0\nspawn: {command: bin/other}\n',
    'name: documents\nversion: 0.2.0\nversion: 0.2.0\nspawn: {command: bin/agent24-documents}\n',
    'name: documents\nversion: 0.2.0\n---\nname: documents\n',
    'name: documents\nversion: 0.2.0\n1: value\n',
  ])('does not ignore business changes or invalid YAML: %s', (text) => {
    const b = bundle()
    const copy = path.join(installedCopy(b), 'documents')
    fs.writeFileSync(path.join(copy, 'domain-os.yml'), text)
    expect(sameFiles(b, copy)).toBe(false)
  })

  it('entries and bytes, not modes; a symlink is never the same', () => {
    const b = bundle()
    const copy = path.join(root, 'copy')
    fs.cpSync(b, copy, { recursive: true })
    fs.chmodSync(path.join(copy, 'bin', 'agent24-documents'), 0o755)
    expect(sameFiles(b, copy)).toBe(true)
    fs.mkdirSync(path.join(copy, 'empty'))
    expect(sameFiles(b, copy)).toBe(false)
    fs.rmdirSync(path.join(copy, 'empty'))
    fs.rmSync(path.join(copy, 'bin', 'agent24-documents'))
    fs.symlinkSync(path.join(b, 'bin', 'agent24-documents'), path.join(copy, 'bin', 'agent24-documents'))
    expect(sameFiles(b, copy)).toBe(false)
    expect(sameFiles(b, path.join(root, 'missing'))).toBe(false)
  })
})
