import { useState, useEffect } from 'react'
import IdorisPanel from '../components/idoris/IdorisPanel'
import { translate, useT } from '../i18n'
import { modelsMessages } from './Models.i18n'

// Shape of one entry in `GET /api/v1/models`'s `models` array
// (`agent24_protocol::Model`, `rust/crates/agent24-protocol/src/types.rs`):
// `{ id, provider, tier, loaded }`. Mirrors how `App.tsx`'s
// `extractDaemonDefaultModel` reads the same endpoint's `default_model`.
interface OmlxModel {
  id: string
  provider?: string
  tier?: string
  loaded?: boolean
}

const STATIC_MODELS = [
  { name: 'Qwen3-30B-A3B', desc: 'model.qwen30', mem: '32 GB', badge: 'badge-fixed', label: 'badge.resident' },
  { name: 'bge-m3', desc: 'model.bge', mem: '1.5 GB', badge: 'badge-fixed', label: 'badge.resident' },
  { name: 'ModernBERT-reranker', desc: 'model.reranker', mem: '0.5 GB', badge: 'badge-fixed', label: 'badge.resident' },
  { name: 'Qwen3.6-27B', desc: 'model.qwen27', mem: '35 GB', badge: 'badge-ondemand', label: 'badge.onDemand' },
  { name: 'Qwen3-32B', desc: 'model.qwen32', mem: '41 GB', badge: 'badge-ondemand', label: 'badge.onDemand' },
  { name: 'mlx-whisper large-v3', desc: 'model.whisper', mem: '~3 GB', badge: 'badge-unload', label: 'badge.unload' },
  { name: 'FLUX.1-schnell', desc: 'model.flux', mem: '16 GB', badge: 'badge-unload', label: 'badge.unload' },
  { name: 'LTX-2.3 Distilled Q4', desc: 'model.ltx', mem: '19 GB', badge: 'badge-unload', label: 'badge.unload' },
  { name: 'CosyVoice2-0.5B', desc: 'model.cosy', mem: '3 GB', badge: 'badge-unload', label: 'badge.unload' },
] as const

function statusDot(model: OmlxModel) {
  const loaded = model.loaded === true
  const color = loaded ? '#4caf50' : '#888'
  const title = translate(modelsMessages, loaded ? 'live.loaded' : 'live.unloaded')
  return <span title={title} style={{ display: 'inline-block', width: 8, height: 8, borderRadius: '50%', background: color, marginRight: 6 }} />
}

export default function ModelsPage() {
  const t = useT(modelsMessages)
  const [liveModels, setLiveModels] = useState<OmlxModel[]>([])
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    // AUDIT-1: the daemon's real route is `/api/v1/models`, returning
    // `{ models: Model[], default_model: string | null }` (`routes.rs`
    // `get_models`) — not a bare array at `/api/llm/models` (404, doesn't
    // exist). Same endpoint the topbar (`App.tsx`) already polls.
    void window.agent24.backendProxy({ method: 'GET', path: '/api/v1/models' })
      .then((res) => {
        if (!res.ok) return
        const data = res.data as { models?: unknown } | null
        if (Array.isArray(data?.models)) setLiveModels(data.models as OmlxModel[])
      })
      .catch(() => {/* oMLX not running */})
      .finally(() => setLoading(false))
  }, [])

  const hasLive = liveModels.length > 0

  return (
    <div className="content">
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 6 }}>
        <div className="page-title">{t('page.title')}</div>
        <button className="btn btn-primary" style={{ fontSize: 12 }}>{t('action.download')}</button>
      </div>
      <div className="page-sub">{t('page.subtitle')}</div>

      <IdorisPanel />

      {/* Live oMLX model status */}
      {hasLive && (
        <div style={{ marginBottom: 16 }}>
          <div style={{ fontSize: 11, fontWeight: 600, color: 'var(--muted)', marginBottom: 8, textTransform: 'uppercase', letterSpacing: '0.05em' }}>
            {t('live.heading')}
          </div>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
            {liveModels.map((m) => (
              <div key={m.id} style={{ display: 'flex', alignItems: 'center', fontSize: 12, padding: '6px 12px', background: 'var(--surface2)', borderRadius: 8 }}>
                {statusDot(m)}
                <span style={{ fontFamily: 'monospace', fontSize: 11 }}>{m.id}</span>
                <span style={{ marginLeft: 'auto', fontSize: 10, color: 'var(--muted)' }}>
                  {t(m.loaded === true ? 'live.loaded' : 'live.unloaded')}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}
      {!hasLive && !loading && (
        <div style={{ fontSize: 12, color: 'var(--muted)', marginBottom: 12, padding: '6px 0' }}>
          {t('live.unavailable')}
        </div>
      )}

      {/* Static model catalog */}
      <div style={{ fontSize: 11, fontWeight: 600, color: 'var(--muted)', marginBottom: 8, textTransform: 'uppercase', letterSpacing: '0.05em' }}>
        {t('catalog.heading')}
      </div>
      <div className="models-list">
        {STATIC_MODELS.map(m => (
          <div key={m.name} className="model-row">
            <div className="model-info">
              <h4>{m.name}</h4>
              <p>{t(m.desc)}</p>
            </div>
            <span className={`model-badge ${m.badge}`}>{t(m.label)}</span>
            <span className="model-mem">{m.mem}</span>
          </div>
        ))}
      </div>
    </div>
  )
}
