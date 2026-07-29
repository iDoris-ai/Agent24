// @vitest-environment jsdom (for future tests)
import { useState, useEffect, useRef } from 'react'
import type { DiscoveredModule, ModuleInfo, TrustTier } from '../../shared/ipc-types'

// M4 marketplace: trust-tier chips (roadmap 官方 / 社区 / 第三方).
const TIER_META: Record<TrustTier, { label: string; color: string }> = {
  official: { label: '官方', color: '#4caf50' },
  community: { label: '社区', color: '#2196f3' },
  'third-party': { label: '第三方', color: '#ff9800' },
}
type TierFilter = 'all' | TrustTier
type InstalledFilter = 'all' | 'installed' | 'available'

export default function ModulesManagerPage() {
  const [modules, setModules] = useState<ModuleInfo[]>([])
  const [loading, setLoading] = useState(true)
  const [toggling, setToggling] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  // M3: npm install state
  const [installPkg, setInstallPkg] = useState('')
  const [installing, setInstalling] = useState(false)
  const [installMsg, setInstallMsg] = useState<{ ok: boolean; text: string } | null>(null)
  const [uninstalling, setUninstalling] = useState<string | null>(null)
  // M4: marketplace browse state
  const [discovered, setDiscovered] = useState<DiscoveredModule[]>([])
  const [browseLoading, setBrowseLoading] = useState(true)
  const [browseError, setBrowseError] = useState<string | null>(null)
  const [browseQuery, setBrowseQuery] = useState('')
  const [tierFilter, setTierFilter] = useState<TierFilter>('all')
  const [installedFilter, setInstalledFilter] = useState<InstalledFilter>('all')
  const [marketInstalling, setMarketInstalling] = useState<string | null>(null)

  const loadSeq = useRef(0)
  const browseSeq = useRef(0)

  useEffect(() => {
    void load()
  }, [])

  // Re-query the marketplace whenever a filter changes (the daemon applies the
  // filters server-side; #94). Debounced so typing in the search box doesn't
  // fire a request per keystroke.
  useEffect(() => {
    const t = setTimeout(() => void browse(), 250)
    return () => clearTimeout(t)
  }, [browseQuery, tierFilter, installedFilter])

  async function load() {
    const seq = ++loadSeq.current
    setLoading(true)
    setError(null)
    try {
      const mods = await window.agent24.modulesList()
      if (seq === loadSeq.current) setModules(mods)
    } catch {
      if (seq === loadSeq.current) {
        setModules([])
        setError('加载模块列表失败，请重试')
      }
    } finally {
      if (seq === loadSeq.current) setLoading(false)
    }
  }

  async function toggle(mod: ModuleInfo) {
    setToggling(mod.id)
    setError(null)
    try {
      const result = mod.enabled
        ? await window.agent24.modulesDisable(mod.id)
        : await window.agent24.modulesEnable(mod.id)
      if (result.ok) {
        await load()
      } else {
        setError(`${mod.enabled ? '停用' : '启用'} ${mod.name} 失败`)
      }
    } catch {
      setError(`切换 ${mod.name} 时网络错误`)
    } finally {
      setToggling(null)
    }
  }

  async function install() {
    const pkg = installPkg.trim()
    if (!pkg) return
    setInstalling(true)
    setInstallMsg(null)
    try {
      const result = await window.agent24.modulesInstall(pkg)
      if (result.ok) {
        setInstallMsg({ ok: true, text: `✅ 安装成功：${result.id ?? pkg}，已注册到 daemon` })
        setInstallPkg('')
        await load()
      } else {
        setInstallMsg({ ok: false, text: `❌ 安装失败：${result.error ?? '未知错误'}` })
      }
    } catch {
      setInstallMsg({ ok: false, text: '❌ 安装时网络错误' })
    } finally {
      setInstalling(false)
    }
  }

  async function uninstall(mod: ModuleInfo, packageName: string) {
    setUninstalling(mod.id)
    setError(null)
    try {
      const result = await window.agent24.modulesUninstall(packageName, mod.id)
      if (result.ok) {
        await load()
      } else {
        setError(`卸载 ${mod.name} 失败：${result.error ?? ''}`)
      }
    } catch {
      setError(`卸载 ${mod.name} 时网络错误`)
    } finally {
      setUninstalling(null)
    }
  }

  async function browse() {
    const seq = ++browseSeq.current
    setBrowseLoading(true)
    setBrowseError(null)
    try {
      const results = await window.agent24.modulesDiscover({
        query: browseQuery.trim() || undefined,
        trustTier: tierFilter === 'all' ? undefined : tierFilter,
        installed: installedFilter === 'all' ? undefined : installedFilter === 'installed',
      })
      if (seq === browseSeq.current) setDiscovered(results)
    } catch {
      if (seq === browseSeq.current) {
        setDiscovered([])
        setBrowseError('加载模块市场失败，请重试')
      }
    } finally {
      if (seq === browseSeq.current) setBrowseLoading(false)
    }
  }

  async function installFromMarket(pkg: DiscoveredModule) {
    setMarketInstalling(pkg.packageName)
    setInstallMsg(null)
    try {
      const result = await window.agent24.modulesInstall(pkg.packageName)
      if (result.ok) {
        setInstallMsg({ ok: true, text: `✅ 安装成功：${result.id ?? pkg.packageName}，已注册到 daemon（默认停用，待你确认权限后启用）` })
        await Promise.all([load(), browse()]) // refresh installed list + market "installed" flags
      } else {
        setInstallMsg({ ok: false, text: `❌ 安装失败：${result.error ?? '未知错误'}` })
      }
    } catch {
      setInstallMsg({ ok: false, text: '❌ 安装时网络错误' })
    } finally {
      setMarketInstalling(null)
    }
  }

  function renderTierChip(tier: TrustTier) {
    const { label, color } = TIER_META[tier]
    return (
      <span
        title={`信任级：${label}`}
        style={{
          fontSize: 10, fontWeight: 700, padding: '2px 8px', borderRadius: 12, whiteSpace: 'nowrap',
          background: `${color}22`, color, border: `1px solid ${color}44`,
        }}
      >{label}</span>
    )
  }

  const TYPE_LABELS: Record<string, string> = { ui: 'UI', headless: '后台', hybrid: '混合' }
  const TYPE_COLORS: Record<string, string> = { ui: '#4caf50', headless: '#2196f3', hybrid: '#ff9800' }

  // Heuristic: bundled modules don't have a packageName convention — community ones do
  const BUILTIN_IDS = new Set(['ping', '@auraaihq/example-summarize', '@auraaihq/example-hello'])
  const builtinMods = modules.filter((m) => BUILTIN_IDS.has(m.id))
  const communityMods = modules.filter((m) => !BUILTIN_IDS.has(m.id))

  function renderModule(mod: ModuleInfo, isCommunity = false) {
    return (
      <div key={mod.id} style={{
        background: 'var(--surface2)', borderRadius: 10, padding: '14px 18px',
        border: `1px solid ${mod.enabled ? 'var(--border)' : 'rgba(255,255,255,0.05)'}`,
        opacity: mod.enabled ? 1 : 0.6, transition: 'opacity 0.2s',
      }}>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
            {mod.navItem?.icon && <span style={{ fontSize: 20 }}>{mod.navItem.icon}</span>}
            <div>
              <div style={{ fontWeight: 600, fontSize: 14 }}>{mod.name}</div>
              <div style={{ fontSize: 11, color: 'var(--muted)', marginTop: 2 }}>{mod.id} · v{mod.version}</div>
              {/* H10: show what the module declares it needs, so enabling a
                  pending-consent module is an informed decision. */}
              {mod.permissions?.length > 0 && (
                <div style={{ display: 'flex', gap: 4, marginTop: 4, flexWrap: 'wrap' }}>
                  {mod.permissions.map((p) => (
                    <span
                      key={p}
                      title="此模块声明需要的权限"
                      style={{
                        fontSize: 9, padding: '1px 6px', borderRadius: 8,
                        background: 'rgba(224,160,32,0.12)', color: '#e0a020',
                        border: '1px solid rgba(224,160,32,0.25)',
                      }}
                    >{p}</span>
                  ))}
                </div>
              )}
            </div>
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <span style={{
              fontSize: 10, fontWeight: 700, padding: '2px 8px', borderRadius: 12,
              background: `${TYPE_COLORS[mod.type] ?? '#888'}22`,
              color: TYPE_COLORS[mod.type] ?? '#888',
              border: `1px solid ${TYPE_COLORS[mod.type] ?? '#888'}44`,
            }}>{TYPE_LABELS[mod.type] ?? mod.type}</span>
            <button
              onClick={() => void toggle(mod)}
              disabled={toggling === mod.id || uninstalling === mod.id}
              style={{
                fontSize: 12, fontWeight: 600, padding: '5px 14px', borderRadius: 20, cursor: 'pointer',
                background: mod.enabled ? 'rgba(76,175,80,0.15)' : 'rgba(255,255,255,0.05)',
                color: mod.enabled ? '#4caf50' : 'var(--muted)',
                border: mod.enabled ? '1px solid #4caf5066' : '1px solid var(--border)',
                transition: 'all 0.15s',
              }}
            >
              {toggling === mod.id ? '…' : mod.enabled ? '启用中' : '已停用'}
            </button>
            {isCommunity && (
              <button
                onClick={() => void uninstall(mod, mod.id)}
                disabled={uninstalling === mod.id || toggling === mod.id}
                title="卸载此模块"
                style={{
                  fontSize: 11, padding: '5px 10px', borderRadius: 20, cursor: 'pointer',
                  background: 'rgba(244,67,54,0.1)', color: '#f44336',
                  border: '1px solid rgba(244,67,54,0.3)', transition: 'all 0.15s',
                }}
              >
                {uninstalling === mod.id ? '…' : '卸载'}
              </button>
            )}
          </div>
        </div>
        <div style={{ fontSize: 12, color: 'var(--muted)', marginTop: 8 }}>{mod.description}</div>
        {mod.permissions.length > 0 && (
          <div style={{ marginTop: 6, display: 'flex', gap: 4, flexWrap: 'wrap' }}>
            {mod.permissions.map((p) => (
              <span key={p} style={{
                fontSize: 10, padding: '1px 7px', borderRadius: 8,
                background: 'rgba(255,255,255,0.06)', color: 'var(--muted)',
                border: '1px solid var(--border)',
              }}>{p}</span>
            ))}
          </div>
        )}
      </div>
    )
  }

  return (
    <div className="content">
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 6 }}>
        <div className="page-title">模块管理</div>
        <button className="btn btn-ghost" onClick={() => void load()} style={{ fontSize: 12 }}>↻ 刷新</button>
      </div>
      <div className="page-sub">安装的能力模块 — 切换启停立即生效，无需重启</div>

      {error && (
        <div style={{ color: '#f44336', fontSize: 12, padding: '8px 12px', marginTop: 8, background: 'rgba(244,67,54,0.1)', borderRadius: 6, border: '1px solid rgba(244,67,54,0.3)' }}>
          {error}
        </div>
      )}

      {/* ── M3: npm install panel ── */}
      <div style={{ marginTop: 16, padding: '14px 18px', background: 'var(--surface2)', borderRadius: 10, border: '1px solid var(--border)' }}>
        <div style={{ fontWeight: 600, fontSize: 13, marginBottom: 8 }}>从 npm 安装社区模块</div>
        <div style={{ display: 'flex', gap: 8 }}>
          <input
            type="text"
            value={installPkg}
            onChange={(e) => setInstallPkg(e.target.value)}
            onKeyDown={(e) => { if (e.key === 'Enter') void install() }}
            placeholder="@auraaihq/example-hello"
            disabled={installing}
            style={{
              flex: 1, padding: '6px 12px', borderRadius: 8, fontSize: 13,
              background: 'var(--surface)', border: '1px solid var(--border)',
              color: 'var(--text)', outline: 'none',
            }}
          />
          <button
            onClick={() => void install()}
            disabled={installing || !installPkg.trim()}
            style={{
              padding: '6px 18px', borderRadius: 8, fontSize: 13, fontWeight: 600,
              background: installing ? 'rgba(255,255,255,0.05)' : 'rgba(33,150,243,0.2)',
              color: '#2196f3', border: '1px solid rgba(33,150,243,0.4)', cursor: 'pointer',
            }}
          >
            {installing ? '安装中…' : '安装'}
          </button>
        </div>
        {installMsg && (
          <div style={{
            marginTop: 8, fontSize: 12, padding: '6px 10px', borderRadius: 6,
            background: installMsg.ok ? 'rgba(76,175,80,0.1)' : 'rgba(244,67,54,0.1)',
            color: installMsg.ok ? '#4caf50' : '#f44336',
            border: `1px solid ${installMsg.ok ? 'rgba(76,175,80,0.3)' : 'rgba(244,67,54,0.3)'}`,
          }}>
            {installMsg.text}
          </div>
        )}
      </div>

      {/* ── M4: 模块市场（浏览 + 搜索 + 过滤 + 一键装）── */}
      <div style={{ marginTop: 16, padding: '14px 18px', background: 'var(--surface2)', borderRadius: 10, border: '1px solid var(--border)' }}>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 8 }}>
          <div style={{ fontWeight: 600, fontSize: 13 }}>模块市场</div>
          <button className="btn btn-ghost" onClick={() => void browse()} style={{ fontSize: 11 }}>↻ 刷新</button>
        </div>

        {/* 搜索框 */}
        <input
          type="text"
          value={browseQuery}
          onChange={(e) => setBrowseQuery(e.target.value)}
          placeholder="搜索模块名 / 描述…"
          aria-label="搜索模块市场"
          style={{
            width: '100%', boxSizing: 'border-box', padding: '6px 12px', borderRadius: 8, fontSize: 13,
            background: 'var(--surface)', border: '1px solid var(--border)', color: 'var(--text)', outline: 'none',
          }}
        />

        {/* 过滤：信任级 chip + 已装/未装 */}
        <div style={{ display: 'flex', gap: 6, marginTop: 10, flexWrap: 'wrap', alignItems: 'center' }}>
          <FilterChip active={tierFilter === 'all'} onClick={() => setTierFilter('all')} label="全部" />
          {(Object.keys(TIER_META) as TrustTier[]).map((t) => (
            <FilterChip
              key={t}
              active={tierFilter === t}
              onClick={() => setTierFilter(t)}
              label={TIER_META[t].label}
              color={TIER_META[t].color}
            />
          ))}
          <span style={{ width: 1, height: 16, background: 'var(--border)', margin: '0 4px' }} />
          {(['all', 'available', 'installed'] as InstalledFilter[]).map((f) => (
            <FilterChip
              key={f}
              active={installedFilter === f}
              onClick={() => setInstalledFilter(f)}
              label={f === 'all' ? '全部' : f === 'available' ? '未装' : '已装'}
            />
          ))}
        </div>

        {/* 结果 */}
        {browseError && (
          <div style={{ color: '#f44336', fontSize: 12, padding: '8px 12px', marginTop: 10, background: 'rgba(244,67,54,0.1)', borderRadius: 6, border: '1px solid rgba(244,67,54,0.3)' }}>
            {browseError}
          </div>
        )}
        {browseLoading ? (
          <div style={{ color: 'var(--muted)', fontSize: 12, padding: '16px 0', textAlign: 'center' }}>搜索中…</div>
        ) : discovered.length === 0 ? (
          <div style={{ color: 'var(--muted)', fontSize: 12, padding: '16px 0', textAlign: 'center' }}>没有匹配的模块</div>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8, marginTop: 10 }}>
            {discovered.map((m) => (
              <div key={m.packageName} style={{
                background: 'var(--surface)', borderRadius: 10, padding: '12px 16px', border: '1px solid var(--border)',
              }}>
                <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 10 }}>
                  <div style={{ minWidth: 0 }}>
                    <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                      <span style={{ fontWeight: 600, fontSize: 14, overflow: 'hidden', textOverflow: 'ellipsis' }}>{m.name}</span>
                      {renderTierChip(m.trustTier)}
                    </div>
                    <div style={{ fontSize: 11, color: 'var(--muted)', marginTop: 2 }}>{m.packageName}{m.version ? ` · v${m.version}` : ''}</div>
                  </div>
                  {m.installed ? (
                    <span style={{
                      fontSize: 11, fontWeight: 600, padding: '5px 12px', borderRadius: 20, whiteSpace: 'nowrap',
                      background: 'rgba(76,175,80,0.12)', color: '#4caf50', border: '1px solid rgba(76,175,80,0.3)',
                    }}>已安装</span>
                  ) : (
                    <button
                      onClick={() => void installFromMarket(m)}
                      disabled={marketInstalling === m.packageName}
                      aria-label={`安装 ${m.name}`}
                      style={{
                        fontSize: 12, fontWeight: 600, padding: '5px 16px', borderRadius: 20, cursor: 'pointer', whiteSpace: 'nowrap',
                        background: 'rgba(33,150,243,0.2)', color: '#2196f3', border: '1px solid rgba(33,150,243,0.4)',
                      }}
                    >
                      {marketInstalling === m.packageName ? '安装中…' : '安装'}
                    </button>
                  )}
                </div>
                {m.description && <div style={{ fontSize: 12, color: 'var(--muted)', marginTop: 8 }}>{m.description}</div>}
              </div>
            ))}
          </div>
        )}
      </div>

      {loading ? (
        <div style={{ color: 'var(--muted)', fontSize: 13, padding: '24px 0', textAlign: 'center' }}>加载中…</div>
      ) : (
        <>
          {/* Builtin modules */}
          {builtinMods.length > 0 && (
            <div style={{ marginTop: 16 }}>
              <div style={{ fontSize: 11, fontWeight: 600, color: 'var(--muted)', marginBottom: 8, textTransform: 'uppercase', letterSpacing: '0.05em' }}>内置模块</div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
                {builtinMods.map((m) => renderModule(m, false))}
              </div>
            </div>
          )}

          {/* Community modules */}
          <div style={{ marginTop: 16 }}>
            <div style={{ fontSize: 11, fontWeight: 600, color: 'var(--muted)', marginBottom: 8, textTransform: 'uppercase', letterSpacing: '0.05em' }}>社区模块</div>
            {communityMods.length === 0 ? (
              <div style={{ color: 'var(--muted)', fontSize: 12, padding: '12px 0' }}>暂无已安装的社区模块</div>
            ) : (
              <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
                {communityMods.map((m) => renderModule(m, true))}
              </div>
            )}
          </div>
        </>
      )}
    </div>
  )
}

// A pill toggle for marketplace filters. When `color` is given (a trust tier),
// the active state paints in that tier's color so the filter reads as the chip
// it selects.
function FilterChip({ active, onClick, label, color }: {
  active: boolean
  onClick: () => void
  label: string
  color?: string
}) {
  const accent = color ?? '#2196f3'
  return (
    <button
      onClick={onClick}
      aria-pressed={active}
      style={{
        fontSize: 11, fontWeight: 600, padding: '3px 11px', borderRadius: 14, cursor: 'pointer',
        background: active ? `${accent}22` : 'transparent',
        color: active ? accent : 'var(--muted)',
        border: `1px solid ${active ? `${accent}66` : 'var(--border)'}`,
        transition: 'all 0.15s',
      }}
    >{label}</button>
  )
}
