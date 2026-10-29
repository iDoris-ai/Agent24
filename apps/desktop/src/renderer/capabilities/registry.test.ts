import { describe, expect, it, vi } from 'vitest'
import { CAPABILITY_GROUPS } from './types'
import { CAPABILITIES, withDynamicCapabilities } from './registry'
import { capabilitiesMessages } from './capabilities.i18n'

describe('capability registry', () => {
  it('excludes and logs reserved route ids and every static capability id', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const reserved = ['capabilities', 'settings', 'settings/general', 'settings/activity', 'settings/custom', 'home', ...CAPABILITIES.map(d => d.id)]
    try {
      const defs = withDynamicCapabilities(reserved.map(id => ({ id, type: 'ui', name: 'Impostor' })))
      expect(defs).toEqual(CAPABILITIES)
      for (const id of reserved) expect(warn).toHaveBeenCalledWith('Ignoring reserved capability manifest', { id })
    } finally {
      warn.mockRestore()
    }
  })

  it('bounds dynamic names and descriptions before registering every locale', () => {
    const defs = withDynamicCapabilities([{ id: 'long-copy', type: 'ui', name: '猫'.repeat(70), description: '🐈'.repeat(210) }])
    const def = defs.find(d => d.id === 'long-copy')
    if (!def) throw new Error('Missing dynamic capability')
    for (const table of Object.values(capabilitiesMessages)) {
      expect(table[def.nameKey]).toBe('猫'.repeat(64))
      expect(table[def.descKey]).toBe('🐈'.repeat(200))
    }
  })

  it('has the agreed unique ids, six nonempty groups and only Assistant fixed', () => {
    expect(CAPABILITIES.map(d => d.id).sort()).toEqual([
      'assistant', 'documents', 'localModels', 'voice', 'knowledgeBase', 'mediaBase',
      'design', 'create', 'sin90', 'cos72', 'tools', 'memory', 'schedules', 'approvals', 'communication',
    ].sort())
    expect(new Set(CAPABILITIES.map(d => d.id)).size).toBe(CAPABILITIES.length)
    for (const group of CAPABILITY_GROUPS) expect(CAPABILITIES.some(d => d.group === group)).toBe(true)
    expect(CAPABILITIES.filter(d => d.fixed).map(d => d.id)).toEqual(['assistant'])
    expect(CAPABILITIES.filter(d => d.builtin).map(d => d.id)).toEqual(['assistant', 'documents'])
    for (const messages of Object.values(capabilitiesMessages)) {
      for (const d of CAPABILITIES) {
        expect(messages[d.nameKey]).toBeTruthy()
        expect(messages[d.descKey]).toBeTruthy()
      }
    }
  })

  it('registers enabled UI/hybrid manifests in domainOs without duplicates or replacing builtins', () => {
    const defs = withDynamicCapabilities([
      { id: 'hello', type: 'ui', enabled: true, name: 'Hello', description: 'Greeting', navItem: { route: 'hello' } },
      { id: 'hybrid', type: 'hybrid', enabled: true, name: 'Hybrid' },
      { id: 'headless', type: 'headless', enabled: true },
      { id: 'disabled', type: 'ui', enabled: false },
      { id: 'assistant', type: 'ui', enabled: true, name: 'Impostor' },
      { id: 'hello', type: 'ui', enabled: true, name: 'Duplicate' },
      null,
    ])
    expect(defs.slice(CAPABILITIES.length).map(d => [d.id, d.group, d.page])).toEqual([
      ['hello', 'domainOs', 'module:hello'], ['hybrid', 'domainOs', 'module:hybrid'],
    ])
    expect(defs.find(d => d.id === 'assistant')?.fixed).toBe(true)
  })
})
