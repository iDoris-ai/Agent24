// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { BackendProxyRequest } from '../../shared/ipc-types'
import { setLocale } from '../i18n'
import { CapabilityPage } from './CapabilityPage'
import { CAPABILITIES } from './registry'

const sin90 = CAPABILITIES.find((item) => item.id === 'sin90')!
const cos72 = CAPABILITIES.find((item) => item.id === 'cos72')!

let modules: unknown[] = []
let components: unknown[] = []
const osPackageInstall = vi.fn(async () => ({ ok: true, status: 200, data: { version: '0.5.3', upgraded: true } }))
const osPackageLatest = vi.fn(async () => ({ ok: true, status: 200, data: { version: '0.5.3', upgraded: false } }))

function module(version: string, state = 'mounted') {
  return {
    name: 'sin90', namespace: '/api/v1/sin90', version, enabled: true, state,
    granted: [], missing_models: [], resources: 'ok', restart_required: false,
  }
}

beforeEach(() => {
  setLocale('en')
  modules = []
  components = []
  osPackageInstall.mockClear()
  osPackageLatest.mockClear()
  Object.defineProperty(window, 'agent24', {
    configurable: true,
    value: {
      backendProxy: async (req: BackendProxyRequest) => {
        if (req.path === '/api/v1/os') return { ok: true, status: 200, data: { modules } }
        if (req.path === '/api/v1/components') return { ok: true, status: 200, data: { components } }
        if (req.path === '/api/v1/sin90/today') {
          return { ok: true, status: 200, data: { must_do: [], deep_block: null, inbox: [], carry_over_candidates: [] } }
        }
        if (req.path === '/api/v1/sin90/directions') return { ok: true, status: 200, data: { directions: [] } }
        if (req.path === '/api/v1/sin90/areas') return { ok: true, status: 200, data: { areas: [] } }
        if (req.path.startsWith('/api/v1/sin90/tasks')) return { ok: true, status: 200, data: { tasks: [] } }
        throw new Error(String(req.path))
      },
      osPackageInstall,
      osPackageLatest,
      onOsPackageProgress: () => () => {},
    },
  })
})

afterEach(() => cleanup())

