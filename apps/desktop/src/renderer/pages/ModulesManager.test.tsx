// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import ModulesManagerPage from './ModulesManager'
import type { DiscoveredModule, DiscoverFilter } from '../../shared/ipc-types'
import type { DomainOsList, DomainOsView } from './agent/api'

const CATALOG: DiscoveredModule[] = [
  { packageName: '@auraaihq/wechat', version: '1.0.0', name: '@auraaihq/wechat', description: 'WeChat 桥', trustTier: 'official', installed: false },
  { packageName: '@someorg/weather', version: '2.1.0', name: '@someorg/weather', description: '天气 feed', trustTier: 'community', installed: true },
]

let discoverCalls: DiscoverFilter[] = []

const EMPTY_OS_LIST: DomainOsList = { modules: [] }

function mount(opts: {
  discover?: (f: DiscoverFilter) => DiscoveredModule[]
  install?: () => { ok: boolean; id?: string; error?: string }
  osList?: DomainOsList
  onOsPatch?: (name: string, enabled: boolean) => DomainOsList
} = {}) {
  discoverCalls = []
  const install = vi.fn(() => Promise.resolve(opts.install ? opts.install() : { ok: true, id: 'x' }))
  const backendProxy = vi.fn((req: { method: string; path: string; body?: unknown }) => {
    if (req.method === 'GET' && req.path === '/api/v1/os') {
      return Promise.resolve({ ok: true, status: 200, data: opts.osList ?? EMPTY_OS_LIST })
    }
    if (req.method === 'PATCH' && req.path.startsWith('/api/v1/os/')) {
      const name = decodeURIComponent(req.path.slice('/api/v1/os/'.length))
      const enabled = (req.body as { enabled: boolean }).enabled
      const data = opts.onOsPatch ? opts.onOsPatch(name, enabled) : EMPTY_OS_LIST
      return Promise.resolve({ ok: true, status: 200, data })
    }
    return Promise.resolve({ ok: true, status: 200, data: {} })
  })
  window.agent24 = {
    modulesList: vi.fn(() => Promise.resolve([])),
    modulesDiscover: vi.fn((f: DiscoverFilter) => {
      discoverCalls.push(f)
      return Promise.resolve(opts.discover ? opts.discover(f) : CATALOG)
    }),
    modulesInstall: install,
    modulesEnable: vi.fn(),
    modulesDisable: vi.fn(),
    modulesUninstall: vi.fn(),
    backendProxy,
  } as never
  render(<ModulesManagerPage />)
  return { install, backendProxy }
}

const SIN90: DomainOsView = {
  name: 'sin90', namespace: '/api/v1/sin90', version: '0.5.0', enabled: true,
  state: 'mounted', granted: ['events', 'scheduler'], missing_models: [],
  resources: 'ok', restart_required: false,
}

