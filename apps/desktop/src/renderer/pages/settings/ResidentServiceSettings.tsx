import { useEffect, useState } from 'react'
import type { ResidentServiceResult, ResidentServiceStatus } from '../../../shared/ipc-types'
import { residentServiceMessages } from '../../../shared/resident-service.i18n'
import { useT } from '../../i18n'

// M5 B4c2: the 「后台运行」 switch. Hidden unless `available` (a packaged
// macOS app). Checked while the LaunchAgent's plist is there; a change calls
// `setResidentService(on)` once (the switch is disabled meanwhile), then the
// status is read again, and a failure is shown by its stage. The status is
// read again on focus and every 10 s too.
export function ResidentServiceSettings() {
  const t = useT(residentServiceMessages)
  const [status, setStatus] = useState<ResidentServiceStatus | null>(null)
  const [busy, setBusy] = useState(false)
  const [failure, setFailure] = useState<Extract<ResidentServiceResult, { ok: false }> | null>(null)
  async function refresh() {
    try { setStatus(await window.agent24.residentServiceStatus()) } catch { setStatus(null) }
  }
  useEffect(() => {
    // Also as the first-launch box, the tray or another window change it (Codex B4c2 C3).
    void refresh()
    const timer = setInterval(() => { void refresh() }, 10_000)
    window.addEventListener('focus', refresh)
    return () => { clearInterval(timer); window.removeEventListener('focus', refresh) }
  }, [])
  if (!status?.available) return null
  async function change(on: boolean) {
    if (busy) return
    setBusy(true)
    setFailure(null)
    try {
      const result = await window.agent24.setResidentService(on)
      if (!result.ok) setFailure(result)
    } catch {
      setFailure({ ok: false, stage: on ? 'install' : 'shutdown', code: 'ipc_failed' })
    } finally {
      await refresh()
      setBusy(false)
    }
  }
  const message = failure && (failure.code === 'not_connected' ? t('failed.not_connected') : failure.code === 'busy' ? t('failed.busy') : t(`failed.${failure.stage}`, { code: failure.code }))
  return <section className="settings-section" aria-labelledby="resident-service-title">
    <h3 id="resident-service-title">{t('title')}</h3>
    <div className="setting-row">
      <div><label htmlFor="resident-service-on">{t('label')}</label><p>{t('hint')}</p></div>
      <input id="resident-service-on" type="checkbox" checked={status.installed} disabled={busy}
        onChange={(e) => { void change(e.target.checked) }} />
    </div>
    {busy && <p role="status">{t('busy')}</p>}
    {message && <p role="alert">{message}</p>}
  </section>
}

export type { ResidentServiceResult, ResidentServiceStatus }
