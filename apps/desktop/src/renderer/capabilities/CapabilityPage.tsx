import { useContext } from 'react'
import { useT } from '../i18n'
import { capabilitiesMessages } from './capabilities.i18n'
import { CapabilitiesContext, type CapabilityDef } from './types'

export function CapabilityPage({ capability }: { capability: CapabilityDef }) {
  const c = useT(capabilitiesMessages)
  const { open } = useContext(CapabilitiesContext)
  const next = capability.group === 'domainOs' ? 'next.domainOs'
    : capability.id === 'mediaBase' ? 'next.mediaBase'
      : capability.id === 'create' ? 'next.create' : 'next.service'
  return <main className="content">
    <h1 className="page-title">{c(capability.nameKey)}</h1>
    <p className="page-sub">{c(capability.descKey)}</p>
    <p role="status">{c('status.notSetUp')}</p>
    <p>{c(next)}</p>
    {capability.group === 'domainOs' && <button onClick={() => open('tools', 'other')}>{c('next.tools')}</button>}
  </main>
}
