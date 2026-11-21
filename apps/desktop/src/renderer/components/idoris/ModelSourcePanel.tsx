import { useEffect, useId, useRef, useState } from 'react'
import { useLocale, useT } from '../../i18n'
import { isChatModelId } from '../../pages/settings/chatModels'
import { getCandidates, getOperation, getStatus, getUpstreamModels, IdorisError, submitOperation, type IdorisCandidates, type IdorisOperation, type IdorisStatus, type ModelSource } from './api'
import { idorisErrors } from './IdorisPanel.i18n'
import { modelSourceMessages } from './ModelSourcePanel.i18n'
import './idoris.css'

const errorCode = (cause: unknown) => cause instanceof IdorisError ? cause.code : 'invalid_response'

export default function ModelSourcePanel() {
  const t = useT(modelSourceMessages)
  const locale = useLocale()
  const id = useId()
  const mounted = useRef(false)
  const locked = useRef(true)
  const autoConnect = useRef(false)
  const [omlxSetup, setOmlxSetup] = useState<IdorisCandidates['setup']>()
  const [status, setStatus] = useState<IdorisStatus | null>(null)
  const [source, setSource] = useState<ModelSource>('omlx')
  const [endpoint, setEndpoint] = useState('')
  const [models, setModels] = useState<string[]>([])
  const [daily, setDaily] = useState('')
  const [pending, setPending] = useState<'loading' | 'reading' | 'applying' | null>('loading')
  const [operation, setOperation] = useState<IdorisOperation | null>(null)
  const [authEndpoint, setAuthEndpoint] = useState<string>()
  const [error, setError] = useState<string | null>(null)
  const [switched, setSwitched] = useState(false)

  useEffect(() => {
    mounted.current = true
    void getStatus().then(next => {
      if (!mounted.current) return
      setStatus(next); setSource(next.source ?? 'omlx')
    }).catch(cause => { if (mounted.current) setError(errorCode(cause)) })
      .finally(() => { if (mounted.current) { locked.current = false; setPending(null) } })
    return () => { mounted.current = false }
  }, [])

  useEffect(() => {
    if (!operation) return
    if (operation.result !== null) {
      if (operation.result === 'succeeded') {
        setSwitched(true)
        if (autoConnect.current) {
          void getCandidates().then(result => { if (mounted.current) setOmlxSetup(result.setup) })
            .catch(cause => { if (mounted.current) setError(errorCode(cause)) })
        }
        setStatus(current => current && { ...current, roles: ['daily'], source })
      } else {
        setAuthEndpoint(operation.setup_endpoint)
        setError(operation.setup_code ?? operation.error_code ?? (operation.result === 'cancelled' ? 'cancelled' : 'verify_failed'))
      }
      locked.current = false; setPending(null); setOperation(null)
      return
    }
    let disposed = false
    const timer = window.setTimeout(() => {
      void getOperation(operation.operation_id).then(next => { if (!disposed) setOperation(next) })
        .catch(cause => {
          if (disposed) return
          setError(errorCode(cause)); setOperation(null); locked.current = false; setPending(null)
        })
    }, 1000)
    return () => { disposed = true; window.clearTimeout(timer) }
  }, [operation, source])

  async function readModels() {
    if (locked.current) return
    locked.current = true; setPending('reading'); setError(null); setSwitched(false)
    setModels([]); setDaily('')
    try {
      const next = await getUpstreamModels(endpoint.trim())
      if (!mounted.current) return
      setModels(next.models); setDaily(next.models[0] ?? '')
      if (next.models.length === 0) setError('noModels')
    } catch (cause) { if (mounted.current) setError(errorCode(cause)) }
    finally { if (mounted.current) { locked.current = false; setPending(null) } }
  }

  async function apply() {
    if (locked.current || !status) return
    locked.current = true; setPending('applying'); setError(null); setSwitched(false)
    try {
      let targetEndpoint = endpoint.trim()
      let targetDaily = daily
      autoConnect.current = false
      if (source === 'omlx') {
        const discovery = await getCandidates()
        if (!mounted.current) return
        const candidate = discovery.candidates.find(item => item.identity === 'omlx' && item.authentication === 'ready')
        const model = candidate?.models.find(isChatModelId)
        if (!candidate || !model) throw new IdorisError('noOmlx')
        autoConnect.current = candidate.endpoint === '' || discovery.setup !== undefined
        setOmlxSetup(discovery.setup)
        targetEndpoint = candidate.endpoint; targetDaily = model
      }
      const replace = status.roles.length > 0
      const core = status.budget_suggestion?.core_reserved_bytes
      if (!replace && core === undefined) throw new IdorisError('noBudget')
      const next = await submitOperation('connect', {
        source, endpoint: targetEndpoint, daily_model: targetDaily, replace, save_omlx_key: false,
        ...(!replace ? { core_reserved_bytes: core } : {}),
      })
      if (mounted.current) setOperation(next)
    } catch (cause) {
      if (mounted.current) { setError(errorCode(cause)); locked.current = false; setPending(null) }
    }
  }

  const message = Object.entries(modelSourceMessages[locale]).find(([code]) => code === error)?.[1]
    ?? Object.entries(idorisErrors).find(([code]) => code === error)?.[1][locale] ?? error
  return <section className="idoris-panel" aria-labelledby={`${id}-title`}>
    <h2 id={`${id}-title`}>{t('title')}</h2>
    <fieldset className="idoris-setup" disabled={pending !== null} aria-labelledby={`${id}-title`}>
      <label className="idoris-check"><input type="radio" name={`${id}-source`} checked={source === 'omlx'} onChange={() => { setSource('omlx'); setError(null); setSwitched(false) }} />{t('omlx')}</label>
      <label className="idoris-check"><input type="radio" name={`${id}-source`} checked={source === 'openai_compat'} onChange={() => { setSource('openai_compat'); setError(null); setSwitched(false) }} />{t('compat')}</label>
      {source === 'openai_compat' && <>
        <label htmlFor={`${id}-endpoint`}>{t('endpoint')}</label>
        <input id={`${id}-endpoint`} type="text" placeholder={t('placeholder')} value={endpoint} onChange={event => { setEndpoint(event.target.value); setModels([]); setDaily(''); setError(null); setSwitched(false) }} />
        <button className="btn" disabled={!endpoint.trim()} onClick={() => void readModels()}>{t('read')}</button>
        <label htmlFor={`${id}-daily`}>{t('daily')}</label>
        <select id={`${id}-daily`} value={daily} onChange={event => setDaily(event.target.value)}>
          <option value="">{t('choose')}</option>
          {models.map(model => <option key={model} value={model}>{model}</option>)}
        </select>
      </>}
      <button className="btn" disabled={!status || (source === 'openai_compat' && !daily)} onClick={() => void apply()}>{t('apply')}</button>
    </fieldset>
    {pending && <p role="status">{t(pending)}</p>}
    {switched && <p role="status">{t('switched')}</p>}
    {source === 'omlx' && omlxSetup && <p data-testid="omlx-setup">{omlxSetup.endpoint} · {omlxSetup.mode} · {t(omlxSetup.engine.installed ? 'engineInstalled' : 'engineMissing')}</p>}
    {error && <p role="alert">{message}{error === 'auth_required' && authEndpoint && ` · ${authEndpoint}`}</p>}
    <p style={{ fontSize: 12, color: 'var(--muted)' }}>{t('hint')}</p>
  </section>
}
