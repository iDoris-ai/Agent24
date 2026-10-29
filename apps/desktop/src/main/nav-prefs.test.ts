import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createNavPrefsStore } from './nav-prefs'
import { NAV_PREFS_DEFAULT } from '../renderer/capabilities/types'

let dir: string
beforeEach(() => { dir = fs.mkdtempSync(path.join(os.tmpdir(), 'nav-prefs-')) })
afterEach(() => { vi.restoreAllMocks(); fs.rmSync(dir, { recursive: true, force: true }) })

describe('navigation preferences on disk', () => {
  it('defaults on first run and persists across store instances with atomic replacement', () => {
    const store = createNavPrefsStore(dir)
    expect(store.get()).toEqual(NAV_PREFS_DEFAULT)
    const prefs = { pinned: ['voice'], slotCount: 2, showOverview: false }
    store.set(prefs)
    expect(createNavPrefsStore(dir).get()).toEqual(prefs)
    expect(JSON.parse(fs.readFileSync(path.join(dir, 'ui-prefs.json'), 'utf8'))).toEqual(prefs)
    expect(fs.readdirSync(dir)).toEqual(['ui-prefs.json'])
  })

  it.each(['not json', 'null', '[]', '{"pinned":[3],"slotCount":2,"showOverview":true}',
    '{"pinned":[],"slotCount":"3","showOverview":true}'])('falls back on damaged data: %s', data => {
    fs.writeFileSync(path.join(dir, 'ui-prefs.json'), data)
    expect(createNavPrefsStore(dir).get()).toEqual(NAV_PREFS_DEFAULT)
  })

  it('rejects invalid IPC input and preserves the previous file when replacement fails', () => {
    const store = createNavPrefsStore(dir)
    store.set(NAV_PREFS_DEFAULT)
    expect(() => store.set({ pinned: 'invalid' })).toThrow()
    const rename = vi.spyOn(fs, 'renameSync').mockImplementation(() => { throw new Error('disk unavailable') })
    expect(() => store.set({ pinned: [], slotCount: 1, showOverview: false })).toThrow('disk unavailable')
    rename.mockRestore()
    expect(store.get()).toEqual(NAV_PREFS_DEFAULT)
    expect(createNavPrefsStore(dir).get()).toEqual(NAV_PREFS_DEFAULT)
    expect(fs.readdirSync(dir)).toEqual(['ui-prefs.json'])
  })
})
