import { expect, it } from 'vitest'
import { clampPrefs, type CapabilityDef } from './types'

const defs: CapabilityDef[] = [
  { id: 'assistant', group: 'office', nameKey: 'a', descKey: 'a', builtin: true, fixed: true, page: 'assistant' },
  { id: 'documents', group: 'office', nameKey: 'd', descKey: 'd', builtin: true, page: 'documents' },
  { id: 'design', group: 'creative', nameKey: 'g', descKey: 'g', builtin: false, page: 'design' },
  { id: 'create', group: 'creative', nameKey: 'c', descKey: 'c', builtin: false, page: 'create' },
  { id: 'schedules', group: 'services', nameKey: 's', descKey: 's', builtin: true, page: 'schedules' },
]

it('unpins what no longer fits and reports it', () => {
  const r = clampPrefs({ pinned: ['documents', 'design', 'create', 'schedules'], slotCount: 3, showOverview: true }, defs)
  expect(r.prefs.pinned).toEqual(['documents', 'design', 'create'])
  expect(r.dropped).toEqual(['schedules'])
})

it('clamps slot count to 1..6 and drops unknown, fixed and duplicate ids', () => {
  const r = clampPrefs({ pinned: ['assistant', 'nope', 'design', 'design'], slotCount: 99, showOverview: false }, defs)
  expect(r.prefs).toEqual({ pinned: ['design'], slotCount: 6, showOverview: false })
  expect(clampPrefs({ pinned: [], slotCount: 0, showOverview: true }, defs).prefs.slotCount).toBe(1)
})
