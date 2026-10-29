// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import ModulesManagerPage from './ModulesManager'
import { setLocale } from '../i18n'
import type { DomainOsList, DomainOsView } from './agent/api'

const EMPTY_OS_LIST: DomainOsList = { modules: [] }

function mount(opts: {
  osList?: DomainOsList
  onOsPatch?: (name: string, enabled: boolean) => DomainOsList
} = {}) {
  const backendProxy = vi.fn((req: { method: string; path: string; body?: unknown }) => {
    if (req.method === 'GET' && req.path === '/api/v1/os') {
      return Promise.resolve({ ok: true, status: 200, data: opts.osList ?? EMPTY_OS_LIST })
    }
    return Promise.resolve({ ok: true, status: 200, data: {} })
  })
  // A1: PATCH /api/v1/os/{name} moved to a dedicated IPC.
  const osModuleSetEnabled = vi.fn((name: string, enabled: boolean) => {
    const data = opts.onOsPatch ? opts.onOsPatch(name, enabled) : EMPTY_OS_LIST
    return Promise.resolve({ ok: true, status: 200, data })
  })
  window.agent24 = {
    backendProxy,
    osModuleSetEnabled,
  } as never
  render(<ModulesManagerPage />)
  return { backendProxy, osModuleSetEnabled }
}

const SIN90: DomainOsView = {
  name: 'sin90', namespace: '/api/v1/sin90', version: '0.5.0', enabled: true,
  state: 'mounted', granted: ['events', 'scheduler'], missing_models: [],
  resources: 'ok', restart_required: false,
}

describe('ModulesManagerPage — English default', () => {
  beforeEach(() => vi.restoreAllMocks())

  it('renders the English title and the OS-modules section', async () => {
    mount()
    expect(screen.getByText('Module management')).toBeInTheDocument()
    await waitFor(() => expect(screen.getByText('No OS modules mounted')).toBeInTheDocument())
  })
})

describe('ModulesManagerPage — OS 模块 (FU-90)', () => {
  beforeEach(() => { vi.restoreAllMocks(); setLocale('zh') })

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
    const { osModuleSetEnabled } = mount({
      osList: { modules: [SIN90] },
      onOsPatch: (name, enabled) => ({ modules: [{ ...SIN90, name, enabled, state: enabled ? 'mounted' : 'disabled' }] }),
    })
    await waitFor(() => expect(screen.getByRole('button', { name: '停用' })).toBeInTheDocument())
    fireEvent.click(screen.getByRole('button', { name: '停用' }))
    await waitFor(() => expect(screen.getByText('已停用')).toBeInTheDocument())
    expect(osModuleSetEnabled).toHaveBeenCalledWith('sin90', false)
    expect(screen.getByRole('button', { name: '启用' })).toBeInTheDocument()
  })
})
