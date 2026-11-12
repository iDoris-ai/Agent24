import fs from 'node:fs'
import path from 'node:path'
import { describe, expect, it } from 'vitest'
import { stopsOwnedOmlxOnQuit } from './omlx-quit-policy'

// PR#1 F1 (ruling 2026-11-13-pr1-omlx-quit-resident, A).
describe('whether quitting stops the oMLX this app started', () => {
  it('keeps it on macOS with the resident service installed', () => {
    expect(stopsOwnedOmlxOnQuit({ platform: 'darwin', serviceInstalled: true })).toBe(false)
  })
  it('stops it without the resident service, as FU-114', () => {
    expect(stopsOwnedOmlxOnQuit({ platform: 'darwin', serviceInstalled: false })).toBe(true)
  })
  it('stops it where there is no resident service', () => {
    for (const platform of ['linux', 'win32'] as const) {
      expect(stopsOwnedOmlxOnQuit({ platform, serviceInstalled: true })).toBe(true)
    }
  })

  // All quit paths in main.ts (shutdown's prepare and stop, its own
  // before-quit) and ipc/index.ts's before-quit go through
  // ensureOwnedOmlxQuit, which asks keepOwnedOmlxOnQuit; main.ts reads the
  // plist when it quits, and 「完全退出」 removed it then (resident-service.ts
  // stopService, `agent24 service uninstall`).
  it('main.ts quits only through ensureOwnedOmlxQuit, asking the plist at the quit', () => {
    const main = fs.readFileSync(path.join(__dirname, 'main.ts'), 'utf8')
    expect(main).not.toMatch(/stopOwnedOmlx\(/)
    expect(main.match(/ensureOwnedOmlxQuit\(\)/g)).toHaveLength(3)
    expect(main).toContain('keepOwnedOmlxOnQuit: () => !stopsOwnedOmlxOnQuit({ platform: process.platform, serviceInstalled: launchAgentPlistExists() }),')
    const ipc = fs.readFileSync(path.join(__dirname, 'ipc', 'index.ts'), 'utf8')
    const ensure = ipc.slice(ipc.indexOf('export function ensureOwnedOmlxQuit'))
    const body = ensure.slice(0, ensure.indexOf('\n}\n'))
    expect(body.indexOf('if (ownedOmlxQuit) return ownedOmlxQuit')).toBeGreaterThan(0)
    expect(body.indexOf('if (keepOwnedOmlxOnQuit()) return keepOwnedOmlx()')).toBeGreaterThan(body.indexOf('if (ownedOmlxQuit) return ownedOmlxQuit'))
    // Asked once per call.
    expect(body.match(/keepOwnedOmlxOnQuit\(\)/g)).toHaveLength(1)
    expect(body.indexOf('if (keepOwnedOmlxOnQuit())')).toBeLessThan(body.indexOf('stopOwnedOmlx()'))
    // Elsewhere only the user's own stop (OmlxStop) stops it.
    expect(ipc.match(/(?<!function )stopOwnedOmlx\(\)/g)).toHaveLength(2)
  })

  // Ruling 2026-11-13-pr1-omlx-orphan-ownership (乙): a kept oMLX is recorded
  // in the data directory, and only the service's stop, after the uninstall,
  // stops what is on record (resident-service.ts stopService afterUninstall;
  // 「完全退出」 and the Settings switch off both go through residentStop).
  it('main.ts records a kept oMLX and stops it only from the service stop', () => {
    const main = fs.readFileSync(path.join(__dirname, 'main.ts'), 'utf8')
    expect(main.match(/rememberKeptOmlx: /g)).toHaveLength(1)
    expect(main.match(/stopKeptOmlx\(/g)).toHaveLength(1)
    const stop = main.slice(main.indexOf('function residentStop(): Promise<StopOutcome>'))
    const body = stop.slice(0, stop.indexOf('\n}\n'))
    expect(body).toContain('afterUninstall:')
    expect(body).toContain('stopKeptOmlx(')
    expect(main.match(/residentStop\b/g)!.length).toBeGreaterThanOrEqual(3)
  })
})
