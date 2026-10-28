import { describe, expect, it } from 'vitest'
import { CAPABILITY_GROUPS } from './types'
import { CAPABILITIES, withDynamicCapabilities } from './registry'
import { capabilitiesMessages } from './capabilities.i18n'

describe('capability registry', () => {
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
