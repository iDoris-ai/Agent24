import type { CapabilityDef, CapabilityGroup } from './types'
import { registerDynamicMessages } from './capabilities.i18n'

function capability(id: string, group: CapabilityGroup, page = id, builtin = false, fixed = false): CapabilityDef {
  return { id, group, page, builtin, fixed, nameKey: `${id}.name`, descKey: `${id}.description` }
}

export const CAPABILITIES: readonly CapabilityDef[] = [
  capability('assistant', 'office', 'chat', true, true),
  capability('documents', 'office', 'documents', true),
  capability('localModels', 'office', 'models'),
  capability('voice', 'office'),
  capability('knowledgeBase', 'knowledge'),
  capability('mediaBase', 'knowledge'),
  capability('design', 'creative', 'creative'),
  capability('create', 'creative'),
  capability('sin90', 'domainOs'),
  capability('cos72', 'domainOs'),
  capability('tools', 'tools', 'modules-manager'),
  capability('memory', 'services'),
  capability('schedules', 'services'),
  capability('approvals', 'services'),
  capability('communication', 'services'),
]

/** Accept only display metadata; manifests cannot add privileged routes or replace builtins. */
export function withDynamicCapabilities(manifests: unknown): readonly CapabilityDef[] {
  const defs = [...CAPABILITIES]
  const seen = new Set(defs.map(d => d.id))
  if (!Array.isArray(manifests)) return defs
  for (const value of manifests) {
    const m: unknown = value
    if (!m || typeof m !== 'object' || !('id' in m) || typeof m.id !== 'string' || !m.id
      || !('name' in m) || typeof m.name !== 'string' || !m.name || ('enabled' in m && m.enabled === false)
      || !('type' in m) || (m.type !== 'ui' && m.type !== 'hybrid') || seen.has(m.id)) continue
    seen.add(m.id)
    defs.push({
      id: m.id, group: 'domainOs', builtin: false, page: `module:${m.id}`,
      ...registerDynamicMessages(m.id, m.name, 'description' in m && typeof m.description === 'string' ? m.description : undefined),
    })
  }
  return defs
}
