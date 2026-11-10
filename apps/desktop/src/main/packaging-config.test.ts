// ADR-037 decision 1 (CORE-8): packages/node-daemon is a contract-test /
// dev reference only and must never be copied into an installer. Assert the
// electron-builder config statically so the check runs on every CI platform.
import { describe, it, expect } from 'vitest'
import fs from 'node:fs'
import path from 'node:path'

interface Res {
  from?: string
  to?: string
}
const pkg = JSON.parse(
  fs.readFileSync(path.join(__dirname, '..', '..', 'package.json'), 'utf8'),
) as {
  dependencies?: Record<string, string>
  scripts: Record<string, string>
  build: Record<string, unknown> & {
    extraResources: Res[]
    mac: { extraResources: Res[] }
    win: { extraResources: Res[] }
    linux: { extraResources: Res[] }
  }
}

describe('installer excludes node-daemon (ADR-037)', () => {
  const groups: Record<string, Res[]> = {
    global: pkg.build.extraResources,
    mac: pkg.build.mac.extraResources,
    win: pkg.build.win.extraResources,
    linux: pkg.build.linux.extraResources,
  }
  for (const [name, list] of Object.entries(groups)) {
    it(`${name} extraResources has no node-daemon`, () => {
      expect(JSON.stringify(list)).not.toMatch(/node-daemon/)
    })
  }

  it('mac/win/linux each still ship agent24d', () => {
    for (const p of ['mac', 'win', 'linux'] as const) {
      expect(pkg.build[p].extraResources.some((r) => /agent24d/.test(r.from ?? ''))).toBe(true)
    }
  })

  it('node-daemon is not a runtime dependency (electron-builder would pack it)', () => {
    expect(pkg.dependencies ?? {}).not.toHaveProperty('@agent24/node-daemon')
  })

  it('build:* scripts do not build node-daemon', () => {
    for (const k of ['build:mac', 'build:win', 'build:linux']) {
      expect(pkg.scripts[k]).not.toMatch(/node-daemon/)
    }
  })
})

// T1-1 (#1159 Q1 = A, ADR-DOC-01 D6): the mac installer carries the documents
// module package staged by scripts/stage-documents-module.sh; Linux/Windows do
// not (decided 2026-11-03). build:mac stages it before electron-builder runs,
// so a mac build never packs a stale or missing module by accident.
describe('mac installer carries the documents module (T1-1)', () => {
  const isModule = (r: Res): boolean => r.to === 'documents-module'

  it('mac extraResources copies .documents-module to documents-module', () => {
    const entries = pkg.build.mac.extraResources.filter(isModule)
    expect(entries).toHaveLength(1)
    expect(entries[0]!.from).toBe('.documents-module')
  })

  it('win, linux and the global list do not', () => {
    for (const list of [pkg.build.extraResources, pkg.build.win.extraResources, pkg.build.linux.extraResources]) {
      expect(JSON.stringify(list)).not.toMatch(/documents-module/)
    }
  })

  it('mac signs both module programs as binaries (T1-3: Hardened Runtime, the app\'s team)', () => {
    const binaries = (pkg.build.mac as { binaries?: string[] }).binaries ?? []
    expect(binaries).toEqual(expect.arrayContaining([
      'Contents/Resources/documents-module/bin/agent24-documents',
      'Contents/Resources/documents-module/bin/agent24-documents-pdfkit',
    ]))
  })

  it('release-mac.sh builds the module, stages it, and only then runs electron-builder (T1-3)', () => {
    const release = fs.readFileSync(path.join(__dirname, '..', '..', 'scripts', 'release-mac.sh'), 'utf8')
    const cargo = release.indexOf('-p agent24-documents')
    const stage = release.indexOf('scripts/stage-documents-module.sh" "$CARGO_OUT"')
    const builder = release.indexOf('pnpm exec electron-builder')
    expect(cargo).toBeGreaterThan(-1)
    expect(stage).toBeGreaterThan(cargo)
    expect(builder).toBeGreaterThan(stage)
  })

  it('build:mac builds and stages the module before electron-builder', () => {
    const steps = pkg.scripts['build:mac']!.split('&&').map((s) => s.trim())
    const at = (s: string): number => steps.indexOf(s)
    expect(at('pnpm build:rust:documents')).toBeGreaterThanOrEqual(0)
    expect(at('pnpm stage:documents')).toBeGreaterThan(at('pnpm build:rust:documents'))
    expect(at('electron-builder --mac')).toBeGreaterThan(at('pnpm stage:documents'))
    expect(pkg.scripts['build:win']).not.toMatch(/documents/)
    expect(pkg.scripts['build:linux']).not.toMatch(/documents/)
  })
})

describe('installer excludes development bridges (ADR-044 T3)', () => {
  it('packaging, scripts and runtime dependencies never reference bridge packages', () => {
    expect(JSON.stringify({ build: pkg.build, scripts: pkg.scripts, dependencies: pkg.dependencies }))
      .not.toMatch(/(?:packages[\\/]|@agent24[\\/])[^"\s]*-bridge/)
  })

  it('build, staging and workflow scripts never stage bridge packages', () => {
    const desktop = path.join(__dirname, '..', '..')
    const repo = path.join(desktop, '..', '..')
    for (const dir of [path.join(desktop, 'scripts'), path.join(repo, 'scripts'), path.join(repo, '.github', 'workflows')]) {
      for (const name of fs.readdirSync(dir, { withFileTypes: true })
        .filter((entry) => entry.isFile() && !/\.test\.[cm]?[jt]s$/.test(entry.name))
        .map((entry) => entry.name)) {
        expect(fs.readFileSync(path.join(dir, name), 'utf8'))
          .not.toMatch(/(?:packages[\\/]|@agent24[\\/])[^"\s]*-bridge/)
      }
    }
  })
})
