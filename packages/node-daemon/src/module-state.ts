// M2: Module enable/disable state — persisted to ~/.agent24/module-state.json

import fs from 'node:fs'
import path from 'node:path'
import os from 'node:os'

const STATE_DIR = path.join(os.homedir(), '.agent24')
const STATE_FILE = path.join(STATE_DIR, 'module-state.json')

// State: Record<moduleId, enabled>. Default is enabled (true) if not set.
let _state: Record<string, boolean> = {}

export function loadState(): void {
  try {
    const raw = JSON.parse(fs.readFileSync(STATE_FILE, 'utf8')) as unknown
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
    fs.mkdirSync(STATE_DIR, { recursive: true })
    fs.writeFileSync(STATE_FILE, JSON.stringify(_state, null, 2))
    return true
  } catch (err) {
    console.error('[module-state] failed to persist state:', err)
    return false
  }
}

export function getAllState(): Record<string, boolean> {
  return { ..._state }
}
