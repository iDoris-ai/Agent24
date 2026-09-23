import { useState, useEffect, useCallback, useMemo, useRef } from 'react'
import {
  listSchedules,
  createSchedule,
  deleteSchedule,
  updateSchedule,
  runScheduleNow,
  type Schedule,
  type ScheduleSpec,
  type DisabledBy,
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

  // Monotonic request token: only the latest refresh's result may land, so a
  // slow poll can't overwrite fresher post-mutation state (review C7).
  const reqSeq = useRef(0)
  const refresh = useCallback(() => {
    const seq = ++reqSeq.current
    listSchedules()
      .then((s) => {
        if (seq !== reqSeq.current) return // a newer refresh superseded this one
        setSchedules(s)
        setError(null)
      })
      .catch((e: unknown) => {
        if (seq !== reqSeq.current) return
        setError(e instanceof Error ? e.message : String(e))
      })
  }, [])

  useEffect(() => {
    refresh()
    const timer = setInterval(refresh, 5000)
    return () => clearInterval(timer)
  }, [refresh])

  // A 1-Hz clock so the preview (especially `at`) stays live as time passes,
  // not only when the form changes.
  const [nowTick, setNowTick] = useState(() => Date.now())
  useEffect(() => {
    const t = setInterval(() => setNowTick(Date.now()), 1000)
    return () => clearInterval(t)
  }, [])

  // Live next-fire preview (recomputed on form change AND each clock tick)
  const preview = useMemo(() => {
    const specInput: ScheduleSpecInput = {
      type: specType,
      expr,
      tz,
      secs: Number(secs),
      ts,
    }
    return previewNextFire(specInput, new Date(nowTick))
  }, [specType, expr, tz, secs, ts, nowTick])

  const onCreate = async () => {
    if (name.trim() === '' || prompt.trim() === '') {
      setError('名称和 prompt 必填')
      return
    }
    if (preview.isError) {
      setError(`调度规格无效：${preview.message}`)
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
      // `effective_enabled` reflects module suspend/system-disable on top of
      // `enabled`; older daemons never send it, so `?? enabled` keeps this
      // desktop build working against them (design §8.2/§13 1.2.2d).
      const eff = s.effective_enabled ?? s.enabled
      await updateSchedule(s.id, { enabled: !eff })
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
      // A module row's run_now returns a fire_id (delivered to the module,
      // no AgentRun happens), not a run_id — tell them apart by prefix
      // (design §8.2) so the notice never reads "已触发运行 undefined".
      const outcome = await runScheduleNow(id)
      setNotice(
        outcome.startsWith('fire_') ? `已投递给模块（fire_id ${outcome}）` : `已触发运行 ${outcome}`,
      )
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
            {preview.isError ? (
              <span style={{ color: '#e05050' }}>下次触发：{preview.message}</span>
            ) : preview.next ? (
              <span style={{ color: 'var(--muted)' }}>
                下次触发：{formatPreview(preview.next)}
                {preview.approximate && '（近似 · 服务端按所选时区精确计算）'}
              </span>
            ) : (
              <span style={{ color: 'var(--muted)' }}>下次触发：{preview.message}</span>
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
        {schedules.map((s) => {
          // `?? enabled` is the load-bearing forward-compat fallback (design
          // §8.2): an older daemon never sends `effective_enabled`, so this
          // desktop build must keep reading plain `enabled` for it.
          const eff = s.effective_enabled ?? s.enabled
          const reason = s.owner ? disabledReasonText(s.disabled_by) : null
          return (
            <div
              key={s.id}
              style={{
                border: '1px solid var(--border)',
                borderRadius: 8,
                padding: 12,
                opacity: eff ? 1 : 0.55,
              }}
            >
              <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
                <strong style={{ fontSize: 13 }}>{s.name}</strong>
                <span style={{ fontSize: 10, color: 'var(--muted)' }}>{specSummary(s.spec)}</span>
                {s.owner && (
                  <span style={{ fontSize: 10, color: 'var(--muted)' }}>
                    来自模块 {s.owner.module}
                  </span>
                )}
                {reason && (
                  <span style={{ fontSize: 10, color: '#e0a020' }}>{reason}</span>
                )}
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
                    {eff ? '禁用' : '启用'}
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
              {/* `action` is null exactly when `owner` is set (design §8.1) —
                  a module row has no AgentRun prompt to show, ever. */}
              {s.action === null && (
                <div style={{ fontSize: 11, color: 'var(--muted)', marginTop: 2 }}>
                  由模块 {s.owner?.module ?? '未知模块'} 处理
                </div>
              )}
            </div>
          )
        })}
      </div>
    </div>
  )
}

function disabledReasonText(by: DisabledBy | undefined): string | null {
  switch (by) {
    case 'user':
      return '已被你暂停'
    case 'system':
      return '系统因连续失败禁用'
    case 'module':
      return '模块自己停用'
    default:
      return null
  }
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
