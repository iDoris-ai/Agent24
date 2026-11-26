import { useEffect, useState } from 'react'
import type { KnowledgeSettingsResult, KnowledgeStatus, LiteKnowledgeStatus } from '../../../shared/ipc-types'
import { useT, type T } from '../../i18n'
import { knowledgeMessages } from './KnowledgeSettings.i18n'
import { formatKbNames, loadKbNames, parseKbNames, saveKbNames } from '../chat/kb-names'

/** Splits the knowledge base field on new lines and commas. */
export function parseKnowledgeBaseIds(text: string): string[] {
  return text.split(/[\n,]/).map((id) => id.trim()).filter((id) => id !== '')
}

type KnowledgeKey = keyof typeof knowledgeMessages.en & string
const MB = (bytes: number | undefined) => (bytes === undefined ? '?' : (bytes / 1_048_576).toFixed(1))

/** Loop 9 3c: the local knowledge base's install and launch, as one line. */
export function localLine(local: LiteKnowledgeStatus, t: T<KnowledgeKey>): string {
  switch (local.phase) {
    case 'not_installed': return t('local.not_installed', { size: MB(local.total) })
    case 'downloading': return t('local.downloading', { received: MB(local.received), total: MB(local.total) })
    case 'install_failed': return t(`local.install_failed.${local.failure}`)
    case 'failed': return t(`local.failed.${local.reason}`)
    default: return t(`local.${local.phase}`)
  }
}

/** Phases that move on their own: the page asks again every second. */
const MOVING = new Set<LiteKnowledgeStatus['phase']>(['not_installed', 'downloading', 'verifying', 'starting'])

