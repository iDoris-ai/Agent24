import { useCallback, useEffect, useRef, useState } from 'react'
import {
  listMemoryAssertions,
  forgetMemoryAssertion,
  getMemorySettings,
  putMemorySettings,
  type MemoryAssertion,
} from './agent/api'

/** `YYYY-MM-DD HH:mm` from an ISO timestamp — falls back to the raw string
 *  for anything this build cannot parse, so a surprising value is still
 *  shown rather than silently hidden. */
function formatRecordedAt(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`
}

export default function MemoryPage() {
  const [assertions, setAssertions] = useState<MemoryAssertion[]>([])
  const [query, setQuery] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [confirmingId, setConfirmingId] = useState<string | null>(null)
  const [enabled, setEnabled] = useState<boolean | null>(null)
  const [settingsError, setSettingsError] = useState<string | null>(null)
  // Review Low: disabled while a PUT is in flight, so a slow request cannot
  // be fired twice by an impatient double-click.
  const [togglePending, setTogglePending] = useState(false)

  // Monotonic request token: only the latest list refresh may land, same
  // pattern as Schedules.tsx (review C7) — a slow/superseded fetch must not
  // overwrite fresher state.
  const reqSeq = useRef(0)
  const refresh = useCallback((q: string) => {
    const seq = ++reqSeq.current
    listMemoryAssertions(q)
      .then((rows) => {
        if (seq !== reqSeq.current) return
        setAssertions(rows)
        setError(null)
      })
      .catch((e: unknown) => {
        if (seq !== reqSeq.current) return
        // A failed refresh shows the error but does NOT clear what is
        // already on screen — a transient daemon hiccup must not look like
        // "all your memories are gone".
        setError(e instanceof Error ? e.message : String(e))
      })
  }, [])

  useEffect(() => {
    // Review Low: ~250ms debounce — a fresh GET per keystroke is wasted work
    // the user never sees (superseded by the next keystroke's request
    // anyway, via `reqSeq`'s own staleness check).
    const timer = setTimeout(() => refresh(query), 250)
    return () => clearTimeout(timer)
  }, [query, refresh])

  useEffect(() => {
    getMemorySettings()
      .then((s) => setEnabled(s.enabled))
      .catch((e: unknown) => setSettingsError(e instanceof Error ? e.message : String(e)))
  }, [])

  const onTogglePause = async () => {
    if (enabled === null || togglePending) return
    const next = !enabled
    setTogglePending(true)
    try {
      const s = await putMemorySettings(next)
      setEnabled(s.enabled)
      setSettingsError(null)
    } catch (e: unknown) {
      setSettingsError(e instanceof Error ? e.message : String(e))
    } finally {
      setTogglePending(false)
    }
  }

  const onConfirmForget = async (id: string) => {
    try {
      await forgetMemoryAssertion(id)
      setAssertions((prev) => prev.filter((a) => a.id !== id))
      // Review M3: a GET issued before this retract (e.g. an earlier
      // search keystroke) may still be in flight. Bump the request token
      // so its stale, pre-retract result is discarded when it lands
      // instead of overwriting the filter above, then ask for a fresh
      // list to re-sync with the server.
      reqSeq.current += 1
      refresh(query)
      setConfirmingId(null)
      setNotice('已撤回')
      setError(null)
    } catch (e: unknown) {
      // Failure: keep the item in the list (and the confirmation open) —
      // never silently drop something that was not actually retracted.
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  return (
    <div className="content">
      <div className="page-title">记忆</div>
      <div className="page-sub">Agent24 记得的事 · 查看、搜索、撤回</div>

      {/* 暂停记忆 */}
      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'space-between',
          border: '1px solid var(--border)',
          borderRadius: 8,
          padding: 12,
          marginBottom: 16,
        }}
      >
        <div>
          <div style={{ fontSize: 13, fontWeight: 600 }}>暂停记忆</div>
          <div style={{ fontSize: 11, color: 'var(--muted)', marginTop: 2 }}>
            暂停后不再写入新的记忆，也不会在新会话中被想起；已有的记忆保留
          </div>
        </div>
        <button
          role="switch"
          // Review H4: this switch's label and meaning are "暂停记忆" — checked
          // means PAUSED is on, i.e. `enabled === false`. `enabled === true`
          // (the earlier version) had it backwards: a screen reader would
          // have announced "暂停记忆: checked" while memory was actually ON.
          aria-checked={enabled === false}
          aria-label="暂停记忆"
          className={`btn${enabled === false ? ' btn-primary' : ''}`}
          style={{ fontSize: 12 }}
          disabled={enabled === null || togglePending}
          onClick={onTogglePause}
        >
          {enabled === null ? '…' : enabled ? '未暂停' : '已暂停'}
        </button>
      </div>
      {settingsError && (
        <div style={{ color: '#e05050', fontSize: 12, marginBottom: 8 }}>{settingsError}</div>
      )}

      {/* 搜索 */}
      <input
        aria-label="搜索记忆"
        placeholder="搜索记忆…"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        style={{
          fontSize: 12,
          padding: '6px 10px',
          background: 'var(--surface2)',
          border: '1px solid var(--border)',
          borderRadius: 6,
          color: 'var(--text)',
          width: '100%',
          marginBottom: 12,
          boxSizing: 'border-box',
        }}
      />

      {error && <div style={{ color: '#e05050', fontSize: 12, marginBottom: 8 }}>{error}</div>}
      {notice && <div style={{ color: '#4caf50', fontSize: 12, marginBottom: 8 }}>{notice}</div>}

      {/* Review Low: never show an empty-state copy while `error` is set —
          an empty list because the load FAILED is not the same fact as an
          empty list because there is truly nothing there, and the former
          must not be mistaken for the latter. */}
      {!error &&
        assertions.length === 0 &&
        (enabled === false ? (
          <div style={{ fontSize: 12, color: 'var(--muted)' }}>记忆已暂停</div>
        ) : query.trim() === '' ? (
          <div style={{ fontSize: 12, color: 'var(--muted)' }}>说『记住……』让我记住</div>
        ) : (
          <div style={{ fontSize: 12, color: 'var(--muted)' }}>没有匹配的记忆</div>
        ))}

      <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
        {assertions.map((a) => (
          <div
            key={a.id}
            style={{ border: '1px solid var(--border)', borderRadius: 8, padding: 12 }}
          >
            <div style={{ fontSize: 13 }}>{a.text}</div>
            <div style={{ fontSize: 11, color: 'var(--muted)', marginTop: 4 }}>
              {a.session ? `来自会话 ${a.session}` : '来源未知'} · 记录于{' '}
              {formatRecordedAt(a.recorded_at)}
            </div>

            {confirmingId === a.id ? (
              <div
                style={{
                  marginTop: 8,
                  padding: 8,
                  background: 'var(--surface2)',
                  borderRadius: 6,
                }}
              >
                <div style={{ fontSize: 12, marginBottom: 8 }}>
                  撤回后不会再在新会话中被想起；原始对话仍保留在本机
                </div>
                <div style={{ display: 'flex', gap: 6 }}>
                  <button
                    className="btn btn-primary"
                    style={{ fontSize: 11 }}
                    onClick={() => onConfirmForget(a.id)}
                  >
                    确认撤回
                  </button>
                  <button
                    className="btn"
                    style={{ fontSize: 11 }}
                    onClick={() => setConfirmingId(null)}
                  >
                    取消
                  </button>
                </div>
              </div>
            ) : (
              <div style={{ marginTop: 8 }}>
                <button
                  className="btn"
                  style={{ fontSize: 11 }}
                  onClick={() => setConfirmingId(a.id)}
                >
                  撤回
                </button>
              </div>
            )}
          </div>
        ))}
      </div>
    </div>
  )
}
