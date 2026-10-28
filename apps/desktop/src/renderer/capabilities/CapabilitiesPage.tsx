import { useContext } from 'react'
import { useT } from '../i18n'
import { appMessages } from '../App.i18n'
import { capabilitiesMessages } from './capabilities.i18n'
import { CAPABILITY_GROUPS, CapabilitiesContext } from './types'

/** Minimal standalone capability center; D2 replaces this view. */
export function CapabilitiesPage() {
  const { defs, open } = useContext(CapabilitiesContext)
  const t = useT(appMessages)
  const c = useT(capabilitiesMessages)
  return <main className="content">
    <h1 className="page-title">{t('nav.capabilities')}</h1>
    {CAPABILITY_GROUPS.map(group => <section key={group} aria-labelledby={`capability-group-${group}`}>
      <h2 id={`capability-group-${group}`}>{c(`group.${group}`)}</h2>
      <ul className="capability-list">{defs.filter(d => d.group === group).map(def => <li key={def.id}>
        <button onClick={() => open(def.id, 'capabilities')}>{c(def.nameKey)}</button>
        <p>{c(def.descKey)}</p>
      </li>)}</ul>
    </section>)}
  </main>
}
