import { useContext, useRef, useState } from 'react'
import { NavPrefsContext, SLOT_MAX, SLOT_MIN, type NavPrefs } from '../../capabilities/types'
import { useT } from '../../i18n'
import { settingsMessages } from '../Settings.i18n'

/** Pinned-slot count, pin order, and the Overview switch. Writes go through
 *  `NavPrefsContext.setPrefs`; ids that call returns are the unpin notice. */
export function SidebarPrefs() {
  const t = useT(settingsMessages)
  const { prefs, setPrefs } = useContext(NavPrefsContext)
  const [saving, setSaving] = useState(false)
  const [notice, setNotice] = useState<string | null>(null)
  const savingRef = useRef(false)

  async function commit(next: NavPrefs) {
    if (savingRef.current) return
    savingRef.current = true
    setSaving(true)
    try {
      const dropped = await setPrefs(next)
      setNotice(Array.isArray(dropped) && dropped.length > 0 ? dropped.join(', ') : null)
    } catch {
      // The context keeps the last saved prefs. A failed write must not claim
      // that something was unpinned.
    } finally {
      savingRef.current = false
      setSaving(false)
    }
  }

  function stepSlots(delta: number) {
    const slotCount = Math.min(SLOT_MAX, Math.max(SLOT_MIN, prefs.slotCount + delta))
    if (slotCount === prefs.slotCount) return
    void commit({ ...prefs, slotCount })
  }

  function move(index: number, dir: -1 | 1) {
    const target = index + dir
    if (target < 0 || target >= prefs.pinned.length) return
    const pinned = prefs.pinned.slice()
    const [item] = pinned.splice(index, 1)
    pinned.splice(target, 0, item)
    void commit({ ...prefs, pinned })
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
        {prefs.pinned.length === 0 ? (
          <p className="settings-note">{t('sidebar.empty')}</p>
        ) : (
          <ol className="settings-pin-list">
            {prefs.pinned.map((id, index) => (
              <li key={id}>
                <span className="settings-pin-name">{id}</span>
                <button type="button" className="btn btn-ghost" aria-label={t('sidebar.moveUp', { name: id })}
                  disabled={saving || index === 0} onClick={() => move(index, -1)}>↑</button>
                <button type="button" className="btn btn-ghost" aria-label={t('sidebar.moveDown', { name: id })}
                  disabled={saving || index === prefs.pinned.length - 1} onClick={() => move(index, 1)}>↓</button>
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
          disabled={saving} onClick={() => { void commit({ ...prefs, showOverview: !prefs.showOverview }) }}>
          <span className="settings-switch-knob" />
        </button>
      </div>

      {notice && <p className="settings-status" role="status">{t('sidebar.unpinned', { names: notice })}</p>}
    </div>
  )
}
