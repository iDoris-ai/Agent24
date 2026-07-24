import { useState, useEffect, useCallback, useMemo } from 'react'
import {
  listSchedules,
  createSchedule,
  deleteSchedule,
  updateSchedule,
  runScheduleNow,
  type Schedule,
  type ScheduleSpec,
} from './agent/api'
import { previewNextFire, formatPreview, type ScheduleSpecInput } from './schedule/cronPreview'

type SpecType = 'cron' | 'every' | 'at'

function buildSpec(type: SpecType, expr: string, tz: string, secs: string, ts: string): ScheduleSpec {
  if (type === 'cron') return { type: 'cron', expr, tz: tz.trim() || null }
  if (type === 'every') return { type: 'every', secs: Number(secs) }
  return { type: 'at', ts }
}

export default function SchedulesPage() {
  const [schedules, setSchedules] = useState<Schedule[]>([])
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)

  // Create form state
  const [name, setName] = useState('')
  const [prompt, setPrompt] = useState('')
  const [specType, setSpecType] = useState<SpecType>('cron')
  const [expr, setExpr] = useState('0 8 * * *')
  const [tz, setTz] = useState('UTC')
  const [secs, setSecs] = useState('3600')
  const [ts, setTs] = useState('')

  const refresh = useCallback(() => {
    listSchedules()
      .then((s) => {
        setSchedules(s)
        setError(null)
      })
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
  }, [])

  useEffect(() => {
    refresh()
    const timer = setInterval(refresh, 5000)
    return () => clearInterval(timer)
  }, [refresh])

  // Live next-fire preview (recomputed as the form changes)
  const preview = useMemo(() => {
    const specInput: ScheduleSpecInput = {
      type: specType,
      expr,
      tz,
      secs: Number(secs),
      ts,
    }
    return previewNextFire(specInput, new Date())
  }, [specType, expr, tz, secs, ts])

  const onCreate = async () => {
    if (name.trim() === '' || prompt.trim() === '') {
      setError('名称和 prompt 必填')
      return
    }
    if (preview.error) {
      setError(`调度规格无效：${preview.error}`)
      return
    }
    try {
      await createSchedule({
        name: name.trim(),
        spec: buildSpec(specType, expr, tz, secs, ts),
        action: { type: 'agent_run', prompt: prompt.trim() },
      })
      setName('')
      setPrompt('')
      setError(null)
      setNotice('已创建调度')
      refresh()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  const onToggle = async (s: Schedule) => {
    try {
      await updateSchedule(s.id, { enabled: !s.enabled })
      refresh()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  const onDelete = async (id: string) => {
    try {
      await deleteSchedule(id)
      refresh()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  const onRunNow = async (id: string) => {
    try {
      const runId = await runScheduleNow(id)
      setNotice(`已触发运行 ${runId}`)
      refresh()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  return (
    <div className="content">
      <div className="page-title">定时调度</div>
      <div className="page-sub">挂钟定时工作流 · cron / 每隔 / 一次性</div>

      {error && <div style={{ color: '#e05050', fontSize: 12, marginBottom: 8 }}>{error}</div>}
      {notice && <div style={{ color: '#4caf50', fontSize: 12, marginBottom: 8 }}>{notice}</div>}

      {/* Create form */}
      <div style={{ border: '1px solid var(--border)', borderRadius: 8, padding: 12, marginBottom: 16 }}>
        <div style={{ fontSize: 12, fontWeight: 600, marginBottom: 8 }}>新建调度</div>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
          <input
            aria-label="名称"
            placeholder="名称，如「每日晨报」"
            value={name}
            onChange={(e) => setName(e.target.value)}
            style={inputStyle}
          />
          <input
            aria-label="prompt"
            placeholder="prompt，如「抓取 RSS 并生成摘要」"
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            style={inputStyle}
          />
          <div style={{ display: 'flex', gap: 6 }}>
            {(['cron', 'every', 'at'] as SpecType[]).map((t) => (
              <button
                key={t}
                className={`btn${specType === t ? ' btn-primary' : ''}`}
                style={{ fontSize: 11 }}
                onClick={() => setSpecType(t)}
              >
                {t === 'cron' ? 'cron' : t === 'every' ? '每隔' : '一次性'}
              </button>
            ))}
          </div>

          {specType === 'cron' && (
            <div style={{ display: 'flex', gap: 6 }}>
              <input
                aria-label="cron 表达式"
                placeholder="0 8 * * *"
                value={expr}
                onChange={(e) => setExpr(e.target.value)}
                style={{ ...inputStyle, flex: 2, fontFamily: 'monospace' }}
              />
              <input
                aria-label="时区"
                placeholder="UTC"
                value={tz}
                onChange={(e) => setTz(e.target.value)}
                style={{ ...inputStyle, flex: 1 }}
              />
            </div>
          )}
          {specType === 'every' && (
            <input
              aria-label="每隔秒数"
              type="number"
              placeholder="秒（60–86400）"
              value={secs}
              onChange={(e) => setSecs(e.target.value)}
              style={inputStyle}
            />
          )}
          {specType === 'at' && (
            <input
              aria-label="触发时间"
              placeholder="2026-08-01T09:00:00Z"
              value={ts}
              onChange={(e) => setTs(e.target.value)}
              style={{ ...inputStyle, fontFamily: 'monospace' }}
            />
          )}

          {/* Live preview */}
          <div style={{ fontSize: 12 }} data-testid="preview">
            {preview.error ? (
              <span style={{ color: '#e05050' }}>下次触发：{preview.error}</span>
            ) : (
              <span style={{ color: 'var(--muted)' }}>
                下次触发：{formatPreview(preview.next!)}
                {preview.approximate && '（近似 · 服务端按所选时区精确计算）'}
              </span>
            )}
          </div>

          <button className="btn btn-primary" style={{ fontSize: 12, alignSelf: 'flex-start' }} onClick={onCreate}>
            创建
          </button>
        </div>
      </div>

      {/* List */}
      <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
        {schedules.length === 0 && (
          <div style={{ fontSize: 12, color: 'var(--muted)' }}>暂无调度</div>
        )}
        {schedules.map((s) => (
          <div
            key={s.id}
            style={{
              border: '1px solid var(--border)',
              borderRadius: 8,
              padding: 12,
              opacity: s.enabled ? 1 : 0.55,
            }}
          >
            <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <strong style={{ fontSize: 13 }}>{s.name}</strong>
              <span style={{ fontSize: 10, color: 'var(--muted)' }}>{specSummary(s.spec)}</span>
              {s.consecutive_failures > 0 && (
                <span style={{ fontSize: 10, color: '#e0a020' }}>
                  连续失败 {s.consecutive_failures}
                </span>
              )}
              <div style={{ marginLeft: 'auto', display: 'flex', gap: 6 }}>
                <button className="btn" style={{ fontSize: 11 }} onClick={() => onRunNow(s.id)}>
                  立即运行
                </button>
                <button className="btn" style={{ fontSize: 11 }} onClick={() => onToggle(s)}>
                  {s.enabled ? '禁用' : '启用'}
                </button>
                <button className="btn" style={{ fontSize: 11 }} onClick={() => onDelete(s.id)}>
                  删除
                </button>
              </div>
            </div>
            <div style={{ fontSize: 11, color: 'var(--muted)', marginTop: 4 }}>
              {s.next_run_at ? `下次 ${s.next_run_at}` : '（无后续触发）'}
              {s.last_run_at && ` · 上次 ${s.last_run_at}`}
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}

function specSummary(spec: ScheduleSpec): string {
  if (spec.type === 'cron') return `cron ${spec.expr}${spec.tz ? ` @${spec.tz}` : ''}`
  if (spec.type === 'every') return `每 ${spec.secs}s`
  return `一次性 ${spec.ts}`
}

const inputStyle: React.CSSProperties = {
  fontSize: 12,
  padding: '5px 8px',
  background: 'var(--surface2)',
  border: '1px solid var(--border)',
  borderRadius: 6,
  color: 'var(--text)',
}
