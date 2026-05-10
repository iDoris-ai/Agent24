import { useEffect, useRef, useState } from 'react'

type OmlxStatus = 'detecting' | 'connected' | 'disconnected' | 'starting' | 'stopping'
type TestResult = { ok: boolean; msg: string } | null

export default function SettingsPage() {
  const [omlxStatus, setOmlxStatus] = useState<OmlxStatus>('detecting')
  const [omlxUrl, setOmlxUrl] = useState('http://127.0.0.1:8088')
  const [omlxApiKey, setOmlxApiKey] = useState('')
  const [showApiKey, setShowApiKey] = useState(false)
  const [omlxModels, setOmlxModels] = useState<string[]>([])
  const [omlxActiveModel, setOmlxActiveModel] = useState('')
  const [omlxPort, setOmlxPort] = useState('8088')
  const [detectError, setDetectError] = useState('')
  const [testResult, setTestResult] = useState<TestResult>(null)
  const testTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const detectRan = useRef(false)

  useEffect(() => {
    if (detectRan.current) return
    detectRan.current = true
    autoDetect()
  }, [])

  function showTestResult(result: TestResult) {
    setTestResult(result)
    if (testTimer.current) clearTimeout(testTimer.current)
    testTimer.current = setTimeout(() => setTestResult(null), 4000)
  }

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
      setDetectError('No running AI model service detected. Fill in the URL below or click Start.')
    }
  }

  async function testConnection() {
    setTestResult(null)
    setDetectError('')
    const prev = omlxStatus
    setOmlxStatus('detecting')
    const result = await window.agent24.omlxModels(omlxUrl, omlxApiKey)
    if (result.ok) {
      setOmlxModels(result.models)
      setOmlxActiveModel((m) => m || result.models[0] || '')
      setOmlxStatus('connected')
      showTestResult({ ok: true, msg: `Connected — ${result.models.length} model(s) found` })
    } else {
      setOmlxStatus(prev === 'connected' ? 'disconnected' : 'disconnected')
      showTestResult({ ok: false, msg: result.error ?? 'Connection failed — check URL and API key' })
    }
  }

  async function startServer() {
    setOmlxStatus('starting')
    setDetectError('')
    const port = parseInt(omlxPort, 10) || 8088
    const result = await window.agent24.omlxStart(port, omlxApiKey)
    if (result.ok) {
      setOmlxUrl(result.url)
      // Poll until the server is ready (up to 8s)
      let attempts = 0
      const poll = async () => {
        const r = await window.agent24.omlxModels(result.url, omlxApiKey)
        if (r.ok) {
          setOmlxModels(r.models)
          setOmlxActiveModel(r.models[0] ?? '')
          setOmlxStatus('connected')
        } else if (++attempts < 4) {
          setTimeout(poll, 2000)
        } else {
          setOmlxStatus('disconnected')
          setDetectError('Service started but not responding yet — try "Auto-detect" in a moment')
        }
      }
      setTimeout(poll, 2000)
    } else {
      setOmlxStatus('disconnected')
      setDetectError(result.error ?? 'Failed to start')
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
    detecting: 'Detecting…', connected: 'Connected',
    disconnected: 'Disconnected', starting: 'Starting…', stopping: 'Stopping…',
  }
  const statusDotColor: Record<OmlxStatus, string> = {
    detecting: '#888', connected: '#4caf50',
    disconnected: '#f44336', starting: '#ff9800', stopping: '#ff9800',
  }

  return (
    <div className="content">
      <div className="page-title">Settings</div>
      <div className="page-sub">Configure AI model service, backend daemon and app preferences</div>

      {/* ── AI Model Service ── */}
      <div className="settings-section">
        <h3>AI Model Service</h3>

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
                {omlxUrl} · {omlxModels.length} model(s)
              </span>
            )}
          </div>
          <div style={{ display: 'flex', gap: 6 }}>
            <button className="btn btn-ghost" onClick={autoDetect} disabled={isBusy}
              style={{ fontSize: 12, padding: '4px 10px' }}>
              Auto-detect
            </button>
            {isConnected ? (
              <button className="btn btn-ghost" onClick={stopServer} disabled={isBusy}
                style={{ fontSize: 12, padding: '4px 10px', color: '#f44336', borderColor: '#f44336' }}>
                Stop Service
              </button>
            ) : (
              <button className="btn btn-primary" onClick={startServer} disabled={isBusy}
                style={{ fontSize: 12, padding: '4px 10px' }}>
                Start oMLX
              </button>
            )}
          </div>
        </div>

        {/* Error message */}
        {detectError && (
          <div style={{
            fontSize: 12, color: '#f44336', padding: '6px 18px', marginBottom: 2,
            background: 'rgba(244,67,54,0.08)', borderRadius: 6,
          }}>
            {detectError}
          </div>
        )}

        {/* URL + Test button + inline result */}
        <div className="setting-row">
          <div>
            <label>Service URL</label>
            <p>OpenAI-compatible API endpoint</p>
          </div>
          <div style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
            <input type="text" value={omlxUrl} onChange={(e) => setOmlxUrl(e.target.value)}
              style={{ width: 210 }} placeholder="http://127.0.0.1:8088" />
            <button className="btn btn-ghost" onClick={testConnection} disabled={isBusy}
              style={{ fontSize: 12, padding: '4px 10px', whiteSpace: 'nowrap' }}>
              {isBusy ? 'Testing…' : 'Test'}
            </button>
            {testResult && (
              <span style={{
                fontSize: 11, fontWeight: 600, whiteSpace: 'nowrap',
                color: testResult.ok ? '#4caf50' : '#f44336',
              }}>
                {testResult.ok ? '✓' : '✗'} {testResult.msg}
              </span>
            )}
          </div>
        </div>

        {/* API Key with eye toggle */}
        <div className="setting-row">
          <div>
            <label>API Key</label>
            <p>Leave empty if no auth required</p>
          </div>
          <div style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
            <input
              type={showApiKey ? 'text' : 'password'}
              value={omlxApiKey}
              onChange={(e) => setOmlxApiKey(e.target.value)}
              style={{ width: 180 }}
              placeholder="optional"
            />
            <button
              onClick={() => setShowApiKey((v) => !v)}
              style={{
                background: 'none', border: 'none', cursor: 'pointer',
                color: 'var(--muted)', fontSize: 15, padding: '2px 4px',
                lineHeight: 1,
              }}
              title={showApiKey ? 'Hide' : 'Show'}
            >
              {showApiKey ? '🙈' : '👁'}
            </button>
          </div>
        </div>

        {/* Launch port — only when disconnected */}
        {!isConnected && (
          <div className="setting-row">
            <div>
              <label>Start Port</label>
              <p>Port used when clicking "Start oMLX"</p>
            </div>
            <input type="text" value={omlxPort} onChange={(e) => setOmlxPort(e.target.value)}
              style={{ width: 80 }} />
          </div>
        )}

        {/* Active model selector */}
        <div className="setting-row">
          <div>
            <label>Active Model</label>
            <p>Default model for chat and agent tasks</p>
          </div>
          {omlxModels.length > 0 ? (
            <select value={omlxActiveModel} onChange={(e) => setOmlxActiveModel(e.target.value)}
              style={{ maxWidth: 300 }}>
              {omlxModels.map((m) => <option key={m} value={m}>{m}</option>)}
            </select>
          ) : (
            <span style={{ fontSize: 12, color: 'var(--muted)' }}>
              {isBusy ? 'Loading…' : 'Connect to list models'}
            </span>
          )}
        </div>
      </div>

      {/* ── Backend Daemon ── */}
      <div className="settings-section">
        <h3>Backend Daemon</h3>
        <div className="setting-row">
          <div>
            <label>Daemon Port</label>
            <p>Agent24 internal service port</p>
          </div>
          <input type="text" defaultValue="8765" style={{ width: 80 }} />
        </div>
      </div>

      {/* ── Appearance ── */}
      <div className="settings-section">
        <h3>Appearance</h3>
        <div className="setting-row">
          <div><label>Language</label><p>UI display language</p></div>
          <select defaultValue="zh">
            <option value="zh">中文</option>
            <option value="en">English</option>
          </select>
        </div>
      </div>

      <div style={{ marginTop: 8 }}>
        <button className="btn btn-primary">Save Settings</button>
        <button className="btn btn-ghost" style={{ marginLeft: 8 }}>Reset Defaults</button>
      </div>
    </div>
  )
}
