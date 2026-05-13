import { useState, useEffect } from 'react'

interface StatusResult { running: boolean; hostPort: number | null }
interface EchoResult { echo: string; from: string }

export default function ServiceBoxDemoPage() {
  const [status, setStatus] = useState<StatusResult | null>(null)
  const [msg, setMsg] = useState('Hello from Agent24!')
  const [echoResult, setEchoResult] = useState('')
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
    setEchoResult('调用中…')
    try {
      const res = await window.agent24.backendProxy({
        method: 'GET',
        path: `/api/svc/example-service-box/api/echo?msg=${encodeURIComponent(msg)}`,
      })
      const data = res.data as EchoResult
      setEchoResult(JSON.stringify(data, null, 2))
    } catch (err) {
      setEchoResult(`Error: ${String(err)}`)
    } finally {
      setLoading(false)
    }
  }

  return (
    <div className="content">
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 6 }}>
        <div className="page-title">服务容器示例</div>
        <span style={{ fontSize: 11, color: status?.running ? 'var(--accent)' : 'var(--muted)' }}>
          {status === null ? '检测中…' : status.running ? `● 运行中 :${status.hostPort}` : '● 容器启动中…'}
        </span>
      </div>
      <div className="page-sub">BoxLite 长期服务容器 · python:slim · 请求通过 /api/svc/* 代理</div>

      {status !== null && !status.running && (
        <div style={{ fontSize: 12, color: 'var(--muted)', padding: '10px 0' }}>
          容器正在启动，首次需拉取 python:slim 镜像（约 1-2 分钟）…
        </div>
      )}

      <div style={{ display: 'flex', flexDirection: 'column', gap: 12, marginTop: 8 }}>
        <div>
          <div style={{ fontSize: 11, fontWeight: 600, color: 'var(--muted)', marginBottom: 6, textTransform: 'uppercase', letterSpacing: '0.05em' }}>
            Echo 接口测试
          </div>
          <div style={{ display: 'flex', gap: 8 }}>
            <input
              value={msg}
              onChange={(e) => setMsg(e.target.value)}
              placeholder="输入消息…"
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
              {loading ? '调用中…' : '▶ 调用'}
            </button>
          </div>
        </div>

        <div>
          <div style={{ fontSize: 11, fontWeight: 600, color: 'var(--muted)', marginBottom: 6, textTransform: 'uppercase', letterSpacing: '0.05em' }}>
            响应
          </div>
          <pre style={{
            minHeight: 80, padding: '10px 12px',
            background: 'var(--surface2)', border: '1px solid var(--border)',
            borderRadius: 8, fontFamily: 'monospace', fontSize: 12,
            whiteSpace: 'pre-wrap', color: 'var(--text)', margin: 0,
          }}>
            {echoResult || '点击"▶ 调用"发送请求到容器服务'}
          </pre>
        </div>

        <div style={{ fontSize: 11, color: 'var(--muted)', lineHeight: 1.8 }}>
          <strong>路由路径：</strong> <code>/api/svc/example-service-box/api/echo?msg=…</code><br />
          <strong>容器镜像：</strong> <code>python:slim</code> · 端口 <code>8000</code><br />
          <strong>可用接口：</strong> <code>/health</code> · <code>/api/echo</code> · <code>/api/info</code>
        </div>
      </div>
    </div>
  )
}