describe('ModulesManagerPage — 模块市场 (marketplace browse)', () => {
  beforeEach(() => vi.restoreAllMocks())

  it('renders discovered modules with trust-tier chips', async () => {
    mount()
    expect(screen.getByText('模块市场')).toBeInTheDocument()
    await waitFor(() => expect(screen.getByText('@auraaihq/wechat')).toBeInTheDocument())
    // trust-tier chips (the "芯片") — queried by title to disambiguate from the
    // filter chips that share the same label text
    expect(screen.getByTitle('信任级：官方')).toBeInTheDocument()
    expect(screen.getByTitle('信任级：社区')).toBeInTheDocument()
    // an installed module shows a badge instead of an install button
    expect(screen.getByText('已安装')).toBeInTheDocument()
  })

  it('typing in the search box queries the daemon with the query (debounced)', async () => {
    mount()
    await waitFor(() => expect(discoverCalls.length).toBeGreaterThan(0))
    fireEvent.change(screen.getByLabelText('搜索模块市场'), { target: { value: 'wechat' } })
    await waitFor(() => expect(discoverCalls.some((f) => f.query === 'wechat')).toBe(true))
  })

  it('clicking a trust-tier filter chip narrows the query to that tier', async () => {
    mount()
    await waitFor(() => expect(screen.getByText('@auraaihq/wechat')).toBeInTheDocument())
    // the "官方" filter chip is a button (aria-pressed); the tier badge is a span
    fireEvent.click(screen.getByRole('button', { name: '官方' }))
    await waitFor(() => expect(discoverCalls.some((f) => f.trustTier === 'official')).toBe(true))
  })

  it('installed / available segmented filter maps to the installed boolean', async () => {
    mount()
    await waitFor(() => expect(discoverCalls.length).toBeGreaterThan(0))
    fireEvent.click(screen.getByRole('button', { name: '已装' }))
    await waitFor(() => expect(discoverCalls.some((f) => f.installed === true)).toBe(true))
    fireEvent.click(screen.getByRole('button', { name: '未装' }))
    await waitFor(() => expect(discoverCalls.some((f) => f.installed === false)).toBe(true))
  })

  it('one-click install calls modulesInstall for the un-installed package', async () => {
    const { install } = mount()
    // aria-label disambiguates the per-row market button from the manual npm panel button
    await waitFor(() => expect(screen.getByRole('button', { name: '安装 @auraaihq/wechat' })).toBeInTheDocument())
    fireEvent.click(screen.getByRole('button', { name: '安装 @auraaihq/wechat' }))
    await waitFor(() => expect(install).toHaveBeenCalledWith('@auraaihq/wechat'))
  })
})

describe('ModulesManagerPage — OS 模块 (FU-90)', () => {
  beforeEach(() => vi.restoreAllMocks())

  it('shows nothing mounted when the daemon has no domain-OS modules', async () => {
    mount({ osList: EMPTY_OS_LIST })
    expect(screen.getByText('OS 模块（内核挂载）')).toBeInTheDocument()
    await waitFor(() => expect(screen.getByText('没有挂载的 OS 模块')).toBeInTheDocument())
  })

  it('renders a mounted module with its granted capabilities via GET /api/v1/os', async () => {
    const { backendProxy } = mount({ osList: { modules: [SIN90] } })
    await waitFor(() => expect(screen.getByText('sin90')).toBeInTheDocument())
    expect(screen.getByText('/api/v1/sin90 · v0.5.0')).toBeInTheDocument()
    expect(screen.getByText('已挂载')).toBeInTheDocument()
    expect(screen.getByText('events')).toBeInTheDocument()
    expect(screen.getByText('scheduler')).toBeInTheDocument()
    expect(backendProxy).toHaveBeenCalledWith({ method: 'GET', path: '/api/v1/os' })
  })

  it('shows missing models and a restart-required badge when reported', async () => {
    mount({
      osList: {
        modules: [{ ...SIN90, resources: 'missing', missing_models: ['Qwen3-8B-4bit'], restart_required: true }],
      },
    })
    await waitFor(() => expect(screen.getByText(/缺少模型：Qwen3-8B-4bit/)).toBeInTheDocument())
    expect(screen.getByText('需重启')).toBeInTheDocument()
  })

  it('a registry_error from the daemon is surfaced', async () => {
    mount({ osList: { modules: [], registry_error: 'os.json disables ["ghost"]' } })
    await waitFor(() => expect(screen.getByText('os.json disables ["ghost"]')).toBeInTheDocument())
  })

  it('clicking 停用 PATCHes enabled:false for that module and refreshes from the response', async () => {
    const { backendProxy } = mount({
      osList: { modules: [SIN90] },
      onOsPatch: (name, enabled) => ({ modules: [{ ...SIN90, name, enabled, state: enabled ? 'mounted' : 'disabled' }] }),
    })
    await waitFor(() => expect(screen.getByRole('button', { name: '停用' })).toBeInTheDocument())
    fireEvent.click(screen.getByRole('button', { name: '停用' }))
    await waitFor(() => expect(screen.getByText('已停用')).toBeInTheDocument())
    expect(backendProxy).toHaveBeenCalledWith({
      method: 'PATCH', path: '/api/v1/os/sin90', body: { enabled: false },
    })
    expect(screen.getByRole('button', { name: '启用' })).toBeInTheDocument()
  })
})
