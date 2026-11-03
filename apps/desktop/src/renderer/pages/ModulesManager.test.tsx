// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import ModulesManagerPage from './ModulesManager'
import { setLocale } from '../i18n'
import type { DomainOsList, DomainOsView } from './agent/api'

const EMPTY_OS_LIST: DomainOsList = { modules: [] }

type DocStatus = { bundled: boolean; installed: boolean; externalDaemon: boolean }
type Res = { ok: boolean; status: number; data: unknown }

function mount(opts: {
  osList?: DomainOsList
  onOsPatch?: (name: string, enabled: boolean) => DomainOsList
  docStatus?: () => DocStatus
  uninstall?: () => Promise<Res>
  install?: () => Promise<Res>
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
  const ok = (): Promise<Res> => Promise.resolve({ ok: true, status: 200, data: { ok: true } })
  const osModuleUninstall = vi.fn(opts.uninstall ?? ok)
  const osModuleInstallBundled = vi.fn(opts.install ?? ok)
  const moduleStatus = vi.fn(() => Promise.resolve(opts.docStatus?.() ?? { bundled: false, installed: false, externalDaemon: false }))
  window.agent24 = {
    backendProxy,
    osModuleSetEnabled,
    osModuleUninstall,
    osModuleInstallBundled,
    documents: { moduleStatus },
  } as never
  render(<ModulesManagerPage />)
  return { backendProxy, osModuleSetEnabled, osModuleUninstall, osModuleInstallBundled, moduleStatus }
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

// T1-2: the documents module the mac app carries.
describe('ModulesManagerPage — documents uninstall / install (T1-2)', () => {
  beforeEach(() => { vi.restoreAllMocks(); setLocale('zh') })
  const DOCS: DomainOsView = {
    name: 'documents', namespace: '/api/v1/documents', version: '0.2.0', enabled: true,
    state: 'mounted', granted: ['events', 'models'], missing_models: [], resources: 'ok', restart_required: false,
  }

  it('installed and listed: an Uninstall button on its row only', async () => {
    const { osModuleUninstall, backendProxy } = mount({
      osList: { modules: [SIN90, DOCS] },
      docStatus: () => ({ bundled: true, installed: true, externalDaemon: false }),
    })
    const button = await screen.findByRole('button', { name: '卸载' })
    expect(screen.getAllByRole('button', { name: '卸载' })).toHaveLength(1)
    fireEvent.click(button)
    await waitFor(() => expect(osModuleUninstall).toHaveBeenCalledOnce())
    // The list is read again afterwards.
    await waitFor(() => expect(backendProxy.mock.calls.filter(([r]) => r.path === '/api/v1/os').length).toBe(2))
  })

  it('uninstalled but the daemon still lists it: says to restart, no buttons for it (not even on/off)', async () => {
    mount({ osList: { modules: [DOCS] }, docStatus: () => ({ bundled: true, installed: false, externalDaemon: true }) })
    expect(await screen.findByText('已卸载，重启 daemon 后生效')).toBeInTheDocument()
    expect(screen.getByText(/agent24 daemon stop && agent24 daemon start/)).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: '卸载' })).toBeNull()
    expect(screen.queryByRole('button', { name: '安装' })).toBeNull()
    // Turning it off now would leave os.json naming a module that is gone.
    expect(screen.queryByRole('button', { name: '停用' })).toBeNull()
  })

  it('a failed install keeps the Install row, with the error beside it', async () => {
    const fail = (): Promise<Res> => Promise.resolve({ ok: false, status: 500, data: { error: { code: 'signature_invalid', message: '失败：a sealed resource is missing or invalid' } } })
    mount({ osList: { modules: [SIN90] }, docStatus: () => ({ bundled: true, installed: false, externalDaemon: false }), install: fail })
    fireEvent.click(await screen.findByRole('button', { name: '安装' }))
    expect(await screen.findByText(/安装 documents 失败：.*sealed resource/)).toBeInTheDocument()
    await waitFor(() => expect(screen.getByRole('button', { name: '安装' })).toBeEnabled())
    expect(screen.getByText('sin90')).toBeInTheDocument()
  })

  it('not installed and the app carries it: an Install row', async () => {
    let status: DocStatus = { bundled: true, installed: false, externalDaemon: false }
    const { osModuleInstallBundled } = mount({
      osList: { modules: [SIN90] },
      docStatus: () => status,
      install: () => { status = { ...status, installed: true }; return Promise.resolve({ ok: true, status: 200, data: { ok: true } }) },
    })
    expect(await screen.findByText('未安装。Agent24 自带这个模块，可在这里安装。')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: '安装' }))
    await waitFor(() => expect(osModuleInstallBundled).toHaveBeenCalledOnce())
    // Installed, not yet mounted: waits for a restart.
    expect(await screen.findByText('已安装，daemon 重启后挂载。')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: '安装' })).toBeNull()
  })

  it('installed but not listed, daemon started outside the app: the CLI restart command', async () => {
    mount({ osList: { modules: [] }, docStatus: () => ({ bundled: true, installed: true, externalDaemon: true }) })
    expect(await screen.findByText(/agent24 daemon stop && agent24 daemon start/)).toBeInTheDocument()
  })

  it('no bundle and not installed: no documents row at all', async () => {
    mount({ osList: { modules: [SIN90] }, docStatus: () => ({ bundled: false, installed: false, externalDaemon: false }) })
    await screen.findByText('sin90')
    expect(screen.queryByText('documents')).toBeNull()
    expect(screen.queryByRole('button', { name: '安装' })).toBeNull()
  })

  it('a failure is shown with its message, after the list is read again', async () => {
    const fail = (): Promise<Res> => Promise.resolve({ ok: false, status: 500, data: { error: { code: 'cli_failed', message: '失败：agent24 os uninstall documents (rc=2): store locked' } } })
    mount({ osList: { modules: [DOCS] }, docStatus: () => ({ bundled: true, installed: true, externalDaemon: false }), uninstall: fail })
    fireEvent.click(await screen.findByRole('button', { name: '卸载' }))
    expect(await screen.findByText(/卸载 documents 失败：.*store locked/)).toBeInTheDocument()
  })

  it('cancelled in the dialog: no error', async () => {
    const cancel = (): Promise<Res> => Promise.resolve({ ok: false, status: 499, data: { error: { code: 'cancelled', message: 'cancelled' } } })
    const { osModuleUninstall } = mount({ osList: { modules: [DOCS] }, docStatus: () => ({ bundled: true, installed: true, externalDaemon: false }), uninstall: cancel })
    fireEvent.click(await screen.findByRole('button', { name: '卸载' }))
    await waitFor(() => expect(osModuleUninstall).toHaveBeenCalledOnce())
    expect(screen.queryByText(/失败/)).toBeNull()
  })
})
