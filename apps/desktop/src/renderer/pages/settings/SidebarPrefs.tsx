import { useContext, useRef, useState } from 'react'
import { CapabilitiesContext, NavPrefsContext, SLOT_MAX, SLOT_MIN, type CapabilityId, type NavPrefsApi } from '../../capabilities/types'
import { capabilitiesMessages } from '../../capabilities/capabilities.i18n'
import { useT } from '../../i18n'
import { settingsMessages } from '../Settings.i18n'

/** Pinned-slot count, pin order, and the Overview switch. Writes go through
 *  `NavPrefsContext.setPrefs`; ids that call returns are the unpin notice. */
export function SidebarPrefs() {
  const t = useT(settingsMessages)
  const c = useT(capabilitiesMessages)
  const { defs, moduleInventory } = useContext(CapabilitiesContext)
  const { prefs, allPinned = prefs.pinned, setPrefs } = useContext(NavPrefsContext)
  const [saving, setSaving] = useState(false)
  const [notice, setNotice] = useState<CapabilityId[]>([])
  const savingRef = useRef(false)

  function nameFor(id: CapabilityId) {
    const def = defs.find(d => d.id === id)
    return def ? c(def.nameKey) : id
  }

  async function commit(next: Parameters<NavPrefsApi['setPrefs']>[0]) {
    if (savingRef.current) return
    savingRef.current = true
    setSaving(true)
    try {
      const dropped = await setPrefs(next)
      setNotice(dropped.filter(id => defs.some(d => d.id === id)))
    } catch {
      // The context keeps the last saved prefs. A failed write must not claim
      // that something was unpinned.
    } finally {
      savingRef.current = false
      setSaving(false)
    }
  }

  function stepSlots(delta: number) {
    void commit(prev => ({ ...prev, slotCount: Math.min(SLOT_MAX, Math.max(SLOT_MIN, prev.slotCount + delta)) }))
  }

  function move(index: number, dir: -1 | 1) {
    const target = index + dir
    if (target < 0 || target >= allPinned.length) return
    const id = allPinned[index]
    const neighbor = allPinned[target]
    void commit(prev => {
      const pinned = prev.pinned.slice()
      const source = pinned.indexOf(id)
      const destination = pinned.indexOf(neighbor)
      if (source < 0 || destination < 0) return prev
      ;[pinned[source], pinned[destination]] = [pinned[destination], pinned[source]]
      return { ...prev, pinned }
    })
  }

  return (
    <div className="settings-section">
      <h3>{t('sidebar.title')}</h3>

      <div className="setting-row">
        <div>
          <label htmlFor="settings-slot-count">{t('sidebar.slots')}</label>
          <p>{t('sidebar.slotsHint', { min: SLOT_MIN, max: SLOT_MAX })}</p>
        </div>
        <div className="settings-stepper">
          <button type="button" className="btn btn-ghost" aria-label={t('sidebar.slotsDecrease')}
            disabled={saving || prefs.slotCount <= SLOT_MIN} onClick={() => stepSlots(-1)}>−</button>
          <output id="settings-slot-count" data-testid="slot-count">{prefs.slotCount}</output>
          <button type="button" className="btn btn-ghost" aria-label={t('sidebar.slotsIncrease')}
            disabled={saving || prefs.slotCount >= SLOT_MAX} onClick={() => stepSlots(1)}>+</button>
        </div>
      </div>

      <div className="setting-row" style={{ alignItems: 'flex-start' }}>
        <div>
          <label>{t('sidebar.order')}</label>
          <p>{t('sidebar.orderHint')}</p>
        </div>
        {allPinned.length === 0 ? (
          <p className="settings-note">{t('sidebar.empty')}</p>
        ) : (
          <ol className="settings-pin-list">
            {allPinned.map((id, index) => (
              <li key={id}>
                <span className="settings-pin-name">{nameFor(id)}{!defs.some(d => d.id === id) && ` — ${t(moduleInventory?.disabledIds.has(id) ? 'sidebar.disabled' : 'sidebar.unavailable')}`}</span>
                <button type="button" className="btn btn-ghost" aria-label={t('sidebar.moveUp', { name: nameFor(id) })}
                  disabled={saving || index === 0} onClick={() => move(index, -1)}>↑</button>
                <button type="button" className="btn btn-ghost" aria-label={t('sidebar.moveDown', { name: nameFor(id) })}
                  disabled={saving || index === allPinned.length - 1} onClick={() => move(index, 1)}>↓</button>
                {!defs.some(d => d.id === id) && <button type="button" className="btn btn-ghost"
                  aria-label={t('sidebar.unpinName', { name: id })} disabled={saving}
                  onClick={() => { void commit(prev => ({ ...prev, pinned: prev.pinned.filter(pin => pin !== id) })) }}>{t('sidebar.unpin')}</button>}
              </li>
            ))}
          </ol>
        )}
      </div>

      <div className="setting-row">
        <div>
          <label id="settings-show-overview">{t('sidebar.showOverview')}</label>
          <p>{t('sidebar.showOverviewHint')}</p>
        </div>
        <button type="button" className="settings-switch" role="switch"
          aria-labelledby="settings-show-overview" aria-checked={prefs.showOverview}
          disabled={saving} onClick={() => { void commit(prev => ({ ...prev, showOverview: !prev.showOverview })) }}>
          <span className="settings-switch-knob" />
        </button>
      </div>

      {notice.length > 0 && <p className="settings-status" role="status">{t('sidebar.unpinned', { names: notice.map(nameFor).join(', ') })}</p>}
    </div>
  )
}
