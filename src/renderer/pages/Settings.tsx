import { useEffect, useRef, useState } from 'react'

type OmlxStatus = 'detecting' | 'connected' | 'disconnected' | 'starting' | 'stopping'

export default function SettingsPage() {
  const [omlxStatus, setOmlxStatus] = useState<OmlxStatus>('detecting')
  const [omlxUrl, setOmlxUrl] = useState('http://127.0.0.1:8088')
  const [omlxApiKey, setOmlxApiKey] = useState('')
  const [omlxModels, setOmlxModels] = useState<string[]>([])
  const [omlxActiveModel, setOmlxActiveModel] = useState('')
  const [omlxPort, setOmlxPort] = useState('8000')
  const [detectError, setDetectError] = useState('')
  const detectRan = useRef(false)

  useEffect(() => {
    if (detectRan.current) return
    detectRan.current = true
    autoDetect()
  }, [])

  async function autoDetect() {
    setOmlxStatus('detecting')
    setDetectError('')
    const result = await window.agent24.omlxDetect()
    if (result) {
      setOmlxUrl(result.url)
      setOmlxApiKey(result.apiKey)
      setOmlxModels(result.models)
      setOmlxActiveModel(result.models[0] ?? '')
      setOmlxStatus('connected')
    } else {
      setOmlxStatus('disconnected')
      setDetectError('未检测到运行中的 oMLX 服务，请手动填写地址或点击启动')
    }
  }

  async function testConnection() {
    setOmlxStatus('detecting')
    setDetectError('')
    const result = await window.agent24.omlxModels(omlxUrl, omlxApiKey)
    if (result.ok) {
      setOmlxModels(result.models)
      setOmlxActiveModel(result.models[0] ?? '')
      setOmlxStatus('connected')
    } else {
      setOmlxStatus('disconnected')
      setDetectError(result.error ?? '连接失败')
    }
  }

  async function startServer() {
    setOmlxStatus('starting')
    const port = parseInt(omlxPort, 10) || 8000
    const result = await window.agent24.omlxStart(port, omlxApiKey)
    if (result.ok) {
      setOmlxUrl(result.url)
      setTimeout(() => testConnection(), 3000)
    } else {
      setOmlxStatus('disconnected')
      setDetectError(result.error ?? '启动失败')
    }
  }

  async function stopServer() {
    setOmlxStatus('stopping')
    await window.agent24.omlxStop()
    setOmlxModels([])
    setOmlxActiveModel('')
    setOmlxStatus('disconnected')
  }

  const isConnected = omlxStatus === 'connected'
  const isBusy = omlxStatus === 'detecting' || omlxStatus === 'starting' || omlxStatus === 'stopping'

  const statusLabel: Record<OmlxStatus, string> = {
    detecting: '探测中…', connected: '已连接',
    disconnected: '未连接', starting: '启动中…', stopping: '停止中…',
  }
  const statusDotColor: Record<OmlxStatus, string> = {
    detecting: '#888', connected: '#4caf50',
    disconnected: '#f44336', starting: '#ff9800', stopping: '#ff9800',
  }

  return (
    <div className="content">
      <div className="page-title">设置</div>
      <div className="page-sub">配置 LLM 运行时、后端服务和应用偏好</div>

      {/* ── oMLX 推理服务 ── */}
      <div className="settings-section">
        <h3>oMLX 推理服务</h3>

        {/* Status bar */}
        <div className="setting-row" style={{ background: 'var(--surface2)', marginBottom: 2 }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <span style={{
              width: 8, height: 8, borderRadius: '50%', flexShrink: 0,
              background: statusDotColor[omlxStatus],
              boxShadow: isConnected ? `0 0 6px ${statusDotColor[omlxStatus]}66` : 'none',
            }} />
            <span style={{ fontSize: 13 }}>{statusLabel[omlxStatus]}</span>
            {isConnected && (
              <span style={{ fontSize: 11, color: 'var(--muted)' }}>
                {omlxUrl} · {omlxModels.length} 个模型
              </span>
            )}
          </div>
          <div style={{ display: 'flex', gap: 6 }}>
            <button className="btn btn-ghost" onClick={autoDetect} disabled={isBusy}
              style={{ fontSize: 12, padding: '4px 10px' }}>
              自动探测
            </button>
            {isConnected ? (
              <button className="btn btn-ghost" onClick={stopServer} disabled={isBusy}
                style={{ fontSize: 12, padding: '4px 10px', color: '#f44336', borderColor: '#f44336' }}>
                停止服务
              </button>
            ) : (
              <button className="btn btn-primary" onClick={startServer} disabled={isBusy}
                style={{ fontSize: 12, padding: '4px 10px' }}>
                启动 oMLX
              </button>
            )}
          </div>
        </div>

        {detectError && (
          <div style={{
            fontSize: 12, color: '#f44336', padding: '6px 18px', marginBottom: 2,
            background: 'rgba(244,67,54,0.08)', borderRadius: 6,
          }}>
            {detectError}
          </div>
        )}

        {/* URL */}
        <div className="setting-row">
          <div>
            <label>服务地址</label>
            <p>OpenAI 兼容 API 端点</p>
          </div>
          <div style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
            <input type="text" value={omlxUrl} onChange={(e) => setOmlxUrl(e.target.value)}
              style={{ width: 210 }} placeholder="http://127.0.0.1:8000" />
            <button className="btn btn-ghost" onClick={testConnection} disabled={isBusy}
              style={{ fontSize: 12, padding: '4px 10px', whiteSpace: 'nowrap' }}>
              测试连接
            </button>
          </div>
        </div>

        {/* API Key */}
        <div className="setting-row">
          <div>
            <label>API Key</label>
            <p>留空则不鉴权</p>
          </div>
          <input type="password" value={omlxApiKey} onChange={(e) => setOmlxApiKey(e.target.value)}
            style={{ width: 200 }} placeholder="可选" />
        </div>

        {/* Launch port — only when disconnected */}
        {!isConnected && (
          <div className="setting-row">
            <div>
              <label>启动端口</label>
              <p>点击"启动 oMLX"时使用的端口</p>
            </div>
            <input type="text" value={omlxPort} onChange={(e) => setOmlxPort(e.target.value)}
              style={{ width: 80 }} />
          </div>
        )}

        {/* Model selector */}
        <div className="setting-row">
          <div>
            <label>活跃模型</label>
            <p>对话和 Agent 任务默认使用的模型</p>
          </div>
          {omlxModels.length > 0 ? (
            <select value={omlxActiveModel} onChange={(e) => setOmlxActiveModel(e.target.value)}
              style={{ maxWidth: 280 }}>
              {omlxModels.map((m) => <option key={m} value={m}>{m}</option>)}
            </select>
          ) : (
            <span style={{ fontSize: 12, color: 'var(--muted)' }}>
              {isBusy ? '加载中…' : '连接后自动列出'}
            </span>
          )}
        </div>
      </div>

      {/* ── 后端服务 ── */}
      <div className="settings-section">
        <h3>后端服务</h3>
        <div className="setting-row">
          <div>
            <label>Daemon 端口</label>
            <p>Agent24 内部服务监听端口</p>
          </div>
          <input type="text" defaultValue="8765" style={{ width: 80 }} />
        </div>
      </div>

      {/* ── 界面 ── */}
      <div className="settings-section">
        <h3>界面</h3>
        <div className="setting-row">
          <div><label>语言</label><p>界面显示语言</p></div>
          <select defaultValue="zh">
            <option value="zh">中文</option>
            <option value="en">English</option>
          </select>
        </div>
      </div>

      <div style={{ marginTop: 8 }}>
        <button className="btn btn-primary">保存设置</button>
        <button className="btn btn-ghost" style={{ marginLeft: 8 }}>重置默认</button>
      </div>
    </div>
  )
}
