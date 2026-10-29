import { useState, useEffect, useRef } from 'react'
import { listOsModules, setOsModuleEnabled, type DomainOsView } from './agent/api'
import { useT } from '../i18n'
import { modulesManagerMessages } from './ModulesManager.i18n'

// REV-01 (ADR-037 decision 1): the node-daemon-era npm module list / install /
// marketplace sections are gone; this page now manages only the kernel-mounted
// domain-OS modules (`GET`/`PATCH /api/v1/os`, e.g. Sin90).
export default function ModulesManagerPage() {
  const t = useT(modulesManagerMessages)
  const [osModules, setOsModules] = useState<DomainOsView[]>([])
  const [osLoading, setOsLoading] = useState(true)
  const [osError, setOsError] = useState<string | null>(null)
  const [osRegistryError, setOsRegistryError] = useState<string | null>(null)
  const [osToggling, setOsToggling] = useState<string | null>(null)

  const loadOsSeq = useRef(0)

  useEffect(() => {
    void loadOs()
  }, [])

  async function loadOs() {
    const seq = ++loadOsSeq.current
    setOsLoading(true)
    setOsError(null)
    try {
      const list = await listOsModules()
      if (seq === loadOsSeq.current) {
        setOsModules(list.modules)
        setOsRegistryError(list.registry_error ?? null)
      }
    } catch {
      if (seq === loadOsSeq.current) {
        setOsModules([])
        setOsError(t('error.loadOs'))
      }
    } finally {
      if (seq === loadOsSeq.current) setOsLoading(false)
    }
  }

  async function toggleOs(mod: DomainOsView) {
    setOsToggling(mod.name)
    setOsError(null)
    try {
      const list = await setOsModuleEnabled(mod.name, !mod.enabled)
      setOsModules(list.modules)
      setOsRegistryError(list.registry_error ?? null)
    } catch (e) {
      setOsError(t('error.toggleOs', { action: t(mod.enabled ? 'action.disable' : 'action.enable'), name: mod.name, msg: e instanceof Error ? e.message : String(e) }))
    } finally {
      setOsToggling(null)
    }
  }

  const OS_STATE_META: Record<string, { label: string; color: string }> = {
    mounted: { label: t('os.state.mounted'), color: '#4caf50' },
    disabled: { label: t('os.state.disabled'), color: '#888' },
    degraded: { label: t('os.state.degraded'), color: '#ff9800' },
    refused: { label: t('os.state.refused'), color: '#f44336' },
  }

  function renderOsModule(mod: DomainOsView) {
    const meta = OS_STATE_META[mod.state] ?? { label: mod.state, color: '#888' }
    return (
      <div key={mod.name} style={{
        background: 'var(--surface2)', borderRadius: 10, padding: '14px 18px',
        border: `1px solid ${mod.enabled ? 'var(--border)' : 'rgba(255,255,255,0.05)'}`,
        opacity: mod.enabled ? 1 : 0.6, transition: 'opacity 0.2s',
      }}>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
          <div>
            <div style={{ fontWeight: 600, fontSize: 14 }}>{mod.name}</div>
            <div style={{ fontSize: 11, color: 'var(--muted)', marginTop: 2 }}>{mod.namespace} · v{mod.version}</div>
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            {mod.restart_required && (
              <span title={t('os.restartTitle')} style={{
                fontSize: 10, fontWeight: 700, padding: '2px 8px', borderRadius: 12,
                background: 'rgba(224,160,32,0.12)', color: '#e0a020', border: '1px solid rgba(224,160,32,0.25)',
              }}>{t('os.restartRequired')}</span>
            )}
            <span style={{
              fontSize: 10, fontWeight: 700, padding: '2px 8px', borderRadius: 12,
              background: `${meta.color}22`, color: meta.color, border: `1px solid ${meta.color}44`,
            }}>{meta.label}</span>
            <button
              onClick={() => void toggleOs(mod)}
              disabled={osToggling === mod.name}
              style={{
                fontSize: 12, fontWeight: 600, padding: '5px 14px', borderRadius: 20, cursor: 'pointer',
                background: mod.enabled ? 'rgba(76,175,80,0.15)' : 'rgba(255,255,255,0.05)',
                color: mod.enabled ? '#4caf50' : 'var(--muted)',
                border: mod.enabled ? '1px solid #4caf5066' : '1px solid var(--border)',
              }}
            >
              {osToggling === mod.name ? '…' : mod.enabled ? t('os.disable') : t('os.enable')}
            </button>
          </div>
        </div>
        {mod.detail && <div style={{ fontSize: 12, color: 'var(--muted)', marginTop: 8 }}>{mod.detail}</div>}
        {mod.granted.length > 0 && (
          <div style={{ marginTop: 6, display: 'flex', gap: 4, flexWrap: 'wrap' }}>
            {mod.granted.map((g) => (
              <span key={g} title={t('os.grantedTitle')} style={{
                fontSize: 10, padding: '1px 7px', borderRadius: 8,
                background: 'rgba(255,255,255,0.06)', color: 'var(--muted)', border: '1px solid var(--border)',
              }}>{g}</span>
            ))}
          </div>
        )}
        {mod.missing_models.length > 0 && (
          <div style={{ fontSize: 11, color: '#f44336', marginTop: 6 }}>
            {t('os.missingModels', { models: mod.missing_models.join(', ') })}
          </div>
        )}
      </div>
    )
  }

  return (
    <div className="content">
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 6 }}>
        <div className="page-title">{t('title')}</div>
        <button className="btn btn-ghost" onClick={() => void loadOs()} style={{ fontSize: 12 }}>{t('refresh')}</button>
      </div>
      <div className="page-sub">{t('sub')}</div>

      {/* ── FU-90: OS 模块（内核挂载的进程外 domain-OS 模块，如 Sin90）── */}
      <div style={{ marginTop: 16, padding: '14px 18px', background: 'var(--surface2)', borderRadius: 10, border: '1px solid var(--border)' }}>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 8 }}>
          <div style={{ fontWeight: 600, fontSize: 13 }}>{t('os.title')}</div>
          <button className="btn btn-ghost" onClick={() => void loadOs()} style={{ fontSize: 11 }}>{t('refresh')}</button>
        </div>
        {osError && (
          <div style={{ color: '#f44336', fontSize: 12, padding: '8px 12px', marginBottom: 8, background: 'rgba(244,67,54,0.1)', borderRadius: 6, border: '1px solid rgba(244,67,54,0.3)' }}>
            {osError}
          </div>
        )}
        {osRegistryError && (
          <div style={{ color: '#ff9800', fontSize: 12, padding: '8px 12px', marginBottom: 8, background: 'rgba(255,152,0,0.1)', borderRadius: 6, border: '1px solid rgba(255,152,0,0.3)' }}>
            {osRegistryError}
          </div>
        )}
        {osLoading ? (
          <div style={{ color: 'var(--muted)', fontSize: 12, padding: '12px 0', textAlign: 'center' }}>{t('loading')}</div>
        ) : osModules.length === 0 ? (
          <div style={{ color: 'var(--muted)', fontSize: 12, padding: '12px 0' }}>{t('os.empty')}</div>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
            {osModules.map((m) => renderOsModule(m))}
          </div>
        )}
      </div>
    </div>
  )
}
