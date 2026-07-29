// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import ModulesManagerPage from './ModulesManager'
import type { DiscoveredModule, DiscoverFilter } from '../../shared/ipc-types'

const CATALOG: DiscoveredModule[] = [
  { packageName: '@auraaihq/wechat', version: '1.0.0', name: '@auraaihq/wechat', description: 'WeChat 桥', trustTier: 'official', installed: false },
  { packageName: '@someorg/weather', version: '2.1.0', name: '@someorg/weather', description: '天气 feed', trustTier: 'community', installed: true },
]

let discoverCalls: DiscoverFilter[] = []

function mount(opts: { discover?: (f: DiscoverFilter) => DiscoveredModule[]; install?: () => { ok: boolean; id?: string; error?: string } } = {}) {
  discoverCalls = []
  const install = vi.fn(() => Promise.resolve(opts.install ? opts.install() : { ok: true, id: 'x' }))
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
  } as never
  render(<ModulesManagerPage />)
  return { install }
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
