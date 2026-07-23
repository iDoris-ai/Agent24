import { useState } from 'react'

// Reference UI module component.
// Communicates with daemon exclusively via window.agent24.backendProxy().
// This is the pattern all UI modules must follow — no direct IPC, no Node APIs.
export default function HelloModule() {
  const [name, setName] = useState('')
  const [greeting, setGreeting] = useState('')
  const [loading, setLoading] = useState(false)
  const [info, setInfo] = useState<{ moduleId: string; description: string } | null>(null)

  async function greet() {
    if (!name.trim() || loading) return
    // Limit name length to prevent oversized prompts
    const safeName = name.trim().slice(0, 80)
    setLoading(true)
    setGreeting('')
    try {
      const res = await window.agent24.backendProxy({
        method: 'POST',
        path: '/api/modules/hello/greet',
        body: { name: safeName },
      })
      if (res.ok) {
        setGreeting((res.data as { greeting: string }).greeting)
      } else {
        setGreeting(`Error: ${(res.data as { error?: string })?.error ?? res.status}`)
      }
    } catch (e) {
      setGreeting(`Error: ${String(e)}`)
    } finally {
      setLoading(false)
    }
  }

  async function loadInfo() {
    const res = await window.agent24.backendProxy({ method: 'GET', path: '/api/modules/hello/info' })
    if (res.ok) setInfo(res.data as { moduleId: string; description: string })
  }

  return (
    <div className="content">
      <div className="page-title">Hello Module</div>
      <div className="page-sub">Reference UI module — demonstrates the CapabilityModule pattern</div>

      <div className="settings-section">
        <h3>Try it</h3>
        <div className="setting-row">
          <div><label>Your name</label><p>LLM will greet you personally</p></div>
          <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && void greet()}
              placeholder="e.g. Jason"
              style={{ width: 160 }}
            />
            <button className="btn btn-primary" onClick={() => void greet()} disabled={!name.trim() || loading}
              style={{ fontSize: 12, padding: '4px 14px' }}>
              {loading ? 'Thinking…' : 'Greet me'}
            </button>
          </div>
        </div>

        {greeting && (
          <div style={{ padding: '12px 18px', margin: '4px 0', background: 'var(--surface2)', borderRadius: 8, fontSize: 14, lineHeight: 1.6 }}>
            {greeting}
          </div>
        )}
      </div>

      <div className="settings-section">
        <h3>Module Info</h3>
        <div className="setting-row">
          <span style={{ fontSize: 12, color: 'var(--muted)' }}>
            {info ? `ID: ${info.moduleId} — ${info.description}` : 'Click to load'}
          </span>
          <button className="btn btn-ghost" onClick={() => void loadInfo()}
            style={{ fontSize: 12, padding: '4px 10px' }}>
            Load info
          </button>
        </div>
        <div style={{ padding: '8px 18px', fontSize: 11, color: 'var(--muted)', lineHeight: 1.7 }}>
          <b>How this module works:</b><br />
          • Daemon side: <code>example-hello-ui.ts</code> registers <code>POST /api/modules/hello/greet</code> using LLMGateway<br />
          • Renderer side: this component calls <code>window.agent24.backendProxy()</code> — no direct IPC<br />
          • Shell injects the navItem from <code>manifest.navItem</code> into the sidebar automatically
        </div>
      </div>
    </div>
  )
}
