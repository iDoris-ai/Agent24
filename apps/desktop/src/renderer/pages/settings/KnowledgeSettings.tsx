import { useEffect, useState } from 'react'
import type { KnowledgeSettingsResult, KnowledgeStatus } from '../../../shared/ipc-types'
import { useT } from '../../i18n'
import { knowledgeMessages } from './KnowledgeSettings.i18n'

/** Splits the knowledge base field on new lines and commas. */
export function parseKnowledgeBaseIds(text: string): string[] {
  return text.split(/[\n,]/).map((id) => id.trim()).filter((id) => id !== '')
}

// The key goes to main and never comes back: the field starts empty, and an
// empty field keeps the saved key.
export function KnowledgeSettings() {
  const t = useT(knowledgeMessages)
  const [status, setStatus] = useState<KnowledgeStatus | null>(null)
  const [endpoint, setEndpoint] = useState('')
  const [bases, setBases] = useState('')
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

  const save = () => run(() => window.agent24.knowledge.configure({
    endpoint: endpoint.trim(),
    knowledgeBaseIds: parseKnowledgeBaseIds(bases),
    ...(apiKey === '' ? {} : { apiKey }),
  }).then((r) => { if (r.ok) setApiKey(''); return r }))

  const statusText = !status?.enabled ? t('statusOff') : status.active ? t('statusActive') : t('statusIncomplete')

  return <section className="settings-section" aria-labelledby="knowledge-title">
    <h3 id="knowledge-title">{t('title')}</h3>
    <p className="settings-note">{t('hint')}</p>
    <div className="setting-row">
      <div><label htmlFor="knowledge-enabled">{t('enabled')}</label><p role="status">{statusText}</p></div>
      <input id="knowledge-enabled" type="checkbox" checked={status?.enabled ?? false} disabled={busy || !status}
        onChange={(e) => { const on = e.target.checked; void run(() => window.agent24.knowledge.setEnabled(on)) }} />
    </div>
    <div className="setting-row">
      <div><label htmlFor="knowledge-endpoint">{t('endpoint')}</label><p>{t('endpointHint')}</p></div>
      <input id="knowledge-endpoint" type="url" value={endpoint} onChange={(e) => setEndpoint(e.target.value)} />
    </div>
    <div className="setting-row">
      <div><label htmlFor="knowledge-bases">{t('knowledgeBases')}</label><p>{t('knowledgeBasesHint')}</p></div>
      <textarea id="knowledge-bases" rows={3} value={bases} onChange={(e) => setBases(e.target.value)} />
    </div>
    <div className="setting-row">
      <div><label htmlFor="knowledge-key">{t('apiKey')}</label><p>{status?.hasKey ? t('apiKeyKept') : t('apiKeyNone')}</p></div>
      <input id="knowledge-key" type="password" autoComplete="off" value={apiKey} onChange={(e) => setApiKey(e.target.value)} />
    </div>
    <div className="setting-row">
      <button type="button" className="btn" disabled={busy || !status} onClick={() => void save()}>
        {busy ? t('saving') : t('save')}
      </button>
      {result && <p role="alert">{result.ok ? t('saved') : t(`error.${result.reason}`)}</p>}
    </div>
  </section>
}
