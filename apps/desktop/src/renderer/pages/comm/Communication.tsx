import { useCallback, useEffect, useRef, useState } from 'react'
import {
  CommApiError,
  createCommIdentity, importCommHome, setCommRelay, probeCommRelay,
  getCommStatus,
  installComm,
  addCommContact,
  exportCommIdentity,
  getCommStatus as pollCommStatus,
  listCommContacts,
  listCommHistory,
  listCommIdentities,
  pickCommBackup,
  restoreCommIdentity,
  revealCommExport,
  sendCommMessage,
  listCommRelays,
  startCommDaemon,
  stopCommDaemon,
  unlockComm,
  type CommContact,
  type CommHistoryItem,
  type CommDaemonStatus,
  type CommIdentity,
  type CommRelayConfig,
} from './api'
import { translate, useT } from '../../i18n'
import { communicationMessages } from './Communication.i18n'
import './communication.css'
import { getDefaultRelays } from './relayDefaults'

type WriteAction = 'start' | 'stop' | 'unlock' | 'install' | 'create' | 'import' | 'relay' | 'export' | 'contact' | 'send' | 'restore'

/** Daemon states that settle on their own: the page polls until they do. */
const TRANSITIONAL_STATES = ['starting', 'backoff']
const POLL_INTERVAL_MS = 1000
const POLL_MAX_TICKS = 30

function messageOf(error: unknown, secret = ''): string {
  const text = error instanceof Error ? error.message : String(error)
  return secret ? text.split(secret).join('•••') : text
}

