import { useState, useEffect } from 'react'
import { useT } from '../i18n'
import { serviceBoxDemoMessages } from './ServiceBoxDemo.i18n'

interface StatusResult { running: boolean; hostPort: number | null }
interface EchoResult { echo: string; from: string }

export default function ServiceBoxDemoPage() {
  const t = useT(serviceBoxDemoMessages)
  const [status, setStatus] = useState<StatusResult | null>(null)
  const [msg, setMsg] = useState<string | null>(null)
  const [echoResult, setEchoResult] = useState('')
  const [echoError, setEchoError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)

  useEffect(() => {
    const poll = () => {
      void window.agent24.backendProxy({ method: 'GET', path: '/api/service-box/status' })
        .then((res) => { if (res.ok) setStatus(res.data as StatusResult) })
        .catch(() => {/* backend not ready */})
    }
    poll()
    const t = setInterval(poll, 3000)
    return () => clearInterval(t)
  }, [])

  async function callEcho() {
    if (loading || !status?.running) return
    setLoading(true)
    setEchoResult('')
    setEchoError(null)
    try {
      const res = await window.agent24.backendProxy({
        method: 'GET',
        path: `/api/svc/example-service-box/api/echo?msg=${encodeURIComponent(msg ?? t('echo.defaultMessage'))}`,
      })
      const data = res.data as EchoResult
      setEchoResult(JSON.stringify(data, null, 2))
    } catch (err) {
      setEchoError(String(err))
    } finally {
      setLoading(false)
    }
  }

  return (
    <div className="content">
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 6 }}>
        <div className="page-title">{t('title')}</div>
        <span style={{ fontSize: 11, color: status?.running ? 'var(--accent)' : 'var(--muted)' }}>
          {status === null ? t('status.checking') : status.running ? t('status.running', { port: String(status.hostPort) }) : t('status.starting')}
        </span>
      </div>
      <div className="page-sub">{t('subtitle')}</div>

      {status !== null && !status.running && (
        <div style={{ fontSize: 12, color: 'var(--muted)', padding: '10px 0' }}>
          {t('startup.hint')}
        </div>
      )}

      <div style={{ display: 'flex', flexDirection: 'column', gap: 12, marginTop: 8 }}>
        <div>
          <div style={{ fontSize: 11, fontWeight: 600, color: 'var(--muted)', marginBottom: 6, textTransform: 'uppercase', letterSpacing: '0.05em' }}>
            {t('echo.title')}
          </div>
          <div style={{ display: 'flex', gap: 8 }}>
            <input
              value={msg ?? t('echo.defaultMessage')}
              onChange={(e) => setMsg(e.target.value)}
              placeholder={t('echo.placeholder')}
              style={{
                flex: 1, fontFamily: 'monospace', fontSize: 13,
                padding: '8px 12px', background: 'var(--surface2)',
                color: 'var(--text)', border: '1px solid var(--border)',
                borderRadius: 8,
              }}
            />
            <button
              className="btn btn-primary"
              style={{ fontSize: 12 }}
              onClick={() => void callEcho()}
              disabled={loading || !status?.running}
            >
              {loading ? t('echo.calling') : t('echo.call')}
            </button>
          </div>
        </div>

        <div>
          <div style={{ fontSize: 11, fontWeight: 600, color: 'var(--muted)', marginBottom: 6, textTransform: 'uppercase', letterSpacing: '0.05em' }}>
            {t('echo.response')}
          </div>
          <pre style={{
            minHeight: 80, padding: '10px 12px',
            background: 'var(--surface2)', border: '1px solid var(--border)',
            borderRadius: 8, fontFamily: 'monospace', fontSize: 12,
            whiteSpace: 'pre-wrap', color: 'var(--text)', margin: 0,
          }}>
            {loading ? t('echo.calling') : echoError ? t('echo.error', { error: echoError }) : echoResult || t('echo.empty')}
          </pre>
        </div>

        <div style={{ fontSize: 11, color: 'var(--muted)', lineHeight: 1.8 }}>
          <strong>{t('route.path')}</strong> <code>/api/svc/example-service-box/api/echo?msg=…</code><br />
          <strong>{t('image.label')}</strong> <code>python:slim</code> · {t('image.port')} <code>8000</code><br />
          <strong>{t('api.available')}</strong> <code>/health</code> · <code>/api/echo</code> · <code>/api/info</code>
        </div>
      </div>
    </div>
  )
}
