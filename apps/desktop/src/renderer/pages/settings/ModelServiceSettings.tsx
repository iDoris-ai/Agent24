import { useEffect, useState } from 'react'
import { useT } from '../../i18n'
import { getStatus, type IdorisStatus } from '../../components/idoris/api'
import { idorisMessages } from '../../components/idoris/IdorisPanel.i18n'
import { settingsMessages } from '../Settings.i18n'
import { isChatModelId } from './chatModels'
import ModelSourcePanel from '../../components/idoris/ModelSourcePanel'

const IDORIS_STATE: Record<IdorisStatus['state'], 'state.not_installed' | 'state.not_configured' | 'state.stopped' | 'state.starting' | 'state.running' | 'state.failed' | 'state.instance_conflict'> = {
  not_installed: 'state.not_installed',
  not_configured: 'state.not_configured',
  stopped: 'state.stopped',
  starting: 'state.starting',
  running: 'state.running',
  failed: 'state.failed',
  instance_conflict: 'state.instance_conflict',
}

function chatModelIds(data: unknown): { ids: string[]; defaultModel: string } {
  const body = data as { models?: unknown; default_model?: unknown } | null
  const raw = Array.isArray(body?.models) ? body.models : []
  const ids = raw.map((entry) => {
    if (typeof entry === 'string') return entry
    if (entry && typeof entry === 'object' && typeof (entry as { id?: unknown }).id === 'string') return (entry as { id: string }).id
    return ''
  }).filter((id) => id !== '' && isChatModelId(id))
  const defaultModel = typeof body?.default_model === 'string' ? body.default_model : ''
  return { ids, defaultModel }
}

/** Models tab: the chat-model list and the default come only from the daemon.
 *  iDoris status is read-only. Custom upstreams are iDoris's (ID-4). */
export function ModelServiceSettings(): JSX.Element {
  const t = useT(settingsMessages)
  const idorisT = useT(idorisMessages)
  const [models, setModels] = useState<string[]>([])
  const [active, setActive] = useState('')
  const [daemonOffline, setDaemonOffline] = useState(true)
  const [saveError, setSaveError] = useState(false)
  const [idorisState, setIdorisState] = useState<IdorisStatus['state'] | null>(null)
  const [idorisUnavailable, setIdorisUnavailable] = useState(false)

  useEffect(() => { void load() }, [])

  async function load() {
    const proxy = window.agent24.backendProxy
    if (typeof proxy !== 'function') {
      setDaemonOffline(true)
      setModels([])
      return
    }
    try {
      const res = await proxy({ method: 'GET', path: '/api/v1/models' })
      if (!res.ok) {
        setDaemonOffline(true)
        setModels([])
      } else {
        const parsed = chatModelIds(res.data)
        setModels(parsed.ids)
        setActive(parsed.defaultModel)
        setDaemonOffline(false)
      }
    } catch {
      setDaemonOffline(true)
      setModels([])
    }
    try {
      const status = await getStatus()
      setIdorisState(status.state)
      setIdorisUnavailable(false)
    } catch {
      setIdorisState(null)
      setIdorisUnavailable(true)
    }
  }

  async function selectDefault(model: string) {
    if (daemonOffline) return
    const proxy = window.agent24.backendProxy
    if (typeof proxy !== 'function') {
      setDaemonOffline(true)
      return
    }
    setSaveError(false)
    try {
      const res = await proxy({ method: 'PUT', path: '/api/v1/models/default', body: { model } })
      if (!res.ok) {
        setSaveError(true)
        return
      }
      setActive(model)
    } catch {
      setSaveError(true)
    }
  }

  return (
    <div className="settings-section">
      <ModelSourcePanel />
      <h3>{t('service.title')}</h3>
      <p style={{ fontSize: 12, color: 'var(--muted)', margin: '0 0 8px' }}>{t('service.hint')}</p>
      <p role="status" style={{ fontSize: 12, margin: '0 0 8px' }}>
        {t('idoris.heading')}: {idorisUnavailable || idorisState === null ? t('idoris.unavailable') : idorisT(IDORIS_STATE[idorisState])}
      </p>
      {daemonOffline && (
        <div style={{ fontSize: 12, color: 'var(--muted)', padding: '4px 0' }}>{t('models.daemonOffline')}</div>
      )}
      {saveError && (
        <div style={{ fontSize: 12, color: '#f44336', padding: '4px 0' }}>{t('models.saveError')}</div>
      )}
      <div style={{ fontSize: 11, fontWeight: 600, color: 'var(--muted)', padding: '8px 0 4px', letterSpacing: '0.6px', textTransform: 'uppercase' }}>
        {t('models.heading')}
      </div>
      {models.length === 0 && (
        <div className="setting-row">
          <span style={{ fontSize: 12, color: 'var(--muted)' }}>{t('models.empty')}</span>
        </div>
      )}
      {models.map((model) => {
        const isActive = active === model
        return (
          <div key={model} className="setting-row" style={{ marginBottom: 2, alignItems: 'center' }}
            onClick={() => { void selectDefault(model) }}
            role="button"
            aria-label={model}
            aria-pressed={isActive}
            aria-disabled={daemonOffline}
            tabIndex={daemonOffline ? -1 : 0}
            onKeyDown={(e) => { if (e.key === 'Enter') void selectDefault(model) }}
          >
            <span style={{
              width: 6, height: 6, borderRadius: '50%', flexShrink: 0,
              background: isActive ? 'var(--accent)' : 'var(--border)',
            }} />
            <span style={{ fontSize: 13, fontWeight: isActive ? 600 : 400, color: isActive ? 'var(--accent)' : 'var(--text)' }}>
              {model}
            </span>
          </div>
        )
      })}
    </div>
  )
}
