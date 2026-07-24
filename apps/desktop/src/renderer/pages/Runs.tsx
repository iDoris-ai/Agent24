import { useState, useEffect, useCallback } from 'react'
import { listRuns, cancelRun, type Run, type RunStatus } from './agent/api'

const STATUS_LABELS: Record<RunStatus, { label: string; color: string }> = {
  queued: { label: '排队', color: '#888' },
  running: { label: '运行中', color: '#e0a020' },
  awaiting_approval: { label: '待审批', color: '#b060d0' },
  completed: { label: '已完成', color: '#4caf50' },
  failed: { label: '失败', color: '#e05050' },
  cancelled: { label: '已取消', color: '#666' },
}

function isTerminal(s: RunStatus): boolean {
  return s === 'completed' || s === 'failed' || s === 'cancelled'
}

export default function RunsPage() {
  const [runs, setRuns] = useState<Run[]>([])
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  const refresh = useCallback(() => {
    listRuns()
      .then((r) => {
        setRuns(r)
        setError(null)
      })
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
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

  return (
    <div className="content">
      <div className="page-title">运行任务</div>
      <div className="page-sub">Agent 运行历史 · 每 3 秒刷新</div>

      {error && <div style={{ color: '#e05050', fontSize: 12, marginBottom: 8 }}>{error}</div>}

      <div style={{ display: 'flex', gap: 12 }}>
        {/* List */}
        <div style={{ flex: '0 0 42%', display: 'flex', flexDirection: 'column', gap: 4 }}>
          {runs.length === 0 && (
            <div style={{ fontSize: 12, color: 'var(--muted)', padding: '8px 0' }}>
              暂无运行任务
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
                  <span style={{ color: st.color, fontSize: 11, fontWeight: 600 }}>{st.label}</span>
                  {run.schedule_id && (
                    <span title="由调度触发" style={{ fontSize: 10 }}>⏰</span>
                  )}
                  <span style={{ marginLeft: 'auto', fontSize: 10, color: 'var(--muted)' }}>
                    {run.usage.total_tokens} tok
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
            <div style={{ fontSize: 12, color: 'var(--muted)' }}>选择一个任务查看详情</div>
          )}
          {selected && (
            <div>
              <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 8 }}>
                <span style={{ color: STATUS_LABELS[selected.status].color, fontWeight: 600 }}>
                  {STATUS_LABELS[selected.status].label}
                </span>
                <code style={{ fontSize: 11, color: 'var(--muted)' }}>{selected.id}</code>
                {!isTerminal(selected.status) && (
                  <button
                    className="btn"
                    style={{ marginLeft: 'auto', fontSize: 11 }}
                    onClick={() => onCancel(selected.id)}
                  >
                    取消
                  </button>
                )}
              </div>
              <div style={{ fontSize: 12, marginBottom: 8 }}>
                <strong>Prompt:</strong> {selected.input.prompt}
              </div>
              {selected.output && (
                <div style={{ fontSize: 12, marginBottom: 8, whiteSpace: 'pre-wrap' }}>
                  <strong>输出:</strong> {selected.output.text}
                </div>
              )}
              {selected.error && (
                <div style={{ fontSize: 12, color: '#e05050', marginBottom: 8 }}>
                  <strong>错误 [{selected.error.code}]:</strong> {selected.error.message}
                </div>
              )}
              <div style={{ fontSize: 11, color: 'var(--muted)' }}>
                创建 {selected.created_at}
                {selected.ended_at && ` · 结束 ${selected.ended_at}`}
                {' · '}
                {selected.usage.total_tokens} tokens
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
