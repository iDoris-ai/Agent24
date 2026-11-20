import { useCallback, useEffect, useId, useState } from 'react'
import { useLocale, useT } from '../../i18n'
import { listComponents, type ComponentView } from '../componentApi'
import { getCandidates, getStatus, IdorisError, submitOperation, type IdorisStatus } from './api'
import { idorisErrors, idorisMessages } from './IdorisPanel.i18n'
import './idoris.css'
import { isChatModelId } from '../../pages/settings/chatModels'

type PanelState = { kind: 'loading' } | { kind: 'ready'; component: ComponentView | null } | { kind: 'error' }

function stateText(t: (key: keyof typeof idorisMessages.en, vars?: Record<string, string | number>) => string, component: ComponentView): string {
  switch (component.state) {
    case 'absent': return t('state.not_installed')
    case 'installed': return component.reason === 'not_configured' ? t('state.not_configured') : t('state.installed')
    case 'ready': return t('state.running')
    case 'starting': return t('state.starting')
    case 'stopped': return t('state.stopped')
    case 'failed': return t('state.failed')
    case 'degraded': return t('state.degraded')
    default: return component.state
  }
}

/** Core component status is read-only; daemon startup owns install and setup. */
export default function IdorisPanel() {
  const t = useT(idorisMessages)
  const locale = useLocale()
  const id = useId()
  const [status, setStatus] = useState<IdorisStatus | null>(null)
  const [componentState, setComponentState] = useState<PanelState>({ kind: 'loading' })
  const [error, setError] = useState<string | null>(null)
  const [pending, setPending] = useState(false)

  const refresh = useCallback(async () => {
    try {
      const [next, components] = await Promise.all([getStatus(), listComponents()])
      setStatus(next)
      setComponentState({ kind: 'ready', component: components.find(item => item.id === 'idoris') ?? null })
    } catch (cause) {
      setError(cause instanceof IdorisError ? cause.code : 'invalid_response')
      setComponentState({ kind: 'error' })
    }
  }, [])
  useEffect(() => {
    void refresh()
    const timer = window.setInterval(() => void refresh(), 2000)
    return () => window.clearInterval(timer)
  }, [refresh])

  const reconnect = async () => {
    if (pending) return
    setPending(true); setError(null)
    try {
      const current = await getStatus()
      if (current.roles.length > 0 && current.state !== 'not_configured' && current.state !== 'not_installed') {
        await submitOperation('start')
      } else {
        const discovery = await getCandidates()
        const candidate = discovery.candidates.find(item => item.identity === 'omlx' && item.authentication === 'ready' && item.models.some(isChatModelId))
        if (!candidate) throw new IdorisError('omlx_unreachable')
        const daily = candidate.models.find(isChatModelId)!
        await submitOperation('connect', {
          endpoint: candidate.endpoint, daily_model: daily, deep_model: null, save_omlx_key: false,
          replace: false, core_reserved_bytes: current.budget_suggestion?.core_reserved_bytes ?? 0,
          consent: { share_omlx_key: true, daily_model: daily, deep_model: null },
        })
      }
      await refresh()
    } catch (cause) {
      setError(cause instanceof IdorisError ? cause.code : 'invalid_response')
    } finally { setPending(false) }
  }

  const component = componentState.kind === 'ready' ? componentState.component : null
  const errorCode = error ?? status?.last_error ?? null
  const message = Object.entries(idorisErrors).find(([code]) => code === errorCode)?.[1][locale] ?? t('errorGeneric')
  return <section className="idoris-panel" aria-labelledby={`${id}-title`}>
    <h2 id={`${id}-title`}>{t('title')}</h2>
    {errorCode && <div role="alert">{message}</div>}
    {!status && componentState.kind === 'loading' ? <p>{t('loading')}</p> : !status ? <p role="alert">{t('component.error')}</p> : <>
      {componentState.kind === 'ready' && component && <p data-testid="idoris-state" data-state={component.state}>{stateText(t, component)}</p>}
      {componentState.kind === 'ready' && !component && <p data-testid="idoris-state" data-state="missing">{t('component.missing')}</p>}
      {componentState.kind === 'error' && <p role="alert">{t('component.error')}</p>}
      {status.version && <p>{t('version', { version: status.version })}</p>}
      {(status.data_port !== null || status.admin_port !== null) && <p>{t('ports', { data: status.data_port ?? t('unknown'), admin: status.admin_port ?? t('unknown') })}</p>}
      {status.last_error && <p role="status">{t('errorGeneric')}</p>}
      <button className="btn" disabled={pending} onClick={() => void reconnect()}>{pending ? t('phase.connecting') : t('connect')}</button>
      <button className="btn" disabled={pending} onClick={() => void refresh()}>{t('retry')}</button>
    </>}
  </section>
}
