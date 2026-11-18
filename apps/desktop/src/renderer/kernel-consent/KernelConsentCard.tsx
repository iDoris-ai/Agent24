import { useEffect, useState } from 'react'
import { useT, type T } from '../i18n'
import { kernelConsentMessages } from './kernelConsent.i18n'
import {
  errorCode,
  loadKernelConsent,
  proposeKernelConsent,
  type KernelConsentView,
} from './kernelConsentApi'

const PHRASES = {
  events: 'cap.events',
  models: 'cap.models',
  scheduler: 'cap.scheduler',
  memory: 'cap.memory',
  approval: 'cap.approval',
} as const

export function kernelCapabilityPhrase(t: T<keyof typeof kernelConsentMessages.en>, id: string): string {
  const key = PHRASES[id as keyof typeof PHRASES]
  return key ? t(key) : t('cap.unknown', { id })
}

/** Checkbox card. It never writes a consent row: Continue only proposes the
 * checked set, and the daemon's presence dialog is the trust anchor. */
export function KernelConsentCard({ name, onDecided }: { name: string; onDecided?: () => void }) {
  const t = useT(kernelConsentMessages)
  const [view, setView] = useState<KernelConsentView | null>(null)
  const [checked, setChecked] = useState<string[]>([])
  const [note, setNote] = useState<string | null>(null)
  const [hidden, setHidden] = useState(false)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    let cancel = false
    setView(null)
    setNote(null)
    setHidden(false)
    void loadKernelConsent(name).then((next) => {
      if (cancel || !next || next.class !== 'missing') return
      setView(next)
      setChecked(next.capabilities)
    }).catch(() => { /* no card when the daemon cannot be asked */ })
    return () => { cancel = true }
  }, [name])

  if (!view) return null
  if (hidden) return note ? <p role="status">{note}</p> : null

  async function agree(): Promise<void> {
    if (!view) return
    if (checked.length === 0) {
      setNote(t('empty'))
      setHidden(true)
      return
    }
    setBusy(true)
    setNote(null)
    try {
      const res = await proposeKernelConsent(view.module, view.fingerprint, checked)
      if (res.ok) {
        setHidden(true)
        onDecided?.()
        return
      }
      const code = errorCode(res)
      setNote(code === 'presence_unavailable' || code === 'presence_timeout' ? t('unavailable')
        : code === 'presence_denied' || code === 'consent_empty' ? t('denied')
          : t('failed'))
      setHidden(true)
    } catch {
      setNote(t('failed'))
      setHidden(true)
    } finally {
      setBusy(false)
    }
  }

  return (
    <section className="kernel-consent" aria-label={t('title', { name: view.module })}>
      <h2>{t('title', { name: view.module })}</h2>
      <p>{view.team ? t('team.present', { team: view.team }) : t('team.missing')}</p>
      <p>{view.origin === 'bundled' ? t('origin.bundled') : t('origin.local')}</p>
      <p>{t('version', { version: view.version })}</p>
      <p>{view.isolated ? t('isolated') : t('notIsolated')}</p>
      <p>{t('hint')}</p>
      <ul>
        {view.capabilities.map((id) => (
          <li key={id}>
            <label>
              <input
                type="checkbox"
                checked={checked.includes(id)}
                onChange={(event) => {
                  setChecked((prev) => event.target.checked
                    ? [...prev, id]
                    : prev.filter((item) => item !== id))
                }}
              />
              {kernelCapabilityPhrase(t, id)}
            </label>
          </li>
        ))}
      </ul>
      {note && <p role="status">{note}</p>}
      <button type="button" disabled={busy} onClick={() => void agree()}>{t('agree')}</button>
      <button type="button" disabled={busy} onClick={() => setHidden(true)}>{t('later')}</button>
    </section>
  )
}
