import { useCallback, useEffect, useRef, useState } from 'react'
import {
  getCommStatus,
  listCommContacts,
  listCommIdentities,
  listCommRelays,
  startCommDaemon,
  stopCommDaemon,
  unlockComm,
  type CommContact,
  type CommDaemonStatus,
  type CommIdentity,
  type CommRelayConfig,
} from './api'
import { translate, useT } from '../../i18n'
import { communicationMessages } from './Communication.i18n'
import './communication.css'

type WriteAction = 'start' | 'stop' | 'unlock'

function messageOf(error: unknown, secret = ''): string {
  const text = error instanceof Error ? error.message : String(error)
  return secret ? text.split(secret).join('•••') : text
}

function processLabel(state: string): string {
  const labels: Record<string, keyof typeof communicationMessages.en> = {
    stopped: 'process.stopped', starting: 'process.starting', running: 'process.running', backoff: 'process.backoff',
    gave_up: 'process.gaveUp', locked: 'process.locked', not_configured: 'process.notConfigured',
  }
  return labels[state] ? translate(communicationMessages, labels[state]) : state
}

function reasonLabel(reason: string | null | undefined): string {
  if (!reason) return ''
  const labels: Record<string, keyof typeof communicationMessages.en> = {
    no_identity: 'reason.noIdentity', no_relay: 'reason.noRelay',
    keychain_unavailable: 'reason.keychainUnavailable', password_rejected: 'reason.passwordRejected',
    binary_rejected: 'reason.binaryRejected', misconfigured: 'reason.misconfigured',
    not_configured: 'reason.notConfigured', locked: 'reason.locked',
  }
  return translate(communicationMessages, labels[reason] ?? 'reason.other')
}

function DaemonStatusView({ status, fresh }: { status: CommDaemonStatus | null; fresh: boolean }) {
  const t = useT(communicationMessages)
  if (!status) return <p className="comm-muted">{t('process.unloaded')}</p>
  const process = status.process
  const probe = status.relay_probe
  const catchUp = status.catch_up
  return (
    <div className="comm-status-grid">
      <section className="comm-status-item">
        <h3>{t('process.title')}</h3>
        <p><span className={`comm-state${fresh && process.state === 'running' ? ' comm-state-running' : ''}`}>{processLabel(process.state)}</span></p>
        {process.reason && <p className="comm-muted">{t('process.reason', { code: process.reason, reason: reasonLabel(process.reason) })}</p>}
        <p className="comm-muted">{t('process.metrics', { generation: process.generation, failures: process.consecutive_failures })}</p>
      </section>
      <section className="comm-status-item">
        <h3>{t('probe.title')}</h3>
        {probe ? <>
          <p>{probe.connected ? t('probe.success') : t('probe.failure')}</p>
          {probe.url && <p className="comm-muted">{probe.url}</p>}
          {probe.at_ms !== null && <p className="comm-muted">{t('probe.timestamp', { time: probe.at_ms })}</p>}
          {probe.error && <p className="comm-muted">{probe.error}</p>}
        </> : <p className="comm-muted">{t('probe.empty')}</p>}
      </section>
      <section className="comm-status-item">
        <h3>{t('catchUp.title')}</h3>
        <p>{catchUp.state === 'incomplete' ? t('catchUp.incomplete') : t('catchUp.unknown')}</p>
        {catchUp.last_incomplete_at_ms !== null && <p className="comm-muted">{t('catchUp.timestamp', { time: catchUp.last_incomplete_at_ms })}</p>}
      </section>
    </div>
  )
}