describe('CapabilityPage domain OS', () => {
  it('shows the Sin90 page and an update when a newer release exists', async () => {
    modules = [module('0.5.2')]
    render(<CapabilityPage capability={sin90} />)
    expect(await screen.findByText('Set a direction first')).toBeInTheDocument()
    expect(await screen.findByRole('button', { name: 'Update to v0.5.3' })).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Install the latest version' })).not.toBeInTheDocument()
    expect(screen.queryByText('Not set up')).not.toBeInTheDocument()
    expect(screen.getByText('Ships with Agent24. Work with the Sin90 domain OS.')).toBeInTheDocument()
  })

  it('shows the Sin90 page with no install button when the mounted version is current', async () => {
    modules = [module('0.5.2')]
    osPackageLatest.mockResolvedValue({ ok: true, status: 200, data: { version: '0.5.2', upgraded: false } })
    render(<CapabilityPage capability={sin90} />)
    expect(await screen.findByText('Set a direction first')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Install the latest version' })).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /Update to/ })).not.toBeInTheDocument()
    expect(screen.queryByText('Not set up')).not.toBeInTheDocument()
    act(() => setLocale('zh'))
    expect(screen.getByText('先定一个方向')).toBeInTheDocument()
    expect(screen.getByText('随 Agent24 安装，用来使用 Sin90 领域 OS。')).toBeInTheDocument()
    act(() => setLocale('th'))
    expect(screen.getByText('กำหนดทิศทางก่อน')).toBeInTheDocument()
    expect(screen.getByText('มากับ Agent24 ใช้ทำงานกับ Domain OS ของ Sin90')).toBeInTheDocument()
    act(() => setLocale('en'))
  })

  it('does not render the Sin90 page when the module is not mounted', async () => {
    render(<CapabilityPage capability={sin90} />)
    expect(await screen.findByRole('button', { name: 'Install the latest version' })).toBeInTheDocument()
    expect(screen.queryByText('Set a direction first')).not.toBeInTheDocument()
    expect(screen.queryByText('Not set up')).not.toBeInTheDocument()
  })

  it('offers to install the latest version when Sin90 is not installed', async () => {
    render(<CapabilityPage capability={sin90} />)
    const button = await screen.findByRole('button', { name: 'Install the latest version' })
    expect(screen.getByRole('status')).toHaveTextContent('Not installed')
    fireEvent.click(button)
    await waitFor(() => expect(osPackageInstall).toHaveBeenCalledWith('sin90'))
  })

  it('shows the consent card when the module still needs one, and a denial leaves it installed', async () => {
    modules = [{ ...module('0.5.2'), name: 'cos72', namespace: '/api/v1/cos72' }]
    components = [{ id: 'cos72', version: '0.5.2', state: 'ready', provides: [], kernel_consent: 'missing' }]
    const posts: BackendProxyRequest[] = []
    Object.defineProperty(window, 'agent24', {
      configurable: true,
      value: {
        backendProxy: async (req: BackendProxyRequest) => {
          if (req.path === '/api/v1/os') return { ok: true, status: 200, data: { modules } }
          if (req.path === '/api/v1/components') return { ok: true, status: 200, data: { components } }
          if (req.method === 'GET' && req.path === '/api/v1/os/cos72/kernel-consent') {
            return {
              ok: true, status: 200, data: {
                module: 'cos72', class: 'missing', fingerprint: 'k=events;o=', version: '0.5.2',
                team: 'OTHER', origin: 'local', isolated: true, capabilities: ['events'], consented: null,
              },
            }
          }
          posts.push(req)
          return { ok: false, status: 403, data: { error: { code: 'presence_denied', message: 'no' } } }
        },
        osPackageInstall,
        osPackageLatest,
        onOsPackageProgress: () => () => {},
      },
    })
    render(<CapabilityPage capability={cos72} />)
    expect(await screen.findByRole('status')).toHaveTextContent('Installed v0.5.2')
    const box = await screen.findByRole('checkbox', { name: 'Send events under this module\'s name' })
    expect(box).toBeChecked()
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }))
    await waitFor(() => expect(posts).toHaveLength(1))
    expect(screen.getByText(/without kernel capabilities/)).toBeInTheDocument()
    expect(screen.getAllByRole('status').some((node) => node.textContent?.includes('Installed v0.5.2'))).toBe(true)
    expect(screen.queryByText(/Failed/)).not.toBeInTheDocument()
  })

  it('does not ask for an exempt module', async () => {
    modules = [{ ...module('0.5.2'), name: 'cos72', namespace: '/api/v1/cos72' }]
    components = [{ id: 'cos72', version: '0.5.2', state: 'ready', provides: [], kernel_consent: 'exempt' }]
    render(<CapabilityPage capability={cos72} />)
    expect(await screen.findByRole('status')).toHaveTextContent('Installed v0.5.2')
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument()
  })

  it('shows an integrity failure and does not offer Cos72 a download', async () => {
    components = [{ id: 'sin90', version: '0.5.2', state: 'failed', reason: 'hash_mismatch', provides: [] }]
    const { unmount } = render(<CapabilityPage capability={sin90} />)
    expect(await screen.findByRole('status')).toHaveTextContent('文件校验失败，请重装可信版本')
    expect(screen.getByRole('button', { name: 'Install the latest version' })).toBeInTheDocument()
    unmount()
    modules = []
    components = []
    render(<CapabilityPage capability={cos72} />)
    expect(await screen.findByRole('status')).toHaveTextContent('Not installed')
    expect(screen.getByText(/not in the install registry/)).toBeInTheDocument()
    expect(screen.queryByRole('button')).not.toBeInTheDocument()
  })
})
