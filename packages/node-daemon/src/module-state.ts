// M2: Module enable/disable state — persisted to <data_dir>/module-state.json
// (ADR-037: @agent24/data-dir). Resolved on each call, not at module load.

import fs from 'node:fs'
import { dataDir, joinLiteral } from '@agent24/data-dir'

export function moduleStateFile(): string {
  return joinLiteral(dataDir(), 'module-state.json')
}

// State: Record<moduleId, enabled>. Default is enabled (true) if not set.
let _state: Record<string, boolean> = {}

export function loadState(): void {
  try {
    const raw = JSON.parse(fs.readFileSync(moduleStateFile(), 'utf8')) as unknown
    if (raw && typeof raw === 'object' && !Array.isArray(raw)) {
      // Validate: only keep entries where key is string and value is boolean
      _state = Object.fromEntries(
        Object.entries(raw as Record<string, unknown>).filter(
          ([k, v]) => typeof k === 'string' && typeof v === 'boolean',
        ) as [string, boolean][],
      )
    } else {
      _state = {}
    }
  } catch { _state = {} }
}

export function isEnabled(moduleId: string): boolean {
  return _state[moduleId] !== false  // default: enabled
}

// Returns whether the new state was DURABLY persisted. Callers that rely on the
// state surviving a restart (H10: "disabled pending consent" must not silently
// revert to enabled after a crash) treat `false` as a hard failure — e.g. the
// installer rolls the install back rather than leave a module it cannot record
// as disabled.
export function setEnabled(moduleId: string, enabled: boolean): boolean {
  _state[moduleId] = enabled
  try {
    const dir = dataDir()
    fs.mkdirSync(dir, { recursive: true })
    fs.writeFileSync(joinLiteral(dir, 'module-state.json'), JSON.stringify(_state, null, 2))
    return true
  } catch (err) {
    console.error('[module-state] failed to persist state:', err)
    return false
  }
}

export function getAllState(): Record<string, boolean> {
  return { ..._state }
}
