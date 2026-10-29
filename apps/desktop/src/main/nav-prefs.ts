import fs from 'node:fs'
import path from 'node:path'
import { NAV_PREFS_DEFAULT, SLOT_MIN, SLOT_MAX, type NavPrefs } from '../renderer/capabilities/types'

function parsePrefs(value: unknown): NavPrefs {
  if (typeof value !== 'object' || value === null
    || !('pinned' in value) || !Array.isArray(value.pinned) || !value.pinned.every((id: unknown) => typeof id === 'string')
    || !('slotCount' in value) || typeof value.slotCount !== 'number' || !Number.isFinite(value.slotCount)
    || !('showOverview' in value) || typeof value.showOverview !== 'boolean') {
    throw new Error('Invalid navigation preferences')
  }
  return {
    pinned: [...new Set<string>(value.pinned)],
    slotCount: Math.max(SLOT_MIN, Math.min(SLOT_MAX, Math.round(value.slotCount))),
    showOverview: value.showOverview,
  }
}

/** Synchronous atomic replacement keeps overlapping IPC writes in invocation order. */
export function createNavPrefsStore(dir: string) {
  const file = path.join(dir, 'ui-prefs.json')
  return {
    get(): NavPrefs {
      try { return parsePrefs(JSON.parse(fs.readFileSync(file, 'utf8'))) }
      catch { return { ...NAV_PREFS_DEFAULT, pinned: [...NAV_PREFS_DEFAULT.pinned] } }
    },
    set(value: unknown): void {
      const prefs = parsePrefs(value)
      const temp = `${file}.tmp`
      try {
        fs.mkdirSync(dir, { recursive: true })
        fs.writeFileSync(temp, JSON.stringify(prefs), { mode: 0o600 })
        fs.renameSync(temp, file)
      } finally {
        fs.rmSync(temp, { force: true })
      }
    },
  }
}
