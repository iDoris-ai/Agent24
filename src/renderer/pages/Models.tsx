import { useState, useEffect } from 'react'

interface OmlxModel {
  id: string
  engine?: string
  status?: string
}

const STATIC_MODELS = [
  { name: 'Qwen3-30B-A3B', desc: 'MoE · 128K · 日常全能', mem: '32 GB', badge: 'badge-fixed', label: '常驻' },
  { name: 'bge-m3', desc: '多语言嵌入 · RAG 检索', mem: '1.5 GB', badge: 'badge-fixed', label: '常驻' },
  { name: 'ModernBERT-reranker', desc: 'RAG 重排序', mem: '0.5 GB', badge: 'badge-fixed', label: '常驻' },
  { name: 'Qwen3.6-27B', desc: 'Dense · 代码 · 结构化输出', mem: '35 GB', badge: 'badge-ondemand', label: '按需' },
  { name: 'Qwen3-32B', desc: 'Dense 旗舰 · 深度推理', mem: '41 GB', badge: 'badge-ondemand', label: '按需' },
  { name: 'mlx-whisper large-v3', desc: 'ASR · 视频字幕', mem: '~3 GB', badge: 'badge-unload', label: '用完即卸' },
  { name: 'FLUX.1-schnell', desc: '文生图 · ~20s/张', mem: '16 GB', badge: 'badge-unload', label: '用完即卸' },
  { name: 'LTX-2.3 Distilled Q4', desc: '短视频生成', mem: '19 GB', badge: 'badge-unload', label: '用完即卸' },
  { name: 'CosyVoice2-0.5B', desc: '中文语音克隆 · TTS', mem: '3 GB', badge: 'badge-unload', label: '用完即卸' },
]

function statusDot(model: OmlxModel) {
  const loaded = model.engine === 'loaded' || model.status === 'loaded'
  const color = loaded ? '#4caf50' : '#888'
  const title = loaded ? '已加载' : '未加载'
  return <span title={title} style={{ display: 'inline-block', width: 8, height: 8, borderRadius: '50%', background: color, marginRight: 6 }} />
}

export default function ModelsPage() {
  const [liveModels, setLiveModels] = useState<OmlxModel[]>([])
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    void window.agent24.backendProxy({ method: 'GET', path: '/api/llm/models' })
      .then((res) => {
        if (res.ok && Array.isArray(res.data)) setLiveModels(res.data as OmlxModel[])
      })
      .catch(() => {/* oMLX not running */})
      .finally(() => setLoading(false))
  }, [])

  const hasLive = liveModels.length > 0

  return (
    <div className="content">
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 6 }}>
        <div className="page-title">模型管理</div>
        <button className="btn btn-primary" style={{ fontSize: 12 }}>+ 下载模型</button>
      </div>
      <div className="page-sub">64 GB 统一内存 · oMLX 管理主力 LLM · Ollama 备选</div>

      {/* Live oMLX model status */}
      {hasLive && (
        <div style={{ marginBottom: 16 }}>
          <div style={{ fontSize: 11, fontWeight: 600, color: 'var(--muted)', marginBottom: 8, textTransform: 'uppercase', letterSpacing: '0.05em' }}>
            oMLX 运行状态
          </div>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
            {liveModels.map((m) => (
              <div key={m.id} style={{ display: 'flex', alignItems: 'center', fontSize: 12, padding: '6px 12px', background: 'var(--surface2)', borderRadius: 8 }}>
                {statusDot(m)}
                <span style={{ fontFamily: 'monospace', fontSize: 11 }}>{m.id}</span>
                <span style={{ marginLeft: 'auto', fontSize: 10, color: 'var(--muted)' }}>
                  {m.engine === 'loaded' || m.status === 'loaded' ? '已加载' : '未加载'}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}
      {!hasLive && !loading && (
        <div style={{ fontSize: 12, color: 'var(--muted)', marginBottom: 12, padding: '6px 0' }}>
          oMLX 未运行 — 启动后此处显示实时模型状态
        </div>
      )}

      {/* Static model catalog */}
      <div style={{ fontSize: 11, fontWeight: 600, color: 'var(--muted)', marginBottom: 8, textTransform: 'uppercase', letterSpacing: '0.05em' }}>
        推荐模型目录（64 GB Mac）
      </div>
      <div className="models-list">
        {STATIC_MODELS.map(m => (
          <div key={m.name} className="model-row">
            <div className="model-info">
              <h4>{m.name}</h4>
              <p>{m.desc}</p>
            </div>
            <span className={`model-badge ${m.badge}`}>{m.label}</span>
            <span className="model-mem">{m.mem}</span>
          </div>
        ))}
      </div>
    </div>
  )
}
