import { useEffect, useState } from 'react'
import type { CliInstallResult, CliToolStatus } from '../../../shared/ipc-types'
import { cliToolMessages } from '../../../shared/cli-tool.i18n'
import { useT } from '../../i18n'

export function CliToolSettings() {
  const t = useT(cliToolMessages)
  const [status, setStatus] = useState<CliToolStatus | null>(null)
  const [result, setResult] = useState<CliInstallResult | null>(null)
  const [busy, setBusy] = useState(false)
  async function refresh() {
    try { setStatus(await window.agent24.cliToolStatus()) }
    catch { setStatus({ state: 'unavailable', path: null, version: null, bundledVersion: null, paths: [] }) }
  }
  useEffect(() => {
    void refresh()
    const timer = setInterval(() => { void refresh() }, 30_000)
    window.addEventListener('focus', refresh)
    return () => { clearInterval(timer); window.removeEventListener('focus', refresh) }
  }, [])
  async function install(userOnly = false) {
    setBusy(true)
    setResult(null)
    try { setResult(await (userOnly ? window.agent24.repairUserCli() : window.agent24.installCliTool())) }
    catch { setResult({ state: 'failed' }) }
    finally { await refresh(); setBusy(false) }
  }
  return <section className="settings-section" aria-labelledby="cli-tool-title">
    <h3 id="cli-tool-title">{t('title')}</h3>
    <div className="setting-row" style={{ flexWrap: 'wrap', gap: 12 }}>
      <div style={{ minWidth: 0, overflowWrap: 'anywhere' }}>
        <p>{t('bundled')}: {status?.bundledVersion ?? '—'}</p>
        {status && !status.paths.some((item) => item.path === status.path) && <div>
          <p>{t('path')}: <code>{status.path ?? t('absent')}</code> · {status.version ?? t('unknownVersion')}</p>
          <p role="status">{t(status.state, { version: status.version ?? '?' })}</p>
        </div>}
        {status?.paths.map((item) => <div key={item.path}>
          <p><code>{item.path}</code> · {item.version ?? t('unknownVersion')}</p>
          <p><span>{t(`link.${item.link}`)}</span> · <span>{t(item.state, { version: item.version ?? t('unknownVersion') })}</span></p>
          {item.path.endsWith('/.agent24/bin/agent24') && item.state === 'outdated' && <div role="alert">
            <p>{t('userWarning', { version: item.version ?? t('unknownVersion') })}</p>
            <button className="btn btn-primary" disabled={busy} onClick={() => { void install(true) }}>{t('repairUser')}</button>
          </div>}
        </div>)}
      </div>
      <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8 }}>
        <button className="btn btn-ghost" disabled={busy} onClick={() => { void refresh() }}>{t('refresh')}</button>
        <button className="btn btn-primary" disabled={busy || !status || status.state === 'unavailable'} onClick={() => { void install() }}>
          {t(busy ? 'busy' : 'install')}
        </button>
      </div>
    </div>
    {result && <div role="status" style={{ padding: '8px 18px', overflowWrap: 'anywhere' }}>
      {t(result.state)}
      {result.state === 'installed' && result.backup && <p>{t('backup', { path: result.backup })}</p>}
    </div>}
  </section>
}
