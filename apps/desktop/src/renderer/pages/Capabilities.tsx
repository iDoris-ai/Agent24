import { useContext, useId } from 'react'
import {
  CAPABILITY_GROUPS,
  CapabilitiesContext,
  NavPrefsContext,
  type CapabilityDef,
  type CapabilityGroup,
  type CapabilityId,
  type NavPrefs,
} from '../capabilities/types'
import { useT } from '../i18n'
import { capabilitiesMessages } from './Capabilities.i18n'

/** Row that shows the pending-approval count. Matches the capability id. */
const APPROVALS_ID = 'approvals'

const GROUP_COPY = {
  office: { title: 'group.office', blurb: 'group.office.blurb' },
  knowledge: { title: 'group.knowledge', blurb: 'group.knowledge.blurb' },
  creative: { title: 'group.creative', blurb: 'group.creative.blurb' },
  domainOs: { title: 'group.domainOs', blurb: 'group.domainOs.blurb' },
  tools: { title: 'group.tools', blurb: 'group.tools.blurb' },
  services: { title: 'group.services', blurb: 'group.services.blurb' },
} as const satisfies Record<CapabilityGroup, {
  title: keyof typeof capabilitiesMessages.en
  blurb: keyof typeof capabilitiesMessages.en
}>

export interface CapabilitiesPageProps {
  /**
   * Later status seam. A returned string replaces the default label for that
   * id; `undefined` keeps Built-in (builtin) or Open (everything else).
   */
  statusFor?: (id: CapabilityId) => string | undefined
  /**
   * Slots-full note. Settings is not a capability id, so the link cannot go
   * through `open`.
   */
  onOpenSettings?: () => void
}

export default function CapabilitiesPage({ statusFor, onOpenSettings }: CapabilitiesPageProps) {
  const { defs, open, approvalsWaiting } = useContext(CapabilitiesContext)
  const { prefs, setPrefs } = useContext(NavPrefsContext)
  const t = useT(capabilitiesMessages)
  const baseId = useId()
  const slotsFull = prefs.pinned.length >= prefs.slotCount

  function commit(next: NavPrefs): void {
    void Promise.resolve(setPrefs(next)).catch(() => {
      // Persistence and any error surface belong to the context owner.
    })
  }

  function statusText(def: CapabilityDef): string {
    const custom = statusFor?.(def.id)
    if (custom) return custom
    return def.builtin ? t('status.builtin') : t('status.open')
  }

  function pinOf(def: CapabilityDef): { label: string; pressed: boolean; disabled: boolean; next: NavPrefs | null } {
    if (def.fixed) return { label: t('pin.fixed'), pressed: false, disabled: true, next: null }
    if (prefs.pinned.includes(def.id)) {
      return {
        label: t('pin.pinned'),
        pressed: true,
        disabled: false,
        next: { ...prefs, pinned: prefs.pinned.filter((id) => id !== def.id) },
      }
    }
    if (slotsFull) return { label: t('pin.full'), pressed: false, disabled: true, next: null }
    return {
      label: t('pin.pin'),
      pressed: false,
      disabled: false,
      next: { ...prefs, pinned: [...prefs.pinned, def.id] },
    }
  }

  const groups = CAPABILITY_GROUPS
    .map((group) => ({ group, items: defs.filter((def) => def.group === group) }))
    .filter((group) => group.items.length > 0)

  return (
    <div className="content">
      <h1 className="page-title">{t('page.title')}</h1>
      <p className="page-sub">{t('page.subtitle')}</p>

      <div className="setting-row cap-overview">
        <label htmlFor={`${baseId}-overview`}>{t('overview.show')}</label>
        <input
          id={`${baseId}-overview`}
          type="checkbox"
          checked={prefs.showOverview}
          onChange={(event) => commit({ ...prefs, showOverview: event.target.checked })}
        />
      </div>

      {slotsFull && (
        <p className="cap-banner">
          {t('banner.before')}
          <a
            href="#settings"
            onClick={(event) => {
              event.preventDefault()
              onOpenSettings?.()
            }}
          >
            {t('banner.settings')}
          </a>
          {t('banner.after')}
        </p>
      )}

      <div className="cap-groups">
        {groups.map(({ group, items }) => {
          const copy = GROUP_COPY[group]
          const headingId = `${baseId}-${group}`
          return (
            <section key={group} className="cap-group" role="region" aria-labelledby={headingId}>
              <h2 id={headingId}>{t(copy.title)}</h2>
              <p className="cap-blurb">{t(copy.blurb)}</p>
              <ul className="cap-list">
                {items.map((def) => {
                  // D1 owns the catalog (capabilities.i18n.ts). This page does
                  // not import it, so an unresolved key is the label.
                  const name = def.nameKey
                  const pin = pinOf(def)
                  return (
                    <li key={def.id} className="cap-row" aria-label={name}>
                      <div className="cap-copy">
                        <div className="cap-titleline">
                          <button type="button" className="cap-name" onClick={() => open(def.id, 'capabilities')}>
                            {name}
                          </button>
                          {def.id === APPROVALS_ID && approvalsWaiting > 0 && (
                            <span className="tag cap-count" role="status" aria-label={t('approvals.waiting', { count: approvalsWaiting })}>
                              {approvalsWaiting}
                            </span>
                          )}
                        </div>
                        <p className="cap-desc">{def.descKey}</p>
                      </div>
                      <div className="cap-actions">
                        <span className="tag cap-status">{statusText(def)}</span>
                        <button type="button" className="btn btn-primary" onClick={() => open(def.id, 'capabilities')}>
                          {t('action.open')}
                        </button>
                        <button
                          type="button"
                          className="btn btn-ghost"
                          aria-pressed={pin.pressed}
                          aria-label={`${pin.label} ${name}`}
                          disabled={pin.disabled}
                          onClick={() => {
                            if (pin.disabled || !pin.next) return
                            commit(pin.next)
                          }}
                        >
                          {pin.label}
                        </button>
                      </div>
                    </li>
                  )
                })}
              </ul>
            </section>
          )
        })}
      </div>
    </div>
  )
}
