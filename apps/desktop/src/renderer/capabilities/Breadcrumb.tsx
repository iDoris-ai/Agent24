import { useT } from '../i18n'
import { appMessages } from '../App.i18n'
import { capabilitiesMessages } from './capabilities.i18n'
import type { CapabilityDef, OpenOrigin } from './types'

export function Breadcrumb({ capability, origin, onBack }: { capability: CapabilityDef; origin: OpenOrigin; onBack: () => void }) {
  const t = useT(appMessages)
  const c = useT(capabilitiesMessages)
  if (origin !== 'capabilities') return null
  return <nav className="capability-breadcrumb" aria-label={c('breadcrumb')}>
    <button onClick={onBack}>{t('nav.capabilities')}</button><span aria-hidden="true">›</span>
    <span aria-current="page">{c(capability.nameKey)}</span>
  </nav>
}
