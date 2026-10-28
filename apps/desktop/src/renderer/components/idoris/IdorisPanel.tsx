import { useCallback, useEffect, useId, useRef, useState } from 'react'
import { useLocale, useT } from '../../i18n'
import { cancelOperation, getCandidates, getOperation, getStatus, IdorisError, submitOperation, type IdorisCandidates, type IdorisOperation, type IdorisStatus } from './api'
import { idorisErrors, idorisMessages } from './IdorisPanel.i18n'
import './idoris.css'

// Runtime truth comes exclusively from daemon snapshots. Form choices live only
// for this mount; no installation, credential or operation state is persisted.
export default function IdorisPanel({ onOpenChat }: { onOpenChat?: () => void } = {}) {
  const t = useT(idorisMessages)
  const locale = useLocale()
  const id = useId()
  const [status, setStatus] = useState<IdorisStatus | null>(null)
  const [operation, setOperation] = useState<IdorisOperation | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [pending, setPending] = useState(false)
  const [discovery, setDiscovery] = useState<IdorisCandidates | null>(null)
  const [endpoint, setEndpoint] = useState('')
  const [confirmed, setConfirmed] = useState<IdorisCandidates | null>(null)
  const [probing, setProbing] = useState(false)
  const [daily, setDaily] = useState('')
  const [deep, setDeep] = useState('')
  const [core, setCore] = useState('')
  const [consent, setConsent] = useState(false)
  const [saveKey, setSaveKey] = useState(false)
  const mounted = useRef(false)
  const busyRef = useRef(false)
  const trackedOperation = useRef<string | null>(null)
  const poll = useRef<Promise<void> | null>(null)
  const discoveryGeneration = useRef(0)

  const reportError = useCallback((err: unknown) => {
    if (mounted.current) setError(err instanceof IdorisError ? err.code : 'invalid_response')
  }, [])
  const refresh = useCallback((): Promise<void> => {
    if (poll.current) return poll.current
    const task = (async () => {
      try {
        const next = await getStatus()
        const operationId = next.current_operation ?? trackedOperation.current
        const op = operationId ? await getOperation(operationId) : null
        if (!mounted.current) return
        setStatus(next)
        if (op) {
          setOperation(op)
          trackedOperation.current = op.result === null ? op.operation_id : null
          if (op.error_code) setError(op.error_code)
        }
      } catch (err) { reportError(err) }
    })()
    poll.current = task
    void task.finally(() => { poll.current = null })
    return task
  }, [reportError])
  useEffect(() => {
    mounted.current = true
    void refresh()
    const timer = window.setInterval(() => { if (!busyRef.current) void refresh() }, 1000)
    return () => { mounted.current = false; window.clearInterval(timer) }
  }, [refresh])

  const needsSetup = status?.state === 'not_configured' || (status?.state === 'failed' && status.roles.length === 0)
  const discover = useCallback(async (selected: string) => {
    const generation = ++discoveryGeneration.current
    setProbing(true); setConfirmed(null); setDaily(''); setDeep(''); setConsent(false)
    try {
      const result = await getCandidates(selected || undefined)
      if (!mounted.current || generation !== discoveryGeneration.current) return
      if (selected) {
        setConfirmed(result)
        setDiscovery(previous => previous ? { ...previous, candidates: previous.candidates.map(c => result.candidates.find(fresh => fresh.endpoint === c.endpoint) ?? c) } : result)
      } else setDiscovery(result)
    } catch (err) { if (generation === discoveryGeneration.current) reportError(err) }
    finally { if (mounted.current && generation === discoveryGeneration.current) setProbing(false) }
  }, [reportError])
  useEffect(() => {
    if (needsSetup) void discover(endpoint)
    return () => { discoveryGeneration.current += 1 }
  }, [needsSetup, endpoint, discover])

  const run = async (action: () => Promise<IdorisOperation>) => {
    if (busyRef.current) return
    busyRef.current = true; setPending(true); setError(null); setConsent(false)
    try {
      // Finish the outstanding read before a mutation so an older response
      // cannot replace the operation returned by this action.
      await poll.current
      const op = await action()
      if (!mounted.current) return
      setOperation(op)
      trackedOperation.current = op.operation_id
    } catch (err) { reportError(err) }
    finally {
      busyRef.current = false
      if (mounted.current) { setPending(false); void refresh() }
    }
  }
  const active = operation?.result === null || Boolean(status?.current_operation)
  const blocked = pending || active
  const candidate = confirmed?.candidates.find(c => c.endpoint === endpoint)
  const coreBytes = /^\d+$/.test(core) ? Number(core) : NaN
  const canConnect = !blocked && !probing && candidate?.identity === 'omlx' && candidate.authentication === 'ready' && candidate.models.includes(daily) && (!deep || candidate.models.includes(deep)) && Number.isSafeInteger(coreBytes) && coreBytes >= 0 && consent
  const errorCode = error ?? status?.last_error ?? (status?.state === 'instance_conflict' ? 'instance_conflict' : null)
  const errorText = Object.entries(idorisErrors).find(([code]) => code === errorCode)?.[1][locale] ?? t('errorGeneric')

  return <section className="idoris-panel" aria-labelledby={`${id}-title`}>
    <h2 id={`${id}-title`}>{t('title')}</h2>
    {errorCode && <div role="alert">{errorText}</div>}
    {!status ? <p>{t('loading')}</p> : <>
      <p data-testid="idoris-state" data-state={status.state}>{t(`state.${status.state}`)}</p>
      {status.version && <p>{t('version', { version: status.version })}</p>}
      {(status.data_port !== null || status.admin_port !== null) && <p>{t('ports', { data: status.data_port ?? t('unknown'), admin: status.admin_port ?? t('unknown') })}</p>}
      {status.roles.length > 0 && <p>{t('roles', { roles: status.roles.join(', ') })}</p>}
      {!status.roles.includes('deep') && status.roles.length > 0 && <p>{t('noDeep')}</p>}
      {active && operation && <div className="idoris-progress" role="status" aria-live="polite">
        <span>{t(`phase.${operation.phase}`)}</span>
        <progress aria-label={t(`phase.${operation.phase}`)} />
        <button className="btn" disabled={pending || !operation.cancellable} onClick={() => { void run(() => cancelOperation(operation.operation_id)) }}>{t('cancel')}</button>
      </div>}
      {status.state === 'not_installed' && <>
        <p>{t('installConsent')}</p>
        <button className="btn btn-primary" disabled={blocked} onClick={() => { void run(() => submitOperation('install', { consent: true })) }}>{t('install')}</button>
      </>}
      {needsSetup && !active && <fieldset disabled={pending} className="idoris-setup">
        <legend>{t('connect')}</legend>
        <label htmlFor={`${id}-endpoint`}>{t('endpoint')}</label>
        <select id={`${id}-endpoint`} value={endpoint} disabled={probing && !discovery} onChange={event => { setEndpoint(event.target.value); setConsent(false); setConfirmed(null); setDaily(''); setDeep('') }}>
          <option value="">{t('chooseEndpoint')}</option>
          {discovery?.candidates.map(c => <option key={c.endpoint} value={c.endpoint}>{c.endpoint} · {t(`identity.${c.identity}`)} · {t(`auth.${c.authentication}`)}</option>)}
        </select>
        {discovery && <ul className="idoris-candidates">
          {discovery.candidates.map(c => <li key={c.endpoint}>
            <code>{c.endpoint}</code>
            <p>{t('identity', { identity: t(`identity.${c.identity}`), authentication: t(`auth.${c.authentication}`) })}</p>
            {c.models.length > 0 && <p>{c.models.join(', ')}</p>}
          </li>)}
        </ul>}
        {discovery?.candidates.length === 0 && <p>{t('noCandidates')}</p>}
        {candidate && <>
          <p>{t('identity', { identity: t(`identity.${candidate.identity}`), authentication: t(`auth.${candidate.authentication}`) })}</p>
          {confirmed?.key_from_local_settings && <p>{t('localKey')}</p>}
          {(candidate.authentication === 'auth_required' || candidate.authentication === 'auth_rejected') && <p>{t('authHelp')}</p>}
          {candidate.authentication === 'unreachable' && <p>{t('noCandidates')}</p>}
          {candidate.authentication === 'ready' && candidate.models.length === 0 && <p>{t('noModels')}</p>}
        </>}
        <label htmlFor={`${id}-daily`}>{t('daily')}</label>
        <select id={`${id}-daily`} required value={daily} disabled={probing || candidate?.authentication !== 'ready'} onChange={event => { setDaily(event.target.value); setConsent(false) }}>
          <option value="">{t('chooseModel')}</option>
          {candidate?.models.map(model => <option key={model} value={model}>{model}</option>)}
        </select>
        <label htmlFor={`${id}-deep`}>{t('deep')}</label>
        <select id={`${id}-deep`} value={deep} disabled={probing || candidate?.authentication !== 'ready'} onChange={event => { setDeep(event.target.value); setConsent(false) }}>
          <option value="">{t('noDeep')}</option>
          {candidate?.models.map(model => <option key={model} value={model}>{model}</option>)}
        </select>
        <label htmlFor={`${id}-core`}>{t('core')}</label>
        <input id={`${id}-core`} type="text" inputMode="numeric" required value={core} aria-describedby={`${id}-budget-help`} onChange={event => { setCore(event.target.value); setConsent(false) }} />
        <p id={`${id}-budget-help`}>{t('budgetHelp')}</p>
        <label className="idoris-check"><input type="checkbox" checked={consent} onChange={event => setConsent(event.target.checked)} />{t('consent')}</label>
        <label className="idoris-check"><input type="checkbox" checked={saveKey} onChange={event => { setSaveKey(event.target.checked); setConsent(false) }} />{t('saveKey')}</label>
        <button className="btn btn-primary" disabled={!canConnect} onClick={() => { void run(() => submitOperation('connect', {
          endpoint, daily_model: daily, deep_model: deep || null, save_omlx_key: saveKey, replace: false, core_reserved_bytes: coreBytes,
          consent: { share_omlx_key: true, daily_model: daily, deep_model: deep || null },
        })) }}>{t('connect')}</button>
      </fieldset>}
      {status.state === 'running' && <>
        <p role="status">{operation?.action === 'connect' && operation.result === 'succeeded' ? t('verified') : t('runningHint')}</p>
        <button className="btn" disabled={!onOpenChat} onClick={onOpenChat}>{t('chat')}</button>
        <button className="btn" disabled={blocked} onClick={() => { void run(() => submitOperation('stop')) }}>{t('stop')}</button>
      </>}
      {(status.state === 'stopped' || (status.state === 'failed' && status.roles.length > 0)) && <button className="btn" disabled={blocked} onClick={() => { void run(() => submitOperation('start')) }}>{t('start')}</button>}
    </>}
    <button className="btn" disabled={pending || probing} onClick={() => { setError(null); void refresh(); if (needsSetup) void discover(endpoint) }}>{t('retry')}</button>
  </section>
}