export default function CommunicationPage() {
  const t = useT(communicationMessages)
  const [identities, setIdentities] = useState<CommIdentity[] | null>(null)
  const [contacts, setContacts] = useState<CommContact[] | null>(null)
  const [relays, setRelays] = useState<CommRelayConfig | null>(null)
  const [daemon, setDaemon] = useState<CommDaemonStatus | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [refreshing, setRefreshing] = useState(false)
  const [pending, setPending] = useState<WriteAction | null>(null)
  const [password, setPassword] = useState('')
  const [remember, setRemember] = useState(false)
  const [actionError, setActionError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const sequence = useRef(0)
  const mounted = useRef(false)

  const refresh = useCallback(async () => {
    const token = ++sequence.current
    setRefreshing(true)
    setLoadError(null)
    const results = await Promise.allSettled([
      listCommIdentities(), listCommContacts(), listCommRelays(), getCommStatus(),
    ])
    if (!mounted.current || token !== sequence.current) return
    const failures: string[] = []
    if (results[0].status === 'fulfilled') setIdentities(results[0].value)
    else failures.push(translate(communicationMessages, 'error.identities', { message: messageOf(results[0].reason) }))
    if (results[1].status === 'fulfilled') setContacts(results[1].value)
    else failures.push(translate(communicationMessages, 'error.contacts', { message: messageOf(results[1].reason) }))
    if (results[2].status === 'fulfilled') setRelays(results[2].value)
    else failures.push(translate(communicationMessages, 'error.relays', { message: messageOf(results[2].reason) }))
    if (results[3].status === 'fulfilled') setDaemon(results[3].value)
    else failures.push(translate(communicationMessages, 'error.process', { message: messageOf(results[3].reason) }))
    setLoadError(failures.length ? translate(communicationMessages, 'error.refresh', { details: failures.join(translate(communicationMessages, 'error.separator')) }) : null)
    setRefreshing(false)
  }, [])

  useEffect(() => {
    mounted.current = true
    void refresh()
    return () => { mounted.current = false; sequence.current++ }
  }, [refresh])

  const runAction = async (action: WriteAction) => {
    if (pending) return
    const token = ++sequence.current // invalidate a read already in flight
    setPending(action)
    setActionError(null)
    setNotice(null)
    const submittedPassword = action === 'unlock' ? password : ''
    if (action === 'unlock') setPassword('')
    try {
      if (action === 'start') {
        await startCommDaemon()
        if (mounted.current && token === sequence.current) setNotice(t('notice.start'))
      } else if (action === 'stop') {
        await stopCommDaemon()
        if (mounted.current && token === sequence.current) setNotice(t('notice.stop'))
      } else {
        const result = await unlockComm(submittedPassword, remember)
        if (mounted.current && token === sequence.current) {
          setNotice(result.remembered ? t('notice.remembered') : t('notice.session'))
        }
      }
    } catch (error) {
      if (mounted.current && token === sequence.current) {
        const code = typeof error === 'object' && error !== null && 'code' in error
          ? (error as { code?: unknown }).code : undefined
        const knownCodes = ['binary_rejected', 'not_configured', 'locked', 'password_rejected', 'keychain_unavailable']
        const reason = typeof code === 'string' && knownCodes.includes(code)
          ? t('error.reasonPrefix', { code, reason: reasonLabel(code) }) : ''
        setActionError(`${reason}${messageOf(error, submittedPassword)}`)
      }
    } finally {
      if (mounted.current) {
        if (action === 'unlock') setPassword('')
        setPending(null)
        void refresh()
      }
    }
  }

  const isFresh = !loadError && !refreshing && daemon !== null
  return (
    <main className="content comm-page">
      <header className="comm-heading">
        <div><h1>{t('page.title')}</h1><p>{t('page.subtitle')}</p></div>
        <button type="button" onClick={() => void refresh()} disabled={refreshing || pending !== null}>
          {refreshing ? t('action.refreshing') : t('action.refresh')}
        </button>
      </header>

      {loadError && <div role="alert" className="comm-alert">{loadError}</div>}
      {actionError && <div role="alert" className="comm-alert">{t('error.action', { message: actionError })}</div>}
      {notice && <div role="status" className="comm-notice">{notice}</div>}

      <section className="comm-card">
        <h2>{t('section.identities')}</h2>
        {identities === null ? <p className="comm-muted">{t('empty.data')}</p> : identities.length === 0 ? <p className="comm-muted">{t('empty.identities')}</p> : (
          <div className="comm-table-wrap"><table><thead><tr><th>{t('table.nickname')}</th><th>{t('table.publicKey')}</th><th>{t('table.default')}</th><th>{t('table.keystore')}</th></tr></thead><tbody>
            {identities.map((identity) => <tr key={identity.nickname}>
              <td>{identity.nickname}</td><td className="comm-key">{identity.npub}</td>
              <td>{identity.default ? t('table.default') : '—'}</td><td>{identity.encrypted ? t('value.encrypted') : t('value.unencrypted')}</td>
            </tr>)}
          </tbody></table></div>
        )}
      </section>

      <section className="comm-card">
        <h2>{t('section.contacts')}</h2>
        {contacts === null ? <p className="comm-muted">{t('empty.data')}</p> : contacts.length === 0 ? <p className="comm-muted">{t('empty.contacts')}</p> : (
          <div className="comm-table-wrap"><table><thead><tr><th>{t('table.nickname')}</th><th>{t('table.publicKey')}</th><th>{t('table.role')}</th></tr></thead><tbody>
            {contacts.map((contact) => <tr key={`${contact.nickname}:${contact.npub}`}>
              <td>{contact.nickname}</td><td className="comm-key">{contact.npub}</td><td>{contact.role || '—'}</td>
            </tr>)}
          </tbody></table></div>
        )}
      </section>

      <section className="comm-card">
        <h2>{t('section.relay')}</h2>
        {relays === null ? <p className="comm-muted">{t('empty.data')}</p> : <>
          <p>{t('relay.source', { source: relays.source, status: relays.configured ? t('value.configured') : t('value.notConfigured') })}</p>
          {relays.relays.length ? <ul>{relays.relays.map((relay) => <li key={relay}>{relay}</li>)}</ul> : <p className="comm-muted">{t('empty.relay')}</p>}
        </>}
      </section>

      <section className="comm-card">
        <div className="comm-section-heading"><h2>{t('section.service')}</h2><span className={isFresh && daemon?.process.state === 'running' ? 'comm-fresh-indicator' : 'comm-muted'}>{isFresh ? t('data.fresh') : t('data.stale')}</span></div>
        <DaemonStatusView status={daemon} fresh={isFresh} />
        <div className="comm-actions">
          <button type="button" onClick={() => void runAction('start')} disabled={pending !== null}>{t('action.start')}</button>
          <button type="button" onClick={() => void runAction('stop')} disabled={pending !== null}>{t('action.stop')}</button>
        </div>
        <form className="comm-unlock" onSubmit={(event) => { event.preventDefault(); void runAction('unlock') }}>
          <label>{t('password.label')}<input type="password" autoComplete="off" value={password} onChange={(event) => setPassword(event.target.value)} /></label>
          <label className="comm-checkbox"><input type="checkbox" checked={remember} onChange={(event) => setRemember(event.target.checked)} />{t('password.remember')}</label>
          <button type="submit" disabled={pending !== null || password.length === 0}>{pending === 'unlock' ? t('action.unlocking') : t('action.unlock')}</button>
        </form>
      </section>
    </main>
  )
}
