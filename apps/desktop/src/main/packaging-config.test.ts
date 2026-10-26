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
