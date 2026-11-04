import fs from 'node:fs'
import path from 'node:path'
import { describe, expect, it } from 'vitest'

import { CREATIVE_QUIT_STOP_BUDGET_MS } from './creative-serve-web'
import { OPEN_DESIGN_KILL_AFTER_MS, OPEN_DESIGN_TERM_GRACE_MS } from './open-design-sidecar'

// main.ts boots Electron at import and is excluded from vitest, so the wiring is checked by reading it.
const source = fs.readFileSync(path.join(__dirname, 'main.ts'), 'utf8')

describe('Creative lifecycle wiring in main', () => {
  it('the quit budget is 22 s and covers the sidecar grace plus kill phases', () => {
    expect(CREATIVE_QUIT_STOP_BUDGET_MS).toBe(22_000)
    expect(CREATIVE_QUIT_STOP_BUDGET_MS).toBeGreaterThan(OPEN_DESIGN_TERM_GRACE_MS + OPEN_DESIGN_KILL_AFTER_MS)
  })

  it('before-quit holds the quit, stops Creative within the budget, then quits again', () => {
    const start = source.indexOf("app.on('before-quit'")
    const end = source.indexOf("app.on('will-quit'")
    expect(start).toBeGreaterThan(-1)
    expect(end).toBeGreaterThan(start)
    const handler = source.slice(start, end)
    expect(handler).toContain('(event)')
    expect(handler).toContain('creative?.hasLiveProcess()')
    expect(handler).toContain('event.preventDefault()')
    expect(handler).toContain('stopWithin(creative, CREATIVE_QUIT_STOP_BUDGET_MS)')
    expect(handler).toContain("creativeQuitState = 'done'")
    expect(handler.indexOf("creativeQuitState = 'done'")).toBeLessThan(handler.indexOf('app.quit()'))
  })

  it('will-quit still stops the backend after Creative, so Open Design goes first', () => {
    const willQuit = source.slice(source.indexOf("app.on('will-quit'"))
    expect(willQuit).toContain('backendManager.stop()')
    expect(willQuit).toContain('creativeServeWeb?.stop()')
  })

  it('the view reloads when the sidecar instance changes', () => {
    const show = source.slice(source.indexOf('async function showCreativeView'), source.indexOf('async function restartCreativeView'))
    expect(show).toContain('loadCreativeView({')
    expect(show).toContain('loadedInstanceId: creativeLoadedInstanceId')
    expect(show).toContain('instanceId: status.instanceId')
    expect(show).toContain('creativeLoadedInstanceId = instanceId')
  })
})
