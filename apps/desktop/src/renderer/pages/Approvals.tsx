import { useState, useEffect, useRef, useCallback } from 'react'
import { listPendingApprovals, decideApproval, type Approval } from './agent/api'

const DECISION_LABELS: Record<string, string> = {
  approve: '批准',
  approve_for_session: '本会话免问',
  deny: '拒绝',
  abort: '中止运行',
}

/** Fire a desktop notification for a newly-seen approval (best-effort — never
 *  throws or leaves an unhandled rejection). */
function notify(approval: Approval): void {
  if (typeof Notification === 'undefined') return
  const show = () => {
    try {
      new Notification('需要审批', {
        body: approval.summary,
        tag: approval.id, // dedup repeat notifications for the same approval
      })
    } catch {
      /* notifications are best-effort */
    }
  }
  if (Notification.permission === 'granted') {
    show()
  } else if (Notification.permission !== 'denied') {
    void Notification.requestPermission()
      .then((p) => {
        if (p === 'granted') show()
      })
      .catch(() => {
        /* permission request unavailable — ignore */
      })
  }
}

export default function ApprovalsPage() {
  const [approvals, setApprovals] = useState<Approval[]>([])
  const [error, setError] = useState<string | null>(null)
  // Which approval is in deny-reason entry
  const [denyingId, setDenyingId] = useState<string | null>(null)
  const [reason, setReason] = useState('')
  const seenIds = useRef<Set<string>>(new Set())

  // Only the latest refresh's result may land, so a slow poll can't resurrect
  // an approval the user just decided (review C7 poll-race guard).
  const reqSeq = useRef(0)
  const refresh = useCallback(() => {
    const seq = ++reqSeq.current
    listPendingApprovals()
      .then((list) => {
        if (seq !== reqSeq.current) return
        setApprovals(list)
        setError(null)
        // Notify for any approval id not seen before this session
        for (const a of list) {
          if (!seenIds.current.has(a.id)) {
            seenIds.current.add(a.id)
            notify(a)
          }
        }
      })
      .catch((e: unknown) => {
        if (seq !== reqSeq.current) return
        setError(e instanceof Error ? e.message : String(e))
      })
  }, [])

  useEffect(() => {
    refresh()
    const timer = setInterval(refresh, 2000)
    return () => clearInterval(timer)
  }, [refresh])

  const decide = async (id: string, type: string, denyReason?: string) => {
    try {
      await decideApproval(id, { type, reason: denyReason })
      setDenyingId(null)
      setReason('')
      refresh()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  return (
    <div className="content">
      <div className="page-title">待审批 ({approvals.length})</div>
      <div className="page-sub">工具执行审批 · 每 2 秒刷新 · 新审批弹系统通知</div>

      {error && <div style={{ color: '#e05050', fontSize: 12, marginBottom: 8 }}>{error}</div>}

      {approvals.length === 0 && (
        <div style={{ fontSize: 12, color: 'var(--muted)', padding: '8px 0' }}>
          没有待处理的审批
        </div>
      )}

      <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
        {approvals.map((a) => (
          <div
            key={a.id}
            style={{ border: '1px solid var(--border)', borderRadius: 8, padding: 12 }}
          >
            <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 6 }}>
              <span
                style={{
                  fontSize: 11,
                  fontWeight: 600,
                  color: '#b060d0',
                  textTransform: 'uppercase',
                }}
              >
                {a.kind}
              </span>
              <code style={{ fontSize: 10, color: 'var(--muted)' }}>{a.run_id}</code>
            </div>
            <div style={{ fontSize: 13, marginBottom: 10 }}>{a.summary}</div>

            {denyingId === a.id ? (
              <div style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
                <input
                  autoFocus
                  aria-label="拒绝原因"
                  placeholder="拒绝原因（必填）"
                  value={reason}
                  onChange={(e) => setReason(e.target.value)}
                  style={{
                    flex: 1,
                    fontSize: 12,
                    padding: '4px 8px',
                    background: 'var(--surface2)',
                    border: '1px solid var(--border)',
                    borderRadius: 6,
                    color: 'var(--text)',
                  }}
                />
                <button
                  className="btn btn-primary"
                  style={{ fontSize: 11 }}
                  disabled={reason.trim() === ''}
                  onClick={() => decide(a.id, 'deny', reason.trim())}
                >
                  确认拒绝
                </button>
                <button
                  className="btn"
                  style={{ fontSize: 11 }}
                  onClick={() => {
                    setDenyingId(null)
                    setReason('')
                  }}
                >
                  返回
                </button>
              </div>
            ) : (
              <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap' }}>
                {a.available_decisions.map((d) => (
                  <button
                    key={d}
                    className={`btn${d === 'approve' ? ' btn-primary' : ''}`}
                    style={{ fontSize: 11 }}
                    onClick={() => {
                      if (d === 'deny') {
                        setDenyingId(a.id)
                        setReason('')
                      } else {
                        void decide(a.id, d)
                      }
                    }}
                  >
                    {DECISION_LABELS[d] ?? d}
                  </button>
                ))}
              </div>
            )}
          </div>
        ))}
      </div>
    </div>
  )
}
