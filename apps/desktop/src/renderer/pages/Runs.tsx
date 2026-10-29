import { useState, useEffect, useCallback, useRef } from 'react'
import { listRuns, cancelRun, createRun, type Run, type RunStatus } from './agent/api'
import { useT } from '../i18n'
import { runsMessages } from './Runs.i18n'

const STATUS_LABELS: Record<RunStatus, { label: 'status.queued' | 'status.running' | 'status.awaitingApproval' | 'status.completed' | 'status.failed' | 'status.cancelled'; color: string }> = {
  queued: { label: 'status.queued', color: '#888' },
  running: { label: 'status.running', color: '#e0a020' },
  awaiting_approval: { label: 'status.awaitingApproval', color: '#b060d0' },
  completed: { label: 'status.completed', color: '#4caf50' },
  failed: { label: 'status.failed', color: '#e05050' },
  cancelled: { label: 'status.cancelled', color: '#666' },
}

function isTerminal(s: RunStatus): boolean {
  return s === 'completed' || s === 'failed' || s === 'cancelled'
}

export default function RunsPage() {
  const t = useT(runsMessages)
  const [runs, setRuns] = useState<Run[]>([])
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [retrying, setRetrying] = useState(false)
  const retryInFlight = useRef(false)

  // Only the latest refresh's result may land (review C7 poll-race guard).
  const reqSeq = useRef(0)
  const refresh = useCallback(() => {
    const seq = ++reqSeq.current
    listRuns()
      .then((r) => {
        if (seq !== reqSeq.current) return
        setRuns(r)
        setError(null)
      })
      .catch((e: unknown) => {
        if (seq !== reqSeq.current) return
        setError(e instanceof Error ? e.message : String(e))
      })
  }, [])

  useEffect(() => {
    refresh()
    const timer = setInterval(refresh, 3000)
    return () => clearInterval(timer)
  }, [refresh])

  const selected = runs.find((r) => r.id === selectedId) ?? null

  const onCancel = async (id: string) => {
    try {
      await cancelRun(id)
      refresh()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  const onConfirmRerun = async (run: Run) => {
    if (retryInFlight.current) return
    retryInFlight.current = true
    setRetrying(true)
    try {
      const rerun = await createRun({
        prompt: run.input.prompt,
        model_override: run.input.model_override,
        mode: run.input.mode,
        workspace_id: run.input.workspace_id,
        session_id: run.session_id,
        idoris_headroom_confirmed: true,
      })
      reqSeq.current += 1
      setRuns(prev => [rerun, ...prev])
      setSelectedId(rerun.id)
      setError(null)
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      retryInFlight.current = false
      setRetrying(false)
    }
  }

  return (
    <div className="content">
      <div className="page-title">{t('title')}</div>
      <div className="page-sub">{t('subtitle')}</div>

      {error && <div style={{ color: '#e05050', fontSize: 12, marginBottom: 8 }}>{error}</div>}

      <div style={{ display: 'flex', gap: 12 }}>
        {/* List */}
        <div style={{ flex: '0 0 42%', display: 'flex', flexDirection: 'column', gap: 4 }}>
          {runs.length === 0 && (
            <div style={{ fontSize: 12, color: 'var(--muted)', padding: '8px 0' }}>
              {t('list.empty')}
            </div>
          )}
          {runs.map((run) => {
            const st = STATUS_LABELS[run.status]
            return (
              <button
                key={run.id}
                onClick={() => setSelectedId(run.id)}
                className={`run-row${selectedId === run.id ? ' active' : ''}`}
                style={{
                  textAlign: 'left',
                  padding: '8px 12px',
                  background: selectedId === run.id ? 'var(--surface2)' : 'transparent',
                  border: '1px solid var(--border)',
                  borderRadius: 8,
                  cursor: 'pointer',
                  color: 'var(--text)',
                }}
              >
                <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <span style={{ color: st.color, fontSize: 11, fontWeight: 600 }}>{t(st.label)}</span>
                  {run.schedule_id && (
                    <span title={t('list.scheduled')} style={{ fontSize: 10 }}>⏰</span>
                  )}
                  <span style={{ marginLeft: 'auto', fontSize: 10, color: 'var(--muted)' }}>
                    {t('usage.short', { count: run.usage.total_tokens })}
                  </span>
                </div>
                <div
                  style={{
                    fontSize: 12,
                    marginTop: 2,
                    whiteSpace: 'nowrap',
                    overflow: 'hidden',
                    textOverflow: 'ellipsis',
                  }}
                >
                  {run.input.prompt}
                </div>
              </button>
            )
          })}
        </div>

        {/* Detail */}
        <div style={{ flex: 1, border: '1px solid var(--border)', borderRadius: 8, padding: 12 }}>
          {!selected && (
            <div style={{ fontSize: 12, color: 'var(--muted)' }}>{t('detail.select')}</div>
          )}
          {selected && (
            <div>
              <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 8 }}>
                <span style={{ color: STATUS_LABELS[selected.status].color, fontWeight: 600 }}>
                  {t(STATUS_LABELS[selected.status].label)}
                </span>
                <code style={{ fontSize: 11, color: 'var(--muted)' }}>{selected.id}</code>
                {!isTerminal(selected.status) && (
                  <button
                    className="btn"
                    style={{ marginLeft: 'auto', fontSize: 11 }}
                    onClick={() => onCancel(selected.id)}
                  >
                    {t('detail.cancel')}
                  </button>
                )}
              </div>
              <div style={{ fontSize: 12, marginBottom: 8 }}>
                <strong>{t('detail.prompt')}</strong> {selected.input.prompt}
              </div>
              {selected.output && (
                <div style={{ fontSize: 12, marginBottom: 8, whiteSpace: 'pre-wrap' }}>
                  <strong>{t('detail.output')}</strong> {selected.output.text}
                </div>
              )}
              {selected.status === 'failed' && selected.error?.code === 'idoris_headroom_confirmation_required' ? (
                <div style={{ fontSize: 12, marginBottom: 8 }}>
                  <p>{t('headroom.explanation')}</p>
                  {selected.input.knowledge_binding_id
                    ? <p>{t('headroom.hostOnly')}</p>
                    : <button className="btn" disabled={retrying} onClick={() => void onConfirmRerun(selected)}>{t('headroom.confirm')}</button>}
                </div>
              ) : selected.error?.code === 'idoris_supervisor_busy' ? (
                <p role="status">{t('error.supervisorBusy')}</p>
              ) : selected.error && (
                <div style={{ fontSize: 12, color: '#e05050', marginBottom: 8 }}>
                  <strong>{t('detail.error', { code: selected.error.code })}</strong> {selected.error.message}
                </div>
              )}
              <div style={{ fontSize: 11, color: 'var(--muted)' }}>
                {t('detail.created', { date: selected.created_at })}
                {selected.ended_at && ` · ${t('detail.ended', { date: selected.ended_at })}`}
                {' · '}
                {t('usage.full', { count: selected.usage.total_tokens })}
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
