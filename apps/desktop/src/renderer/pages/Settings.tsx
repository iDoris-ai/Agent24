import { useEffect, useRef, useState } from 'react'

type OmlxStatus = 'detecting' | 'connected' | 'disconnected' | 'starting' | 'stopping'
type ModelCategory = 'resident' | 'ondemand'
type TestResult = { ok: boolean; msg: string } | null

const CATEGORIES_KEY = 'agent24:model-categories'
const AUTO_START_PORT = 8088
const AUTO_START_KEY = 'xiaobao8088'

function loadCategories(): Record<string, ModelCategory> {
  try { return JSON.parse(localStorage.getItem(CATEGORIES_KEY) ?? '{}') } catch { return {} }
}
function saveCategories(c: Record<string, ModelCategory>) {
  localStorage.setItem(CATEGORIES_KEY, JSON.stringify(c))
}

export default function SettingsPage() {
  const [omlxStatus, setOmlxStatus] = useState<OmlxStatus>('detecting')
  const [omlxUrl, setOmlxUrl] = useState(`http://127.0.0.1:${AUTO_START_PORT}`)
  const [omlxApiKey, setOmlxApiKey] = useState(AUTO_START_KEY)
  const [showApiKey, setShowApiKey] = useState(false)
  const [omlxModels, setOmlxModels] = useState<string[]>([])
  const [omlxActiveModel, setOmlxActiveModel] = useState('')
  const [omlxPort, setOmlxPort] = useState(String(AUTO_START_PORT))
  const [categories, setCategories] = useState<Record<string, ModelCategory>>(loadCategories)
  const [warmingUp, setWarmingUp] = useState<Record<string, boolean>>({})
  const [detectError, setDetectError] = useState('')
  const [testResult, setTestResult] = useState<TestResult>(null)
  const testTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const initDone = useRef(false)

  useEffect(() => {
    if (initDone.current) return
    initDone.current = true
    initialize()
  }, [])

  async function initialize() {
    setOmlxStatus('detecting')
    setDetectError('')
    let detected = await window.agent24.omlxDetect()
    if (!detected) {
      // Auto-start oMLX
      setOmlxStatus('starting')
      const started = await window.agent24.omlxStart(AUTO_START_PORT, AUTO_START_KEY)
      if (started.ok) {
        setOmlxUrl(started.url)
        // Poll up to 5 attempts
        for (let i = 0; i < 5; i++) {
          await new Promise((r) => setTimeout(r, 2000))
          const r = await window.agent24.omlxModels(started.url, AUTO_START_KEY)
          if (r.ok) { detected = { url: started.url, apiKey: AUTO_START_KEY, models: r.models }; break }
        }
      }
    }
    if (detected) {
      setOmlxUrl(detected.url)
      setOmlxApiKey(detected.apiKey)
      applyDetected(detected.url, detected.apiKey, detected.models)
    } else {
      setOmlxStatus('disconnected')
      setDetectError('Could not start or connect to AI model service. Fill in the URL below.')
    }
  }

  function applyDetected(url: string, key: string, models: string[]) {
    setOmlxModels(models)
    setOmlxActiveModel((m) => m || models[0] || '')
    setOmlxStatus('connected')
    // Warmup resident models
    const cats = loadCategories()
    const residents = models.filter((m) => cats[m] === 'resident')
    if (residents.length > 0) warmupModels(url, key, residents)
  }

  async function warmupModels(url: string, key: string, models: string[]) {
    const busy: Record<string, boolean> = {}
    models.forEach((m) => { busy[m] = true })
    setWarmingUp((w) => ({ ...w, ...busy }))
    await Promise.all(models.map((m) => window.agent24.omlxWarmup(url, key, m)))
    setWarmingUp((w) => {
      const next = { ...w }
      models.forEach((m) => { delete next[m] })
      return next
    })
  }

  function showTestResult(r: TestResult) {
    setTestResult(r)
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
      applyDetected(result.url, result.apiKey, result.models)
    } else {
      setOmlxStatus('disconnected')
      setDetectError('No AI model service detected on ports 8088 / 8000 / 8001')
    }
  }

  async function testConnection() {
    setTestResult(null)
    setDetectError('')
    setOmlxStatus('detecting')
    const result = await window.agent24.omlxModels(omlxUrl, omlxApiKey)
    if (result.ok) {
      applyDetected(omlxUrl, omlxApiKey, result.models)
      showTestResult({ ok: true, msg: `Connected — ${result.models.length} model(s)` })
    } else {
      setOmlxStatus('disconnected')
      showTestResult({ ok: false, msg: result.error ?? 'Connection failed' })
    }
  }

  async function startServer() {
    setOmlxStatus('starting')
    setDetectError('')
    const port = parseInt(omlxPort, 10) || AUTO_START_PORT
    const result = await window.agent24.omlxStart(port, omlxApiKey)
    if (result.ok) {
      setOmlxUrl(result.url)
      let attempts = 0
      const poll = async () => {
        const r = await window.agent24.omlxModels(result.url, omlxApiKey)
        if (r.ok) { applyDetected(result.url, omlxApiKey, r.models) }
        else if (++attempts < 5) setTimeout(poll, 2000)
        else { setOmlxStatus('disconnected'); setDetectError('Service started but not responding — try Auto-detect') }
      }
      setTimeout(poll, 2000)
    } else {
      setOmlxStatus('disconnected')
      setDetectError(result.error ?? 'Failed to start service')
    }
  }

  async function stopServer() {
    setOmlxStatus('stopping')
    await window.agent24.omlxStop()
    setOmlxModels([])
    setOmlxActiveModel('')
    setWarmingUp({})
    setOmlxStatus('disconnected')
  }

  function setCategory(model: string, cat: ModelCategory) {
    const next = { ...categories, [model]: cat }
    setCategories(next)
    saveCategories(next)
    // If switching to resident while service is running, warmup immediately
    if (cat === 'resident' && omlxStatus === 'connected') {
      warmupModels(omlxUrl, omlxApiKey, [model])
    }
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
              style={{ fontSize: 12, padding: '4px 10px' }}>Auto-detect</button>
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

        {detectError && (
          <div style={{ fontSize: 12, color: '#f44336', padding: '6px 18px', marginBottom: 2, background: 'rgba(244,67,54,0.08)', borderRadius: 6 }}>
            {detectError}
          </div>
        )}

        {/* URL + Test */}
        <div className="setting-row">
          <div><label>Service URL</label><p>OpenAI-compatible API endpoint</p></div>
          <div style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
            <input type="text" value={omlxUrl} onChange={(e) => setOmlxUrl(e.target.value)}
              style={{ width: 210 }} placeholder="http://127.0.0.1:8088" />
            <button className="btn btn-ghost" onClick={testConnection} disabled={isBusy}
              style={{ fontSize: 12, padding: '4px 10px', whiteSpace: 'nowrap' }}>
              {isBusy ? 'Testing…' : 'Test'}
            </button>
            {testResult && (
              <span style={{ fontSize: 11, fontWeight: 600, whiteSpace: 'nowrap', color: testResult.ok ? '#4caf50' : '#f44336' }}>
                {testResult.ok ? '✓' : '✗'} {testResult.msg}
              </span>
            )}
          </div>
        </div>

        {/* API Key + eye */}
        <div className="setting-row">
          <div><label>API Key</label><p>Leave empty if no auth required</p></div>
          <div style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
            <input type={showApiKey ? 'text' : 'password'} value={omlxApiKey}
              onChange={(e) => setOmlxApiKey(e.target.value)} style={{ width: 180 }} placeholder="optional" />
            <button onClick={() => setShowApiKey((v) => !v)}
              style={{ background: 'none', border: 'none', cursor: 'pointer', color: 'var(--muted)', fontSize: 15, padding: '2px 4px', lineHeight: 1 }}
              title={showApiKey ? 'Hide' : 'Show'}>
              {showApiKey ? '🙈' : '👁'}
            </button>
          </div>
        </div>

        {/* Start port (disconnected only) */}
        {!isConnected && (
          <div className="setting-row">
            <div><label>Start Port</label><p>Port when clicking "Start oMLX"</p></div>
            <input type="text" value={omlxPort} onChange={(e) => setOmlxPort(e.target.value)} style={{ width: 80 }} />
          </div>
        )}

        {/* ── Model list with categories ── */}
        {omlxModels.length > 0 && (
          <div style={{ marginTop: 4 }}>
            <div style={{ fontSize: 11, fontWeight: 600, color: 'var(--muted)', padding: '8px 18px 4px', letterSpacing: '0.6px', textTransform: 'uppercase' }}>
              Models — category persists across restarts
            </div>
            {omlxModels.map((model) => {
              const cat: ModelCategory = categories[model] ?? 'ondemand'
              const isWarmingUp = warmingUp[model]
              const isActive = omlxActiveModel === model
              return (
                <div key={model} className="setting-row" style={{ marginBottom: 2, alignItems: 'center' }}
                  onClick={() => setOmlxActiveModel(model)}
                  role="button"
                  tabIndex={0}
                  onKeyDown={(e) => e.key === 'Enter' && setOmlxActiveModel(model)}
                >
                  <div style={{ display: 'flex', alignItems: 'center', gap: 8, flex: 1, cursor: 'pointer' }}>
                    <span style={{
                      width: 6, height: 6, borderRadius: '50%', flexShrink: 0,
                      background: isActive ? 'var(--accent)' : 'var(--border)',
                    }} />
                    <span style={{ fontSize: 13, fontWeight: isActive ? 600 : 400, color: isActive ? 'var(--accent)' : 'var(--text)' }}>
                      {model}
                    </span>
                    {isWarmingUp && (
                      <span style={{ fontSize: 10, color: '#ff9800' }}>loading…</span>
                    )}
                  </div>
                  <div style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
                    <button
                      onClick={(e) => { e.stopPropagation(); setCategory(model, 'resident') }}
                      style={{
                        fontSize: 11, padding: '3px 10px', borderRadius: 20, cursor: 'pointer', fontWeight: 600,
                        background: cat === 'resident' ? 'rgba(76,175,80,0.15)' : 'transparent',
                        color: cat === 'resident' ? '#4caf50' : 'var(--muted)',
                        border: cat === 'resident' ? '1px solid #4caf5066' : '1px solid var(--border)',
                      }}
                    >
                      Resident
                    </button>
                    <button
                      onClick={(e) => { e.stopPropagation(); setCategory(model, 'ondemand') }}
                      style={{
                        fontSize: 11, padding: '3px 10px', borderRadius: 20, cursor: 'pointer', fontWeight: 600,
                        background: cat === 'ondemand' ? 'rgba(33,150,243,0.15)' : 'transparent',
                        color: cat === 'ondemand' ? '#2196f3' : 'var(--muted)',
                        border: cat === 'ondemand' ? '1px solid #2196f366' : '1px solid var(--border)',
                      }}
                    >
                      On-demand
                    </button>
                  </div>
                </div>
              )
            })}
            <div style={{ fontSize: 11, color: 'var(--muted)', padding: '4px 18px 0' }}>
              <b>Resident</b> — auto-loaded when service starts, stays in memory.&nbsp;
              <b>On-demand</b> — loaded on first use, evicted by oMLX when idle.
            </div>
          </div>
        )}

        {!isConnected && omlxModels.length === 0 && (
          <div className="setting-row">
            <span style={{ fontSize: 12, color: 'var(--muted)' }}>Connect to list and configure models</span>
          </div>
        )}
      </div>

      {/* ── Backend Daemon ── */}
      <div className="settings-section">
        <h3>Backend Daemon</h3>
        <div className="setting-row">
          <div><label>Daemon Port</label><p>Agent24 internal service port</p></div>
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