// The key goes to main and never comes back: the field starts empty, and an
// empty field keeps the saved key.
export function KnowledgeSettings() {
  const t = useT(knowledgeMessages)
  const [status, setStatus] = useState<KnowledgeStatus | null>(null)
  const [endpoint, setEndpoint] = useState('')
  const [bases, setBases] = useState('')
  const [names, setNames] = useState(() => formatKbNames(loadKbNames()))
  const [apiKey, setApiKey] = useState('')
  const [busy, setBusy] = useState(false)
  const [result, setResult] = useState<KnowledgeSettingsResult | null>(null)

  async function refresh(fill: boolean) {
    let next: KnowledgeStatus | null = null
    try {
      next = (await window.agent24.knowledge?.status()) ?? null
    } catch {
      // Main has no Knowledge settings to offer: the controls stay disabled.
    }
    setStatus(next)
    if (next && fill) {
      setEndpoint(next.endpoint)
      setBases(next.knowledgeBaseIds.join('\n'))
    }
  }
  useEffect(() => { void refresh(true) }, [])

  async function run(action: () => Promise<KnowledgeSettingsResult>) {
    setBusy(true)
    setResult(null)
    try {
      setResult(await action())
    } catch {
      setResult({ ok: false, reason: 'unavailable' })
    } finally {
      await refresh(false)
      setBusy(false)
    }
  }

  const save = () => run(() => {
    // L7-K1: a display label per library, kept on this machine only.
    saveKbNames(parseKbNames(names))
    return window.agent24.knowledge.configure({
    endpoint: endpoint.trim(),
    knowledgeBaseIds: parseKnowledgeBaseIds(bases),
    ...(apiKey === '' ? {} : { apiKey }),
    }).then((r) => { if (r.ok) setApiKey(''); return r })
  })

  const statusText = !status?.enabled ? t('statusOff') : status.active ? t('statusActive')
    : status.source === 'local' ? t('statusLocalPending') : t('statusIncomplete')
  // K2: what the last check of the server found, red unless it answered.
  const reach = status?.enabled ? status.reachability : undefined
  const reachTime = reach?.checkedAt ? new Date(reach.checkedAt).toLocaleTimeString() : ''
  const reachText = reach && reach.state !== 'unknown' ? t(`reach.${reach.state}`, { time: reachTime }) : null
  const [checking, setChecking] = useState(false)
  async function checkNow() {
    setChecking(true)
    try {
      const next = await window.agent24.knowledge?.check?.()
      if (next) setStatus(next)
    } catch {
      // The status stays as it was.
    } finally {
      setChecking(false)
    }
  }
  // While this page is open, show what main finds without a reload; every
  // second while the local knowledge base is downloading or starting.
  const local = status?.source === 'local' ? status.local : undefined
  const moving = local !== undefined && MOVING.has(local.phase)
  useEffect(() => {
    if (!status?.enabled) return
    const timer = setInterval(() => { void refresh(false) }, moving ? 1_000 : 15_000)
    return () => clearInterval(timer)
  }, [status?.enabled, moving])
  const source = status?.source ?? 'remote'
  const choose = (next: 'remote' | 'local') => {
    if (next !== source) void run(() => window.agent24.knowledge.setSource(next))
  }
  const retry = async () => {
    try { await window.agent24.knowledge.liteRetry() } catch { /* the status says what happened */ }
    await refresh(false)
  }

  return <section className="settings-section" aria-labelledby="knowledge-title">
    <h3 id="knowledge-title">{t('title')}</h3>
    <p className="settings-note">{t('hint')}</p>
    {status?.local && (
      <fieldset className="setting-row" disabled={busy}>
        <legend>{t('source')}</legend>
        <label><input type="radio" name="knowledge-source" checked={source === 'remote'} onChange={() => choose('remote')} /> {t('source.remote')}</label>
        <label><input type="radio" name="knowledge-source" checked={source === 'local'} onChange={() => choose('local')} /> {t('source.local')}</label>
        <p>{t('source.localHint')}</p>
        {local && local.phase !== 'off' && (
          <p role={local.phase === 'install_failed' || local.phase === 'failed' ? 'alert' : 'status'}>
            {localLine(local, t)}
            {local.phase === 'downloading' && local.total ? <> <progress max={local.total} value={local.received} /></> : null}
            {(local.phase === 'install_failed' || local.phase === 'failed') && (
              <> <button type="button" className="btn btn-ghost" onClick={() => void retry()}>{t('local.retry')}</button></>
            )}
          </p>
        )}
      </fieldset>
    )}
    <div className="setting-row">
      <div>
        <label htmlFor="knowledge-enabled">{t('enabled')}</label>
        <p role="status">{statusText}</p>
        {reachText && (
          <p role={reach?.state === 'ok' ? 'status' : 'alert'}
            style={reach?.state === 'ok' ? { color: 'var(--muted)' } : { color: 'var(--danger, #c0392b)', fontWeight: 600 }}>
            {reachText}
            {' '}
            <button type="button" className="btn btn-ghost" disabled={checking} aria-busy={checking} onClick={() => void checkNow()}>{checking ? t('reach.checking') : t('reach.check')}</button>
          </p>
        )}
      </div>
      <input id="knowledge-enabled" type="checkbox" checked={status?.enabled ?? false} disabled={busy || !status}
        onChange={(e) => { const on = e.target.checked; void run(() => window.agent24.knowledge.setEnabled(on)) }} />
    </div>
    {source === 'remote' && <>
    <div className="setting-row">
      <div><label htmlFor="knowledge-endpoint">{t('endpoint')}</label><p>{t('endpointHint')}</p></div>
      <input id="knowledge-endpoint" type="url" value={endpoint} onChange={(e) => setEndpoint(e.target.value)} />
    </div>
    <div className="setting-row">
      <div><label htmlFor="knowledge-bases">{t('knowledgeBases')}</label><p>{t('knowledgeBasesHint')}</p></div>
      <textarea id="knowledge-bases" rows={3} value={bases} onChange={(e) => setBases(e.target.value)} />
    </div>
    <div className="setting-row">
      <div><label htmlFor="knowledge-names">{t('libraryNames')}</label><p>{t('libraryNamesHint')}</p></div>
      <textarea id="knowledge-names" rows={3} value={names} onChange={(e) => setNames(e.target.value)} />
    </div>
    <div className="setting-row">
      <div><label htmlFor="knowledge-key">{t('apiKey')}</label><p>{status?.hasKey ? t('apiKeyKept') : t('apiKeyNone')}</p></div>
      <input id="knowledge-key" type="password" autoComplete="off" value={apiKey} onChange={(e) => setApiKey(e.target.value)} />
    </div>
    <div className="setting-row">
      <button type="button" className="btn" disabled={busy || !status} onClick={() => void save()}>
        {busy ? t('saving') : t('save')}
      </button>
    </div>
    </>}
    {result && <p role="alert">{result.ok ? t('saved') : t(`error.${result.reason}`)}</p>}
  </section>
}
