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
import { useT, useLocale, type T } from '../i18n'
import { schedulesMessages } from './Schedules.i18n'

type SchedKey = keyof typeof schedulesMessages.en

type SpecType = 'cron' | 'every' | 'at'

function buildSpec(type: SpecType, expr: string, tz: string, secs: string, ts: string): ScheduleSpec {
  if (type === 'cron') return { type: 'cron', expr, tz: tz.trim() || null }
  if (type === 'every') return { type: 'every', secs: Number(secs) }
  return { type: 'at', ts }
}

export default function SchedulesPage() {
  const t = useT(schedulesMessages)
  const locale = useLocale()
  const [schedules, setSchedules] = useState<Schedule[]>([])
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [showFired, setShowFired] = useState(false)

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
    // locale is a dep because previewNextFire translates its messages
  }, [specType, expr, tz, secs, ts, nowTick, locale])

  const onCreate = async () => {
    if (name.trim() === '' || prompt.trim() === '') {
      setError(t('error.required'))
      return
    }
    if (preview.isError) {
      setError(t('error.invalidSpec', { msg: preview.message ?? '' }))
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
      setNotice(t('notice.created'))
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

  // Bulk-remove the fired one-time rows. Uses the existing per-id DELETE (no
  // new API); allSettled so one failure doesn't strand the rest.
  const onClearFired = async (ids: string[]) => {
    const results = await Promise.allSettled(ids.map((id) => deleteSchedule(id)))
    const failed = results.filter((r) => r.status === 'rejected')
    if (failed.length > 0) {
      const first = (failed[0] as PromiseRejectedResult).reason
      setError(first instanceof Error ? first.message : String(first))
    } else {
      setError(null)
      setNotice(t('notice.cleared', { n: ids.length }))
    }
    refresh()
  }

  const onRunNow = async (id: string) => {
    try {
      // A module row's run_now returns a fire_id (delivered to the module,
      // no AgentRun happens), not a run_id — tell them apart by prefix
      // (design §8.2) so the notice never reads "已触发运行 undefined".
      const outcome = await runScheduleNow(id)
      setNotice(
        outcome.startsWith('fire_') ? t('notice.firedToModule', { id: outcome }) : t('notice.runTriggered', { id: outcome }),
      )
      refresh()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  const fired = schedules.filter(isFiredOneTime)
  const active = schedules.filter((x) => !isFiredOneTime(x))

  const renderRow = (s: Schedule) => {
          // `?? enabled` is the load-bearing forward-compat fallback (design
          // §8.2): an older daemon never sends `effective_enabled`, so this
          // desktop build must keep reading plain `enabled` for it.
          const eff = s.effective_enabled ?? s.enabled
          const reason = s.owner ? disabledReasonText(s.disabled_by, t) : null
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
                <span style={{ fontSize: 10, color: 'var(--muted)' }}>{specSummary(s.spec, t)}</span>
                {s.owner && (
                  <span style={{ fontSize: 10, color: 'var(--muted)' }}>
                    {t('item.fromModule', { module: s.owner.module })}
                  </span>
                )}
                {reason && (
                  <span style={{ fontSize: 10, color: '#e0a020' }}>{reason}</span>
                )}
                {s.consecutive_failures > 0 && (
                  <span style={{ fontSize: 10, color: '#e0a020' }}>
                    {t('item.failures', { n: s.consecutive_failures })}
                  </span>
                )}
                <div style={{ marginLeft: 'auto', display: 'flex', gap: 6 }}>
                  <button className="btn" style={{ fontSize: 11 }} onClick={() => onRunNow(s.id)}>
                    {t('item.runNow')}
                  </button>
                  <button className="btn" style={{ fontSize: 11 }} onClick={() => onToggle(s)}>
                    {eff ? t('item.disable') : t('item.enable')}
                  </button>
                  <button className="btn" style={{ fontSize: 11 }} onClick={() => onDelete(s.id)}>
                    {t('item.delete')}
                  </button>
                </div>
              </div>
              <div style={{ fontSize: 11, color: 'var(--muted)', marginTop: 4 }}>
                {s.next_run_at ? t('item.next', { at: s.next_run_at }) : t('item.noNext')}
                {s.last_run_at && t('item.last', { at: s.last_run_at })}
              </div>
              {/* `action` is null exactly when `owner` is set (design §8.1) —
                  a module row has no AgentRun prompt to show, ever. */}
              {s.action === null && (
                <div style={{ fontSize: 11, color: 'var(--muted)', marginTop: 2 }}>
                  {t('item.handledBy', { module: s.owner?.module ?? t('item.unknownModule') })}
                </div>
              )}
            </div>
          )
  }

  return (
    <div className="content">
      <div className="page-title">{t('title')}</div>
      <div className="page-sub">{t('sub')}</div>

      {error && <div style={{ color: '#e05050', fontSize: 12, marginBottom: 8 }}>{error}</div>}
      {notice && <div style={{ color: '#4caf50', fontSize: 12, marginBottom: 8 }}>{notice}</div>}

      {/* Create form */}
      <div style={{ border: '1px solid var(--border)', borderRadius: 8, padding: 12, marginBottom: 16 }}>
        <div style={{ fontSize: 12, fontWeight: 600, marginBottom: 8 }}>{t('form.title')}</div>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
          <input
            aria-label={t('form.nameLabel')}
            placeholder={t('form.namePlaceholder')}
            value={name}
            onChange={(e) => setName(e.target.value)}
            style={inputStyle}
          />
          <input
            aria-label={t('form.promptLabel')}
            placeholder={t('form.promptPlaceholder')}
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            style={inputStyle}
          />
          <div style={{ display: 'flex', gap: 6 }}>
            {(['cron', 'every', 'at'] as SpecType[]).map((st) => (
              <button
                key={st}
                className={`btn${specType === st ? ' btn-primary' : ''}`}
                style={{ fontSize: 11 }}
                onClick={() => setSpecType(st)}
              >
                {t(`form.type.${st}`)}
              </button>
            ))}
          </div>

          {specType === 'cron' && (
            <div style={{ display: 'flex', gap: 6 }}>
              <input
                aria-label={t('form.cronLabel')}
                placeholder="0 8 * * *"
                value={expr}
                onChange={(e) => setExpr(e.target.value)}
                style={{ ...inputStyle, flex: 2, fontFamily: 'monospace' }}
              />
              <input
                aria-label={t('form.tzLabel')}
                placeholder="UTC"
                value={tz}
                onChange={(e) => setTz(e.target.value)}
                style={{ ...inputStyle, flex: 1 }}
              />
            </div>
          )}
          {specType === 'every' && (
            <input
              aria-label={t('form.secsLabel')}
              type="number"
              placeholder={t('form.secsPlaceholder')}
              value={secs}
              onChange={(e) => setSecs(e.target.value)}
              style={inputStyle}
            />
          )}
          {specType === 'at' && (
            <input
              aria-label={t('form.tsLabel')}
              placeholder="2026-08-01T09:00:00Z"
              value={ts}
              onChange={(e) => setTs(e.target.value)}
              style={{ ...inputStyle, fontFamily: 'monospace' }}
            />
          )}

          {/* Live preview */}
          <div style={{ fontSize: 12 }} data-testid="preview">
            {preview.isError ? (
              <span style={{ color: '#e05050' }}>{t('preview.next', { value: preview.message ?? '' })}</span>
            ) : preview.next ? (
              <span style={{ color: 'var(--muted)' }}>
                {t('preview.next', { value: formatPreview(preview.next) })}
                {preview.approximate && t('preview.approx')}
              </span>
            ) : (
              <span style={{ color: 'var(--muted)' }}>{t('preview.next', { value: preview.message ?? '' })}</span>
            )}
          </div>

          <button className="btn btn-primary" style={{ fontSize: 12, alignSelf: 'flex-start' }} onClick={onCreate}>
            {t('form.create')}
          </button>
        </div>
      </div>

      {/* List */}
      <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
        {schedules.length === 0 && (
          <div style={{ fontSize: 12, color: 'var(--muted)' }}>{t('list.empty')}</div>
        )}
        {active.map(renderRow)}
      </div>

      {/* Fired one-time rows (e.g. the agent's self-wake): kept for audit, but
          folded away so they don't bury live schedules. */}
      {fired.length > 0 && (
        <div style={{ marginTop: 16 }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <span style={{ fontSize: 12, color: 'var(--muted)' }}>{t('fired.summary', { n: fired.length })}</span>
            <button className="btn" style={{ fontSize: 11 }} onClick={() => setShowFired((v) => !v)}>
              {showFired ? t('fired.collapse') : t('fired.expand')}
            </button>
            <button
              className="btn"
              style={{ fontSize: 11 }}
              onClick={() => onClearFired(fired.map((f) => f.id))}
            >
              {t('fired.clear')}
            </button>
          </div>
          {showFired && (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 6, marginTop: 8 }}>
              {fired.map(renderRow)}
            </div>
          )}
        </div>
      )}
    </div>
  )
}

/** A one-time (`at`) user schedule that has already fired: nothing left to run
 *  (`next_run_at` cleared) and `last_run_at` proves it fired. Module-owned rows
 *  are never folded away; a one-time row that never fired stays visible. */
export function isFiredOneTime(s: Schedule): boolean {
  return !s.owner && s.spec.type === 'at' && !s.next_run_at && !!s.last_run_at
}

function disabledReasonText(by: DisabledBy | undefined, t: T<SchedKey>): string | null {
  switch (by) {
    case 'user':
      return t('disabled.user')
    case 'system':
      return t('disabled.system')
    case 'module':
      return t('disabled.module')
    default:
      return null
  }
}

function specSummary(spec: ScheduleSpec, t: T<SchedKey>): string {
  if (spec.type === 'cron') return `cron ${spec.expr}${spec.tz ? ` @${spec.tz}` : ''}`
  if (spec.type === 'every') return t('spec.every', { secs: spec.secs })
  return t('spec.at', { ts: spec.ts })
}

const inputStyle: React.CSSProperties = {
  fontSize: 12,
  padding: '5px 8px',
  background: 'var(--surface2)',
  border: '1px solid var(--border)',
  borderRadius: 6,
  color: 'var(--text)',
}
