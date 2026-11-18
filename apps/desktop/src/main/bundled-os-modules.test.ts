// OSI-2: Sin90 ships in the mac app. Version policy, not the documents byte compare.
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  DOCUMENTS_OS,
  SIN90_OS,
  ensureBundledModule,
  ensureBundledOsModules,
  type BundledModuleDeps,
  type CliResult,
} from './documents-module'

let root: string
beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'sin90mod-'))
})
afterEach(() => {
  fs.rmSync(root, { recursive: true, force: true })
})

const manifest = (name: string, version: string): string =>
  `name: ${name}\nversion: "${version}"\nspawn:\n  command: bin/${name}\n`

function bundle(version: string): string {
  const dir = path.join(root, `bundle-${version}`)
  fs.mkdirSync(path.join(dir, 'bin'), { recursive: true })
  fs.writeFileSync(path.join(dir, 'domain-os.yml'), manifest('sin90', version))
  fs.writeFileSync(path.join(dir, 'bin', 'sin90'), `sin90 ${version}`)
  return dir
}

function installed(version: string): string {
  const pkgs = path.join(root, 'data', 'packages')
  fs.mkdirSync(path.join(pkgs, 'sin90', 'bin'), { recursive: true })
  fs.writeFileSync(path.join(pkgs, 'sin90', 'domain-os.yml'), manifest('sin90', version))
  fs.writeFileSync(path.join(pkgs, 'sin90', 'bin', 'sin90'), 'old bytes')
  return pkgs
}

const TEAM_APP = 'Executable=/A/Agent24.app/Contents/MacOS/Agent24\nTeamIdentifier=HYAP444FN9\n'
const RUNTIME = 'CodeDirectory v=20500 size=1 flags=0x10000(runtime) hashes=1+7 location=embedded\n'

function fakeCodesign() {
  return vi.fn(async (args: string[]): Promise<CliResult> => {
    if (args[0] === '-dv') return { code: 0, output: args[1]!.endsWith('.app') ? TEAM_APP : RUNTIME }
    return { code: 0, output: '' }
  })
}

function deps(over: Partial<BundledModuleDeps> = {}): BundledModuleDeps & { runCli: ReturnType<typeof vi.fn> } {
  const runCli = vi.fn(async (args: string[]) => args[0] === 'package-integrity'
    ? { code: 0, output: '' }
    : { code: 0, output: 'installed /x/sin90\n' })
  return {
    bundleDir: over.bundleDir === undefined ? bundle('0.5.2') : over.bundleDir,
    appBundle: over.appBundle === undefined ? '/A/Agent24.app' : over.appBundle,
    codesign: over.codesign ?? fakeCodesign(),
    packagesRoot: over.packagesRoot === undefined ? path.join(root, 'data', 'packages') : over.packagesRoot,
    userDataDir: over.userDataDir ?? root,
    runCli,
    warn: vi.fn(),
    info: vi.fn(),
  }
}

describe('ensureBundledModule sin90', () => {
  it('not installed → generate bin/sin90, then os install, and does not touch a data directory', async () => {
    const d = deps()
    expect(await ensureBundledModule(SIN90_OS, d)).toBe('installed')
    const tmp = d.runCli.mock.calls[0]![0][3]
    expect(d.runCli.mock.calls.map((c) => c[0])).toEqual([
      ['package-integrity', 'generate', '--root', tmp, '--executable', 'bin/sin90'],
      ['os', 'install', tmp],
    ])
    expect(d.runCli.mock.calls.some((c) => c[0].some((arg: string) => arg.includes('/os/sin90')))).toBe(false)
  })

  it('an older install → os uninstall, then os install', async () => {
    const d = deps({ packagesRoot: installed('0.5.1') })
    expect(await ensureBundledModule(SIN90_OS, d)).toBe('upgraded')
    const tmp = d.runCli.mock.calls[0]![0][3]
    expect(d.runCli.mock.calls.map((c) => c[0])).toEqual([
      ['package-integrity', 'generate', '--root', tmp, '--executable', 'bin/sin90'],
      ['os', 'uninstall', 'sin90'],
      ['os', 'install', tmp],
    ])
  })

  it.each(['0.5.2', '0.6.0'])('installed %s → left alone, even when the program bytes differ', async (version) => {
    const pkgs = installed(version)
    fs.writeFileSync(path.join(pkgs, 'sin90', 'bin', 'sin90'), 'bytes from an OSI-1 upgrade')
    const d = deps({ packagesRoot: pkgs })
    expect(await ensureBundledModule(SIN90_OS, d)).toBe('up_to_date')
    expect(d.runCli).not.toHaveBeenCalled()
    expect(d.codesign).not.toHaveBeenCalled()
  })

  it('the user uninstalled it → left uninstalled', async () => {
    fs.writeFileSync(path.join(root, 'sin90-module.json'), JSON.stringify({ uninstalledByUser: true }))
    const d = deps()
    expect(await ensureBundledModule(SIN90_OS, d)).toBe('skipped_user_uninstalled')
    expect(d.runCli).not.toHaveBeenCalled()
  })
})

describe('ensureBundledOsModules', () => {
  it('runs the two modules in order, and a failure does not skip the other', async () => {
    const order: string[] = []
    const documents = deps({
      bundleDir: (() => {
        const dir = path.join(root, 'documents')
        fs.mkdirSync(path.join(dir, 'bin'), { recursive: true })
        fs.writeFileSync(path.join(dir, 'domain-os.yml'), manifest('documents', '0.2.0'))
        fs.writeFileSync(path.join(dir, 'bin', 'agent24-documents'), 'doc')
        fs.writeFileSync(path.join(dir, 'bin', 'agent24-documents-pdfkit'), 'pdf')
        return dir
      })(),
      userDataDir: path.join(root, 'user-a'),
    })
    documents.runCli.mockImplementation(async (args: string[]) => {
      order.push(`documents:${args[0]}`)
      return { code: 1, output: 'documents generate failed\n' }
    })
    const sin90 = deps({ userDataDir: path.join(root, 'user-b') })
    sin90.runCli.mockImplementation(async (args: string[]) => {
      order.push(`sin90:${args[0]}`)
      return { code: 0, output: args[0] === 'os' ? 'installed /x/sin90\n' : '' }
    })
    const failedFirst = await ensureBundledOsModules([
      { spec: DOCUMENTS_OS, deps: documents },
      { spec: SIN90_OS, deps: sin90 },
    ])
    expect(failedFirst).toEqual(['install_failed', 'installed'])
    expect(order).toEqual(['documents:package-integrity', 'sin90:package-integrity', 'sin90:os'])

    order.length = 0
    const sin90Fails = deps({ userDataDir: path.join(root, 'user-c') })
    sin90Fails.runCli.mockImplementation(async () => {
      order.push('sin90')
      throw new Error('sin90 cli missing')
    })
    const documentsOk = deps({
      bundleDir: documents.bundleDir,
      userDataDir: path.join(root, 'user-d'),
      packagesRoot: path.join(root, 'other-packages'),
    })
    documentsOk.runCli.mockImplementation(async (args: string[]) => {
      order.push(`documents:${args[0]}`)
      return { code: 0, output: args[0] === 'os' ? 'installed /x/documents\n' : '' }
    })
    const failedSecond = await ensureBundledOsModules([
      { spec: SIN90_OS, deps: sin90Fails },
      { spec: DOCUMENTS_OS, deps: documentsOk },
    ])
    expect(failedSecond).toEqual(['install_failed', 'installed'])
    expect(order[0]).toBe('sin90')
    expect(order).toContain('documents:os')
  })
})
