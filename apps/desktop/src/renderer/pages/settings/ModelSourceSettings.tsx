import { useEffect, useState } from 'react'
import type { ModelSource, ModelSourceResult } from '../../../shared/ipc-types'
import { useT } from '../../i18n'
import { modelSourceMessages } from './ModelSourceSettings.i18n'

type Key = keyof typeof modelSourceMessages.en
type Note = { ok: boolean; key: Key; vars?: Record<string, string | number> } | null

/** Settings → Models: a local OpenAI-compatible service (LM Studio, …) the
 * daemon uses first. Detect lists `{url}/v1/models` (main asks loopback only);
 * Save writes the setting and restarts the background service. */
export function ModelSourceSettings({ onChange }: { onChange?: (source: ModelSource | null) => void }) {
  const t = useT(modelSourceMessages)
  const [current, setCurrent] = useState<ModelSource | null>(null)
  const [url, setUrl] = useState('http://127.0.0.1:1234')
  const [model, setModel] = useState('')
  const [models, setModels] = useState<string[]>([])
  const [busy, setBusy] = useState<'detect' | 'apply' | null>(null)
  const [note, setNote] = useState<Note>(null)

  async function load() {
    const source = await window.agent24.modelSourceGet().catch(() => null)
    setCurrent(source)
    if (source) { setUrl(source.url); setModel(source.model) }
    onChange?.(source)
  }
  useEffect(() => { void load() }, [])

  async function detect() {
    setBusy('detect')
    setNote(null)
    const base = url.trim().replace(/\/+$/, '').replace(/\/v1$/, '')
    const r = await window.agent24.omlxModels(base, '').catch(() => ({ ok: false, models: [] as string[] }))
    setBusy(null)
    if (r.ok) {
      setModels(r.models)
      if (!model && r.models[0]) setModel(r.models[0])
      setNote({ ok: true, key: 'detected', vars: { count: r.models.length } })
    } else {
      setModels([])
      setNote({ ok: false, key: 'detectFailed', vars: { url: base } })
    }
  }

  async function apply(run: () => Promise<ModelSourceResult>, done: Key) {
    setBusy('apply')
    setNote(null)
    const r = await run().catch((): ModelSourceResult => ({ ok: false, reason: 'not_saved' }))
    setBusy(null)
    if (!r.ok) setNote({ ok: false, key: `error.${r.reason}` })
    else if (r.restart === 'restart_failed') setNote({ ok: false, key: 'restartFailed', vars: { code: r.code } })
    else setNote({ ok: true, key: done })
    await load()
  }

  return (
    <div className="settings-section" aria-labelledby="model-source-title">
      <h3 id="model-source-title">{t('title')}</h3>
      <p style={{ fontSize: 12, color: 'var(--muted)', margin: '0 0 8px' }}>{t('hint')}</p>
      <p role="status" style={{ fontSize: 12, margin: '0 0 8px' }}>
        {current ? t('inUse', { model: current.model, url: current.url }) : t('notSet')}
      </p>
      <div className="setting-row">
        <label htmlFor="model-source-url">{t('url')}</label>
        <div style={{ display: 'flex', gap: 6 }}>
          <input id="model-source-url" value={url} onChange={(e) => setUrl(e.target.value)} style={{ width: 240 }} />
          <button className="btn btn-ghost" disabled={busy !== null} onClick={() => { void detect() }}>
            {busy === 'detect' ? t('detecting') : t('detect')}
          </button>
        </div>
      </div>
      <div className="setting-row">
        <label htmlFor="model-source-model">{t('model')}</label>
        <input id="model-source-model" list="model-source-models" value={model} placeholder={t('modelPlaceholder')}
          onChange={(e) => setModel(e.target.value)} style={{ width: 240 }} />
        <datalist id="model-source-models">{models.map((m) => <option key={m} value={m} />)}</datalist>
      </div>
      <div className="setting-row">
        <span />
        <div style={{ display: 'flex', gap: 6 }}>
          <button className="btn btn-primary" disabled={busy !== null}
            onClick={() => { void apply(() => window.agent24.modelSourceSet(url, model), 'saved') }}>{t('save')}</button>
          {current && (
            <button className="btn btn-ghost" disabled={busy !== null}
              onClick={() => { void apply(() => window.agent24.modelSourceClear(), 'cleared') }}>{t('clear')}</button>
          )}
        </div>
      </div>
      {busy === 'apply' && <p role="status" style={{ fontSize: 12 }}>{t('applying')}</p>}
      {note && <p role={note.ok ? 'status' : 'alert'} style={{ fontSize: 12, color: note.ok ? 'var(--muted)' : '#f44336' }}>{t(note.key, note.vars)}</p>}
    </div>
  )
}
