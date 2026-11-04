import fs from 'node:fs'
import path from 'node:path'
import { describe, expect, it } from 'vitest'

// main.ts boots Electron at import (`app.whenReady`, `app.isPackaged`) and
// vitest excludes it. The snapshot closure is the wiring: reading it is what
// goes red if the call is put back to `headlessSidecarState()`.
describe('Open Design report wiring in main', () => {
  it('the snapshot calls headlessPhaseForReport with the install state', () => {
    const source = fs.readFileSync(path.join(__dirname, 'main.ts'), 'utf8')
    const start = source.indexOf('snapshot: () =>')
    const end = source.indexOf('connection: daemonConnection')
    expect(start).toBeGreaterThan(-1)
    expect(end).toBeGreaterThan(start)
    const snapshot = source.slice(start, end)
    expect(snapshot).toContain(
      "sidecar: creativeServeWeb?.headlessPhaseForReport(install?.state ?? 'unavailable') ?? null,",
    )
    expect(snapshot).not.toContain('headlessSidecarState')
  })
})
