import { useState } from 'react'
import { useT } from '../../i18n'
import { helloModuleMessages } from './HelloModule.i18n'

// Reference UI module component.
// Communicates with daemon exclusively via window.agent24.backendProxy().
// This is the pattern all UI modules must follow — no direct IPC, no Node APIs.
export default function HelloModule() {
  const t = useT(helloModuleMessages)
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
        setGreeting(t('error', { msg: (res.data as { error?: string })?.error ?? res.status }))
      }
    } catch (e) {
      setGreeting(t('error', { msg: String(e) }))
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
      <div className="page-sub">{t('sub')}</div>

      <div className="settings-section">
        <h3>{t('tryIt')}</h3>
        <div className="setting-row">
          <div><label>{t('nameLabel')}</label><p>{t('nameHint')}</p></div>
          <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && void greet()}
              placeholder={t('namePlaceholder')}
              style={{ width: 160 }}
            />
            <button className="btn btn-primary" onClick={() => void greet()} disabled={!name.trim() || loading}
              style={{ fontSize: 12, padding: '4px 14px' }}>
              {loading ? t('thinking') : t('greet')}
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
        <h3>{t('info.title')}</h3>
        <div className="setting-row">
          <span style={{ fontSize: 12, color: 'var(--muted)' }}>
            {info ? t('info.value', { id: info.moduleId, description: info.description }) : t('info.prompt')}
          </span>
          <button className="btn btn-ghost" onClick={() => void loadInfo()}
            style={{ fontSize: 12, padding: '4px 10px' }}>
            {t('info.load')}
          </button>
        </div>
        <div style={{ padding: '8px 18px', fontSize: 11, color: 'var(--muted)', lineHeight: 1.7 }}>
          <b>{t('how.title')}</b><br />
          • {t('how.daemon1')}<code>example-hello-ui.ts</code>{t('how.daemon2')}<code>POST /api/modules/hello/greet</code>{t('how.daemon3')}<br />
          • {t('how.renderer1')}<code>window.agent24.backendProxy()</code>{t('how.renderer2')}<br />
          • {t('how.shell1')}<code>manifest.navItem</code>{t('how.shell2')}
        </div>
      </div>
    </div>
  )
}