function shortNpub(npub: string): string {
  return npub.length > 20 ? `${npub.slice(0, 12)}…${npub.slice(-6)}` : npub
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
        <p><span className={`comm-state${fresh && process.state === 'running' ? ' comm-state-running' : ''}`} aria-busy={TRANSITIONAL_STATES.includes(process.state) || undefined}>
          {TRANSITIONAL_STATES.includes(process.state) && <span className="comm-spinner" data-testid="comm-spinner" aria-hidden="true">⟳</span>}
          {processLabel(process.state)}
        </span></p>
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
  const [notConfigured, setNotConfigured] = useState<{ kind: 'binary' | 'identity' | 'config'; detail: string } | null>(null)
  const [refreshing, setRefreshing] = useState(false)
  const [pending, setPending] = useState<WriteAction | null>(null)
  const [remember, setRemember] = useState(false)
  const [actionError, setActionError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [exportedPath, setExportedPath] = useState<string | null>(null)
  const [warnings, setWarnings] = useState<string[]>([])
  const [nickname, setNickname] = useState('')
  const [importOpen, setImportOpen] = useState(false)
  const defaults = getDefaultRelays()
  const [selectedRelay, setSelectedRelay] = useState('')
  const [handshakeFailed, setHandshakeFailed] = useState(false)
  const [unlockFailed, setUnlockFailed] = useState(false)
  const [unlockError, setUnlockError] = useState<string | null>(null)
  const [copied, setCopied] = useState<string | null>(null)
  const [contactName, setContactName] = useState('')
  const [contactNpub, setContactNpub] = useState('')
  const [contactRole, setContactRole] = useState('')
  const [contactError, setContactError] = useState<string | null>(null)
  const [messageTo, setMessageTo] = useState('')
  const [messageText, setMessageText] = useState('')
  const contactNameRef = useRef<HTMLInputElement>(null)
  const [restore, setRestore] = useState<{ ticket: string; fileName: string } | null>(null)
  const [restoreNickname, setRestoreNickname] = useState('')
  const [restoreDefault, setRestoreDefault] = useState(false)
  const [history, setHistory] = useState<CommHistoryItem[] | null>(null)
  // A stale choice (no longer offered after a refresh) falls back to the default option.
  const relayOptions = [...defaults.map((relay) => relay.url), ...(relays?.relays ?? [])]
  const relayUrl = (selectedRelay && relayOptions.includes(selectedRelay) ? selectedRelay : '')
    || (relays?.configured ? relays.relays[0] : '') || defaults[0]?.url || ''
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
    const setupMessages: string[] = []
    // The daemon reports setup prerequisites and configuration errors with
    // `not_configured`; their messages determine the guidance below.
    const fail = (key: 'error.identities' | 'error.contacts' | 'error.relays' | 'error.process', reason: unknown) => {
      if (reason instanceof CommApiError && reason.code === 'not_configured') { setupMessages.push(reason.message); return }
      failures.push(translate(communicationMessages, key, { message: messageOf(reason) }))
    }
    if (results[0].status === 'fulfilled') setIdentities(results[0].value)
    else fail('error.identities', results[0].reason)
    if (results[1].status === 'fulfilled') setContacts(results[1].value)
    else fail('error.contacts', results[1].reason)
    if (results[2].status === 'fulfilled') setRelays(results[2].value)
    else fail('error.relays', results[2].reason)
    if (results[3].status === 'fulfilled') setDaemon(results[3].value)
    else fail('error.process', results[3].reason)
    setNotConfigured(setupMessages.length === 0 ? null : {
      kind: setupMessages.some((m) => /A24_COMM_|not a recognized|misconfigur|invalid|malformed|unsupported|unrecognized/i.test(m))
        ? 'config' : setupMessages.some((m) => /hyphae binary|A24_HYPHAE_BIN/i.test(m)) ? 'binary' : 'identity',
      detail: [...new Set(setupMessages)].join(translate(communicationMessages, 'error.separator')),
    })
    setLoadError(failures.length ? translate(communicationMessages, 'error.refresh', { details: failures.join(translate(communicationMessages, 'error.separator')) }) : null)
    setRefreshing(false)
  }, [])

  const loadHistory = useCallback(async () => {
    try {
      const items = await listCommHistory(20)
      if (mounted.current) setHistory(items)
    } catch {
      // The list is a convenience; the status cards already report real failures.
      if (mounted.current) setHistory(null)
    }
  }, [])

  useEffect(() => {
    mounted.current = true
    void refresh()
    void loadHistory()
    return () => { mounted.current = false; sequence.current++ }
  }, [refresh, loadHistory])

  // `starting` becomes `running` on its own once the child has stayed alive
  // for a few seconds; keep the card current until the state settles (bounded).
  const processState = daemon?.process.state
  useEffect(() => {
    if (!processState || !TRANSITIONAL_STATES.includes(processState)) return
    let ticks = 0
    const timer = setInterval(() => {
      ticks += 1
      if (ticks > POLL_MAX_TICKS) { clearInterval(timer); return }
      pollCommStatus().then((next) => { if (mounted.current) setDaemon(next) }).catch(() => undefined)
    }, POLL_INTERVAL_MS)
    return () => clearInterval(timer)
  }, [processState])

  const runAction = async (action: WriteAction) => {
    if (pending) return
    const token = ++sequence.current // invalidate a read already in flight
    setPending(action)
    setActionError(null)
    setNotice(null)
    setExportedPath(null)
    if (action === 'unlock') setUnlockError(null)
    try {
      if (action === 'install') {
        const result = await installComm()
        if (mounted.current && token === sequence.current) {
          setNotice(result.restart_required ? t('notice.restart', { reason: result.reason ?? '' }) : t('notice.installed'))
        }
      } else if (action === 'create') {
        await createCommIdentity(nickname.trim())
      } else if (action === 'import') {
        await importCommHome()
      } else if (action === 'relay') {
        // PUT replaces the whole list server-side (and restarts the daemon):
        // keep the existing entries, and skip the write if nothing changes.
        const existing = relays?.configured ? relays.relays : []
        if (existing[0] !== relayUrl) {
          await setCommRelay([relayUrl, ...existing.filter((url) => url !== relayUrl)].slice(0, 8))
        }
        setHandshakeFailed(true)
        const probe = await probeCommRelay(relayUrl)
        if (probe.connected) setHandshakeFailed(false)
        if (!probe.connected) throw new Error(`${t('probe.failure')}${probe.error ? `: ${probe.error}` : ''}`)
        if (mounted.current && token === sequence.current) setNotice(t('probe.success'))
      } else if (action === 'start') {
        await startCommDaemon()
        if (mounted.current && token === sequence.current) setNotice(t('notice.start'))
      } else if (action === 'stop') {
        await stopCommDaemon()
        if (mounted.current && token === sequence.current) setNotice(t('notice.stop'))
      } else {
        const result = await unlockComm(remember)
        if (mounted.current && token === sequence.current) {
          setNotice(result.remembered ? t('notice.remembered') : t('notice.session'))
          setUnlockFailed(false)
        }
      }
    } catch (error) {
      if (mounted.current && token === sequence.current) {
        const code = typeof error === 'object' && error !== null && 'code' in error
          ? error.code : undefined
        // Dismissing a native step (folder picker, confirm, password window) is not an error.
        if (code === 'cancelled') return
        if (action === 'unlock') {
          setUnlockFailed(true)
          // A rejected password is retried inside main's window; reaching here
          // means the keystore could not be unlocked for another reason.
        }
        const knownCodes = ['binary_rejected', 'not_configured', 'locked', 'password_rejected', 'keychain_unavailable']
        const reason = typeof code === 'string' && knownCodes.includes(code)
          ? t('error.reasonPrefix', { code, reason: reasonLabel(code) }) : ''
        setActionError(`${reason}${messageOf(error)}`)
      }
    } finally {
      if (mounted.current) {
        setPending(null)
        void refresh()
      }
    }
  }

  const copyNpub = async (npub: string) => {
    try {
      await navigator.clipboard.writeText(npub)
      setCopied(npub)
      setTimeout(() => { if (mounted.current) setCopied((current) => (current === npub ? null : current)) }, 2000)
    } catch {
      setActionError(t('identity.copyFailed'))
    }
  }

  // Stable daemon codes get their own wording; everything else shows the message.
  const errorText = (error: unknown): string => {
    const code = error instanceof CommApiError ? error.code : undefined
    if (code === 'hyphae_too_old') return t('error.hyphaeTooOld')
    if (code === 'nickname_exists') return t('restore.nicknameExists')
    if (code === 'invalid_key') return t('restore.invalidKey')
    if (code === 'publish_conflict') return t('export.publishConflict')
    if (code === 'permission_denied') return t('export.permissionDenied')
    if (code === 'filesystem_unsupported') return t('export.filesystemUnsupported')
    if (code === 'keystore_locked') return t('restore.keystoreLocked')
    if (code === 'export_in_progress' || (code === 'conflict' && messageOf(error).startsWith('export_in_progress'))) return t('error.exportInProgress')
    return messageOf(error)
  }

  const chooseBackup = async () => {
    if (pending) return
    setPending('restore')
    setActionError(null)
    setNotice(null)
    setExportedPath(null)
    try {
      const picked = await pickCommBackup()
      if (picked && mounted.current) {
        setRestore(picked)
        // A nickname suggested from the file name; the user can change it.
        // eslint-disable-next-line no-control-regex -- strip control characters from the suggested name
        const stem = picked.fileName.replace(/\.(ncryptsec|txt)$/i, '').replace(/[\u0000-\u001f\u007f]/g, '').trim().slice(0, 64)
        setRestoreNickname(stem)
        setRestoreDefault(!identities?.length)
      }
    } catch (error) {
      if (mounted.current) setActionError(errorText(error))
    } finally {
      if (mounted.current) setPending(null)
    }
  }

  const submitRestore = async () => {
    if (pending || !restore || !restoreNickname.trim()) return
    setPending('restore')
    setActionError(null)
    setNotice(null)
    setExportedPath(null)
    try {
      // The backup password is asked for by the main process, not by this page.
      const result = await restoreCommIdentity(restore.ticket, restoreNickname.trim(), restoreDefault)
      if (mounted.current) {
        setNotice(result.status === 'already_present'
          ? (result.nickname ? t('restore.alreadyAs', { nickname: result.nickname }) : t('restore.already'))
          : t('restore.done', { nickname: result.nickname ?? restoreNickname.trim() }))
        setRestore(null)
      }
    } catch (error) {
      // Cancelling the password window keeps the form so the user can try again.
      const cancelled = error instanceof CommApiError && error.code === 'cancelled'
      if (mounted.current && !cancelled) setActionError(errorText(error))
    } finally {
      if (mounted.current) { setPending(null); void refresh() }
    }
  }

  const restoreUi = <>
    <button type="button" disabled={pending !== null} onClick={() => void chooseBackup()}>{t('restore.button')}</button>
    {restore && <form className="comm-setup-form" aria-label={t('restore.button')} onSubmit={(event) => { event.preventDefault(); void submitRestore() }}>
      <p className="comm-muted">{t('restore.file', { file: restore.fileName })}</p>
      <label>{t('restore.nickname')}<input value={restoreNickname} onChange={(event) => setRestoreNickname(event.target.value)} /></label>
      <label className="comm-checkbox"><input type="checkbox" checked={restoreDefault} onChange={(event) => setRestoreDefault(event.target.checked)} />{t('restore.makeDefault')}</label>
      <button type="submit" disabled={pending !== null || !restoreNickname.trim()}>{t('restore.submit')}</button>
      <button type="button" onClick={() => setRestore(null)}>{t('restore.cancel')}</button>
    </form>}
  </>

  const exportKey = async (name: string) => {
    if (pending) return
    setPending('export')
    setActionError(null)
    setNotice(null)
    setExportedPath(null)
    setWarnings([])
    try {
      // The backup password is asked for by the main process, not by this page.
      const result = await exportCommIdentity(name)
      if (mounted.current) {
        setExportedPath(result.path)
        setNotice(t('export.done', { path: result.path }))
        // The backup is saved either way; these say what could not be confirmed.
        setWarnings(result.warnings.map((w) => (w.code === 'cleanup_failed'
          ? t('export.warnCleanup', { path: w.path ?? '' })
          : t('export.warnDurability'))))
      }
    } catch (error) {
      // Declining any native step is not an error.
      const cancelled = error instanceof CommApiError && error.code === 'cancelled'
      if (mounted.current && !cancelled) setActionError(errorText(error))
    } finally {
      if (mounted.current) setPending(null)
    }
  }

  const revealExported = async (filePath: string) => {
    try {
      await revealCommExport(filePath)
    } catch (error) {
      if (mounted.current) setActionError(errorText(error))
    }
  }

  const submitContact = async () => {
    if (pending) return
    const npubValue = contactNpub.trim()
    if (!npubValue.startsWith('npub1')) { setContactError(t('contact.npubInvalid')); return }
    setContactError(null)
    setPending('contact')
    setActionError(null)
    setNotice(null)
    setExportedPath(null)
    try {
      await addCommContact(contactName.trim(), npubValue, contactRole.trim())
      if (mounted.current) {
        setContactName(''); setContactNpub(''); setContactRole('')
        setNotice(t('contact.added'))
      }
    } catch (error) {
      if (mounted.current) setActionError(messageOf(error))
    } finally {
      if (mounted.current) { setPending(null); void refresh() }
    }
  }

  const submitMessage = async () => {
    if (pending) return
    const to = messageTo.trim()
    if (!to || !messageText.trim()) return
    // A contact nickname or any raw npub; contacts are only a local name book.
    if (!to.startsWith('npub1') && !contacts?.some((c) => c.nickname === to)) {
      setActionError(t('message.recipientInvalid'))
      return
    }
    setPending('send')
    setActionError(null)
    setNotice(null)
    setExportedPath(null)
    try {
      const result = await sendCommMessage(to, messageText)
      if (mounted.current) {
        setMessageText('')
        setNotice(result.published_to > 0 ? t('message.sent', { count: result.published_to }) : t('message.queued'))
      }
    } catch (error) {
      if (mounted.current) setActionError(messageOf(error))
    } finally {
      if (mounted.current) { setPending(null); void loadHistory() }
    }
  }

  // `gave_up` has no child but Stop still means something there: it moves the
  // supervisor to `stopped` and switches autostart off, ending the retry
  // cycle. `locked` has nothing to stop; Unlock is the way forward.
  const serviceStoppable = daemon?.process.state === 'running'
    || daemon?.process.state === 'starting'
    || daemon?.process.state === 'backoff'
    || daemon?.process.state === 'gave_up'
  const stopTitle = serviceStoppable ? undefined
    : daemon === null ? t('action.stopUnavailable')
    : daemon.process.state === 'locked' ? t('action.stopLocked')
    : t('action.stopIdle')
  const revealLabel = (() => {
    const hint = typeof navigator === 'undefined' ? '' : (navigator.platform || navigator.userAgent)
    if (/mac|darwin|iphone|ipad/i.test(hint)) return t('export.revealMac')
    if (/\bwin/i.test(hint)) return t('export.revealWin')
    if (/linux|x11|bsd/i.test(hint)) return t('export.revealLinux')
    return t('export.reveal')
  })()
  const needsUnlock = daemon !== null && (daemon.process.state === 'locked'
    || daemon.process.reason === 'password_rejected' || daemon.process.reason === 'keychain_unavailable')
  const showUnlock = needsUnlock || unlockFailed
  const isFresh = !loadError && notConfigured === null && !refreshing && daemon !== null
  const setupOnly = notConfigured !== null && !loadError
  // Installation progress is transient IPC state. All completed steps come
  // from daemon reads, including after a write fails or the page remounts.
  const setupState = pending === 'install' ? 'installing'
    : notConfigured?.kind === 'binary' ? 'not_installed'
    : identities !== null && daemon !== null && relays !== null && !loadError && notConfigured === null
      ? !identities.some((identity) => identity.default) ? 'no_identity'
        : !relays.configured ? 'no_relay' : 'ready'
      : null
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
      {pending === 'install' && <p role="status">{t('action.installing')}</p>}
      {notice && <div role="status" className="comm-notice">
        <span>{notice}</span>
        {exportedPath && <button type="button" onClick={() => void revealExported(exportedPath)}>{revealLabel}</button>}
      </div>}
      {warnings.map((warning) => <div key={warning} role="status" className="comm-warning">{warning}</div>)}

      {setupState && <section className="comm-card" data-testid="comm-setup" data-state={setupState} aria-label={t('wizard.title')}>
        <h2>{t('wizard.title')}</h2>
        <ol className="comm-steps">
          <li aria-current={setupState === 'not_installed' || setupState === 'installing' ? 'step' : undefined}>{t('wizard.download')}{setupState !== 'not_installed' && setupState !== 'installing' ? <span className="comm-step-mark"> ✓</span> : null}</li>
          <li aria-current={setupState === 'no_identity' ? 'step' : undefined}>{t('wizard.identity')}{setupState === 'no_relay' || setupState === 'ready' ? <span className="comm-step-mark"> ✓</span> : null}</li>
          <li aria-current={setupState === 'no_relay' ? 'step' : undefined}>{t('wizard.relay')}{setupState === 'ready' && !handshakeFailed ? <span className="comm-step-mark"> ✓</span> : null}</li>
        </ol>
        {setupState === 'no_identity' && <>
          <form className="comm-setup-form" onSubmit={(event) => { event.preventDefault(); void runAction('create') }}>
            <label>{t('wizard.name')}<input value={nickname} onChange={(event) => setNickname(event.target.value)} /></label>
            <button disabled={pending !== null || refreshing || !nickname.trim()}>{t('wizard.identity')}</button>
          </form>
          <button type="button" disabled={pending !== null} onClick={() => setImportOpen(!importOpen)}>{t('wizard.importKey')}</button>
          {restoreUi}
          {importOpen && <div className="comm-setup-form">
            <p className="comm-muted">{t('wizard.importGuide')}</p>
            <button type="button" disabled={pending !== null || refreshing} onClick={() => void runAction('import')}>{t('wizard.import')}</button>
          </div>}
        </>}
        {(setupState === 'no_relay' || setupState === 'ready') && <form className="comm-setup-form" onSubmit={(event) => { event.preventDefault(); void runAction('relay') }}>
          <label>{t('wizard.relayServer')}<select value={relayUrl} onChange={(event) => setSelectedRelay(event.target.value)}>
            {defaults.map((relay) => <option key={relay.name} value={relay.url}>{relay.name} · {relay.url}</option>)}
            {relays?.relays.filter((url) => !defaults.some((relay) => relay.url === url)).map((url) => <option key={url} value={url}>{url}</option>)}
          </select></label>
          <button disabled={pending !== null || refreshing || !relayUrl}>{t('wizard.relay')}</button>
        </form>}
        {setupState === 'ready' && <p className="comm-muted">{handshakeFailed ? t('wizard.relaySavedFailed') : t('wizard.ready')}</p>}
      </section>}

      {notConfigured && (
        <section className="comm-card" role="status" aria-label={t(notConfigured.kind === 'config' ? 'setup.configTitle' : 'setup.title')}>
          <h2>{t(notConfigured.kind === 'config' ? 'setup.configTitle' : 'setup.title')}</h2>
          {notConfigured.kind === 'config' ? <p className="comm-muted">{t('setup.configGuide')}</p> : notConfigured.kind === 'binary' ? <>
            <p className="comm-muted">{t('setup.binaryGuide')}</p>
            {daemon?.installable === true ? <button type="button" disabled={pending !== null}
              onClick={() => void runAction('install')}>
              {pending === 'install' ? t('action.installing') : t('action.install')}
            </button> : <p className="comm-muted">{t('setup.unsupported')}</p>}
          </> : <>
            <p className="comm-muted">{t('setup.guide')}</p>
            <p><code>{t('setup.command')}</code></p>
          </>}
          {notConfigured.detail && <p className="comm-muted">{t('setup.detail', { detail: notConfigured.detail })}</p>}
        </section>
      )}

      {!setupOnly && <>
      <section className="comm-card">
        <h2>{t('section.identities')}</h2>
        {identities === null ? <p className="comm-muted">{t('empty.data')}</p> : identities.length === 0 ? <p className="comm-muted">{t('empty.identities')}</p> : (
          <div className="comm-table-wrap"><table><thead><tr><th>{t('table.nickname')}</th><th>{t('table.publicKey')}</th><th>{t('table.default')}</th><th>{t('table.keystore')}</th><th>{t('identity.actions')}</th></tr></thead><tbody>
            {identities.map((identity) => <tr key={identity.nickname}>
              <td>{identity.nickname}</td><td className="comm-key">{identity.npub}</td>
              <td>{identity.default ? t('table.default') : '—'}</td><td>{identity.encrypted ? t('value.encrypted') : t('value.unencrypted')}</td>
              <td className="comm-row-actions">
                <button type="button" onClick={() => void copyNpub(identity.npub)} aria-label={`${t('identity.copy')} ${identity.nickname}`}>
                  {copied === identity.npub ? t('identity.copied') : t('identity.copy')}
                </button>
                <button type="button" disabled={pending !== null} onClick={() => void exportKey(identity.nickname)} aria-label={`${t('export.button')} ${identity.nickname}`}>
                  {t('export.button')}
                </button>
              </td>
            </tr>)}
          </tbody></table></div>
        )}
        {identities !== null && identities.length > 0 && <div className="comm-row-actions">{restoreUi}</div>}
        {identities?.some((identity) => identity.default) && <p className="comm-muted">{t('identity.myAddress')}</p>}
      </section>

      <section className="comm-card">
        <h2>{t('section.contacts')}</h2>
        <p className="comm-muted">{t('contact.guide')}</p>
        <p className="comm-muted">{t('contact.guideAny')}</p>
        {contacts === null ? <p className="comm-muted">{t('empty.data')}</p> : contacts.length === 0 ? <p className="comm-muted">{t('empty.contacts')}</p> : (
          <div className="comm-table-wrap"><table><thead><tr><th>{t('table.nickname')}</th><th>{t('table.publicKey')}</th><th>{t('table.role')}</th></tr></thead><tbody>
            {contacts.map((contact) => <tr key={`${contact.nickname}:${contact.npub}`}>
              <td>{contact.nickname}</td><td className="comm-key">{contact.npub}</td><td>{contact.role || '—'}</td>
            </tr>)}
          </tbody></table></div>
        )}
      </section>

      <section className="comm-card" aria-label={t('contact.add')}>
        <h2>{t('contact.add')}</h2>
        <form className="comm-setup-form" onSubmit={(event) => { event.preventDefault(); void submitContact() }}>
          <label>{t('contact.nickname')}<input ref={contactNameRef} value={contactName} onChange={(event) => setContactName(event.target.value)} /></label>
          <label>{t('contact.npub')}<input value={contactNpub} placeholder="npub1…" onChange={(event) => { setContactNpub(event.target.value); setContactError(null) }} /></label>
          <label>{t('contact.role')}<input value={contactRole} onChange={(event) => setContactRole(event.target.value)} /></label>
          <button type="submit" disabled={pending !== null || !contactName.trim() || !contactNpub.trim()}>{t('contact.add')}</button>
          {contactError && <p role="alert" className="comm-alert">{contactError}</p>}
        </form>
      </section>

      <section className="comm-card">
        <div className="comm-section-heading"><h2>{t('message.title')}</h2>
          <button type="button" onClick={() => void loadHistory()}>{t('message.refresh')}</button></div>
        <form className="comm-setup-form" onSubmit={(event) => { event.preventDefault(); void submitMessage() }}>
          <label>{t('message.to')}<input list="comm-recipients" value={messageTo} placeholder={t('message.toPlaceholder')} onChange={(event) => setMessageTo(event.target.value)} /></label>
          <datalist id="comm-recipients">{(contacts ?? []).map((c) => <option key={`${c.nickname}:${c.npub}`} value={c.nickname} />)}</datalist>
          <label>{t('message.content')}<textarea rows={2} value={messageText} onChange={(event) => setMessageText(event.target.value)} /></label>
          <button type="submit" disabled={pending !== null || !messageTo.trim() || !messageText.trim()}>{pending === 'send' ? t('message.sending') : t('message.send')}</button>
        </form>
        <h3>{t('message.recent')}</h3>
        {history && history.length > 0 ? (
          <ul className="comm-history">{history.map((item) => {
            const known = contacts?.find((c) => c.npub === item.from || c.nickname === item.from)
            const isNpub = item.from.startsWith('npub1')
            const sender = known ? known.nickname : isNpub ? shortNpub(item.from) : item.from
            return <li key={item.id}>
              <span className="comm-muted">{item.at}{item.at && sender ? ' · ' : ''}{sender}</span>
              <div>{item.content}</div>
              {item.from && <div className="comm-row-actions">
                <button type="button" onClick={() => setMessageTo(known ? known.nickname : item.from)}>{t('message.reply')}</button>
                {!known && isNpub && <button type="button" onClick={() => { setContactNpub(item.from); setContactError(null); contactNameRef.current?.focus() }}>{t('message.addContact')}</button>}
              </div>}
            </li>
          })}</ul>
        ) : <p className="comm-muted">{t('message.empty')}</p>}
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
          <button type="button" onClick={() => void runAction('stop')} disabled={pending !== null || !serviceStoppable} title={stopTitle}>{t('action.stop')}</button>
        </div>
        {showUnlock && <form className="comm-unlock" aria-label={t('unlock.title')} onSubmit={(event) => { event.preventDefault(); void runAction('unlock') }}>
          <p className="comm-muted comm-unlock-explain">{t('unlock.explain')}</p>
          <label className="comm-checkbox"><input type="checkbox" checked={remember} onChange={(event) => setRemember(event.target.checked)} />{t('password.remember')}</label>
          <button type="submit" disabled={pending !== null}>{pending === 'unlock' ? t('action.unlocking') : t('action.unlock')}</button>
          {unlockError && <p role="alert" className="comm-unlock-error">{unlockError}</p>}
        </form>}
      </section>
      </>}
    </main>
  )
}
