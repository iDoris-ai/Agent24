// T1-1: installing the documents module the mac app carries, at launch.
import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  CODESIGN,
  MARKER_FILE,
  ensureBundledDocumentsModule,
  execCli,
  manifestVersion,
  packagesRootFor,
  parseIdentity,
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
  const runCli = vi.fn(async (_args: string[]) => results.shift() ?? { code: 0, output: 'installed /x/documents\n' })
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

describe('ensureBundledDocumentsModule', () => {
  it('not installed → signatures checked, then os install <bundle>', async () => {
    const { d, runCli, warn, codesign } = deps()
    expect(await ensureBundledDocumentsModule(d)).toBe('installed')
    expect(runCli.mock.calls).toEqual([[['os', 'install', d.bundleDir]]])
    expect(codesign).toHaveBeenCalled()
    expect(warn).not.toHaveBeenCalled()
  })

  it('installed at the same version → nothing runs, not even codesign', async () => {
    const { d, runCli, codesign } = deps({ packagesRoot: installed('0.2.0') })
    expect(await ensureBundledDocumentsModule(d)).toBe('up_to_date')
    expect(runCli).not.toHaveBeenCalled()
    expect(codesign).not.toHaveBeenCalled()
  })

  it('installed at another version → os uninstall, then os install', async () => {
    const { d, runCli } = deps({ packagesRoot: installed('0.1.0') })
    expect(await ensureBundledDocumentsModule(d)).toBe('upgraded')
    expect(runCli.mock.calls).toEqual([[['os', 'uninstall', 'documents']], [['os', 'install', d.bundleDir]]])
  })

  it('a bad signature on an upgrade → the old module is not uninstalled', async () => {
    const codesign = fakeCodesign(TEAM_APP, RUNTIME, { code: 3, output: 'test-requirement: code failed to satisfy specified code requirement(s)\n' })
    const { d, runCli, warn } = deps({ packagesRoot: installed('0.1.0'), codesign })
    expect(await ensureBundledDocumentsModule(d)).toBe('signature_invalid')
    expect(runCli).not.toHaveBeenCalled()
    expect(warn.mock.calls[0]![0]).toMatch(/not signed by team HYAP444FN9: test-requirement/)
  })

  it('the uninstall of an upgrade fails → no install, a warning, no throw', async () => {
    const { d, runCli, warn } = deps({ packagesRoot: installed('0.1.0'), results: [{ code: 1, output: 'consent store unavailable\n' }] })
    expect(await ensureBundledDocumentsModule(d)).toBe('install_failed')
    expect(runCli).toHaveBeenCalledTimes(1)
    expect(warn.mock.calls[0]![0]).toMatch(/upgrading 0\.1\.0 → 0\.2\.0: os uninstall failed \(rc=1\): consent store unavailable/)
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
      ['--verify', '--strict', `${dir}/bin/agent24-documents`],
      ['--verify', '--strict', `${dir}/bin/agent24-documents-pdfkit`],
    ])
  })

  it('an ad-hoc app: an invalid signature is refused', async () => {
    const codesign = fakeCodesign(ADHOC_APP, RUNTIME, { code: 1, output: 'invalid signature (code or signature have been modified)\n' })
    expect(await signatureProblem(dir, '/A/Agent24.app', codesign)).toMatch(/agent24-documents: invalid signature/)
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
  it.runIf(process.platform === 'darwin')('an ad-hoc signed program passes, a modified one does not', async () => {
    const app = path.join(root, 'X.app')
    const dir = path.join(root, 'module')
    fs.mkdirSync(path.join(dir, 'bin'), { recursive: true })
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
    fs.appendFileSync(path.join(dir, 'bin', 'agent24-documents-pdfkit'), 'x')
    expect(await signatureProblem(dir, app, execCli(CODESIGN))).toMatch(/agent24-documents-pdfkit/)
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
