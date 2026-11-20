import { useEffect, useState } from 'react'
import { useT } from '../../i18n'
import { getStatus, type IdorisStatus } from '../../components/idoris/api'
import { idorisMessages } from '../../components/idoris/IdorisPanel.i18n'
import { settingsMessages } from '../Settings.i18n'
import { isChatModelId } from './chatModels'

const IDORIS_STATE: Record<IdorisStatus['state'], 'state.not_installed' | 'state.not_configured' | 'state.stopped' | 'state.starting' | 'state.running' | 'state.failed' | 'state.instance_conflict'> = {
  not_installed: 'state.not_installed',
  not_configured: 'state.not_configured',
  stopped: 'state.stopped',
  starting: 'state.starting',
  running: 'state.running',
  failed: 'state.failed',
  instance_conflict: 'state.instance_conflict',
}

const REASONS = {
  tier_below_16gib: 'models.reason.tier_below_16gib',
  tier_16_48gib: 'models.reason.tier_16_48gib',
  tier_48_64gib: 'models.reason.tier_48_64gib',
  tier_64gib_plus: 'models.reason.tier_64gib_plus',
  fallback_does_not_fit: 'models.reason.fallback_does_not_fit',
  insufficient_memory: 'models.reason.insufficient_memory',
  idoris_unavailable: 'models.reason.idoris_unavailable',
} satisfies Record<string, keyof typeof settingsMessages.en>

function isObject(data: unknown): data is Record<string, unknown> {
  return data !== null && typeof data === 'object' && !Array.isArray(data)
}

function localChatModelIds(data: unknown): string[] | null {
  if (!isObject(data) || !Array.isArray(data.models)) return null
  return data.models.flatMap((entry: unknown) =>
    isObject(entry) && typeof entry.omlx_id === 'string' && (entry.kind === 'chat' || entry.kind === 'vision') ? [entry.omlx_id] : [])
}

type Recommendation = { modelId: string; installed: boolean; sizeBytes: number | null; reason: string }

function parseRecommendation(data: unknown): Recommendation | null {
  if (!isObject(data) || !isObject(data.recommendation)) return null
  const rec = data.recommendation
  if (typeof rec.model_id !== 'string' || typeof rec.installed !== 'boolean' || typeof rec.reason !== 'string') return null
  return {
    modelId: rec.model_id, installed: rec.installed, reason: rec.reason,
    sizeBytes: typeof rec.size_bytes === 'number' ? rec.size_bytes : null,
  }
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
  const [recommendation, setRecommendation] = useState<Recommendation | null>(null)
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
      const [res, local, recommended] = await Promise.all([
        proxy({ method: 'GET', path: '/api/v1/models' }),
        proxy({ method: 'GET', path: '/api/v1/models/local' }).catch(() => null),
        proxy({ method: 'GET', path: '/api/v1/models/recommendation?role=chat' }).catch(() => null),
      ])
      setRecommendation(recommended?.ok ? parseRecommendation(recommended.data) : null)
      if (!res.ok) {
        setDaemonOffline(true)
        setModels([])
      } else {
        const parsed = chatModelIds(res.data)
        setModels((local?.ok ? localChatModelIds(local.data) : null) ?? parsed.ids)
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

  const listedModels = recommendation && !recommendation.installed && !models.includes(recommendation.modelId)
    ? [...models, recommendation.modelId] : models
  const reasonKey = Object.entries(REASONS).find(([code]) => code === recommendation?.reason)?.[1]

  return (
    <div className="settings-section">
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
      {listedModels.length === 0 && (
        <div className="setting-row">
          <span style={{ fontSize: 12, color: 'var(--muted)' }}>{t('models.empty')}</span>
        </div>
      )}
      {listedModels.map((model) => {
        const isActive = active === model
        const recommended = recommendation?.modelId === model ? recommendation : null
        const selectable = !daemonOffline && recommended?.installed !== false
        return (
          <div key={model} className="setting-row" style={{ marginBottom: 2, alignItems: 'center' }}
            onClick={() => { if (selectable) void selectDefault(model) }}
            role={recommended?.installed === false ? 'group' : 'button'}
            aria-label={model}
            aria-pressed={recommended?.installed === false ? undefined : isActive}
            aria-disabled={!selectable}
            tabIndex={selectable ? 0 : -1}
            onKeyDown={(e) => { if (selectable && e.key === 'Enter') void selectDefault(model) }}
          >
            <span style={{
              width: 6, height: 6, borderRadius: '50%', flexShrink: 0,
              background: isActive ? 'var(--accent)' : 'var(--border)',
            }} />
            <span style={{ minWidth: 0, overflowWrap: 'anywhere' }}>
              <span style={{ fontSize: 13, fontWeight: isActive ? 600 : 400, color: isActive ? 'var(--accent)' : 'var(--text)' }}>{model}</span>
              {recommended && <span style={{ fontSize: 11, color: 'var(--accent)', border: '1px solid var(--accent)', borderRadius: 4, padding: '1px 5px', marginInlineStart: 8 }}>{t('models.recommended')}</span>}
              {recommended && (
                <span style={{ display: 'block', fontSize: 12, color: 'var(--muted)', marginTop: 4 }}>
                  {reasonKey ? t(reasonKey) : recommended.reason}
                </span>
              )}
              {recommended?.installed === false && (
                <>
                  <span style={{ display: 'block', fontSize: 12, marginTop: 4 }}>{t('models.notDownloaded', {
                    size: recommended.sizeBytes === null ? t('models.unknownSize') : t('models.approxSize', { size: (recommended.sizeBytes / 1_000_000_000).toFixed(1) }),
                  })}</span>
                  <span style={{ display: 'block', fontSize: 12, color: 'var(--muted)', marginTop: 4 }}>{t('models.downloadSoon')}</span>
                </>
              )}
            </span>
          </div>
        )
      })}
    </div>
  )
}
