// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { setLocale } from '../../i18n'
import CommunicationPage from './Communication'

const fixtures = vi.hoisted(() => ({ aa: '' }))
vi.mock('./relayDefaults', async (original) => {
  const actual = await original<typeof import('./relayDefaults')>()
  return { ...actual, getDefaultRelays: () => actual.getDefaultRelays(fixtures.aa) }
})
const identity = { nickname: 'Alice', npub: 'npub1alice', default: true, encrypted: true }
function setup({ installed = true, identities = [identity], configured = true, connected = true } = {}) {
  const proxy = vi.fn(async ({ method, path }: { method: string; path: string; body?: unknown }) => {
    let data: unknown = {}
    if (method === 'GET') {
      if (path.endsWith('/daemon')) data = {
        installable: true, installed_version: installed ? 'v0.27.2' : null,
        process: { state: 'stopped', generation: 0, consecutive_failures: 0, reason: null },
        relay_probe: null, catch_up: { state: 'unknown', last_incomplete_at_ms: null },
      }
      else if (!installed) return { ok: false, status: 409, data: { ok: false, error: 'not_configured', message: 'no Hyphae binary is configured' } }
      else if (path.endsWith('/identity')) data = identities
      else if (path.endsWith('/contact')) data = []
      else data = { relays: ['wss://relay.aastar.io'], source: configured ? 'config' : 'default', configured }
    } else if (path.endsWith('/identity')) data = identity
    else if (method === 'PUT') data = { relays: ['wss://relay.aastar.io'], source: 'config', configured: true }
    else if (path.endsWith('/probe')) data = { url: 'wss://relay.aastar.io', connected, error: connected ? null : 'connection refused' }
    else if (path.endsWith('/import')) data = { identities: 1, contacts: 0, outbox: 0, db_files: [] }
    return { ok: true, status: 200, data: { ok: true, data } }
  })
  window.agent24 = { ...window.agent24, backendProxy: proxy }
  return proxy
}
beforeEach(() => { setLocale('en'); fixtures.aa = '' })
afterEach(() => { cleanup(); vi.restoreAllMocks() })

it.each([
  ['not_installed', { installed: false }], ['no_identity', { identities: [] }],
  ['no_relay', { configured: false }], ['ready', {}],
])('derives %s from daemon reads', async (state, options) => {
  setup(options)
  render(<CommunicationPage />)
  expect(await screen.findByTestId('comm-setup')).toHaveAttribute('data-state', state)
})
it('creates a default identity through backendProxy without persisting renderer state', async () => {
  const proxy = setup({ identities: [] })
  const fetchSpy = vi.spyOn(window, 'fetch')
  const storageSpy = vi.spyOn(Storage.prototype, 'setItem')
  render(<CommunicationPage />)
  fireEvent.change(await screen.findByLabelText('Display name'), { target: { value: 'Alice' } })
  fireEvent.click(screen.getByRole('button', { name: 'Create identity' }))
  await waitFor(() => expect(proxy).toHaveBeenCalledWith({ method: 'POST', path: '/api/v1/comm/identity', body: { nickname: 'Alice', default: true } }))
  expect(fetchSpy).not.toHaveBeenCalled()
  expect(storageSpy).not.toHaveBeenCalled()
  expect(screen.getByTestId('comm-setup')).toHaveAttribute('data-state', 'no_identity')
})
it.each(['', 'wss://aastar.example'])('hides empty AAStar and selects supplied AAStar as default: %s', async (url) => {
  fixtures.aa = url
  setup({ configured: false })
  render(<CommunicationPage />)
  expect(await screen.findByRole('combobox', { name: 'Relay server' })).toHaveValue(url || 'wss://relay.aastar.io')
  expect(screen.queryByRole('option', { name: /^AAStar/ }) !== null).toBe(Boolean(url))
})
it('saves the selected relay then probes through backendProxy and shows handshake failure', async () => {
  const proxy = setup({ configured: false, connected: false })
  render(<CommunicationPage />)
  fireEvent.click(await screen.findByRole('button', { name: 'Connect relay' }))
  await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('connection refused'))
  expect(proxy.mock.calls.map(([call]) => call).filter((call) => call.method !== 'GET')).toEqual([
    { method: 'PUT', path: '/api/v1/comm/relay', body: { relays: ['wss://relay.aastar.io'] } },
    { method: 'POST', path: '/api/v1/comm/relay/probe', body: { url: 'wss://relay.aastar.io' } },
  ])
})
it('imports an existing Hyphae home with explicit confirmation and clears the password', async () => {
  const proxy = setup({ identities: [] })
  render(<CommunicationPage />)
  fireEvent.click(await screen.findByRole('button', { name: 'Import an existing key' }))
  fireEvent.change(screen.getByLabelText('Existing Hyphae home'), { target: { value: '/Users/alice' } })
  const password = screen.getByLabelText('Source keystore password')
  fireEvent.change(password, { target: { value: 'secret' } })
  fireEvent.click(screen.getByLabelText('I confirm importing this Hyphae home'))
  fireEvent.click(screen.getByRole('button', { name: 'Import' }))
  await waitFor(() => expect(proxy).toHaveBeenCalledWith({ method: 'POST', path: '/api/v1/comm/import', body: { from: '/Users/alice', password: 'secret', confirm: true } }))
  expect(password).toHaveValue('')
})

it('shows installing while native IPC is pending, then derives the next step from refreshed reads', async () => {
  setup({ installed: false })
  let finish!: (value: Awaited<ReturnType<typeof window.agent24.commInstall>>) => void
  window.agent24 = { ...window.agent24, commInstall: vi.fn(() => new Promise<Awaited<ReturnType<typeof window.agent24.commInstall>>>((resolve) => { finish = resolve })) }
  render(<CommunicationPage />)
  fireEvent.click(await screen.findByRole('button', { name: 'Download and set up' }))
  expect(screen.getByTestId('comm-setup')).toHaveAttribute('data-state', 'installing')
  setup({ identities: [] })
  finish({ ok: true, status: 200, data: { ok: true, data: { installed_version: 'v1', path: '/hyphae', restart_required: false, reason: null } } })
  await waitFor(() => expect(screen.getByTestId('comm-setup')).toHaveAttribute('data-state', 'no_identity'))
})
it('advances after a successful create only when daemon reads confirm the default identity', async () => {
  setup({ identities: [], configured: false })
  render(<CommunicationPage />)
  fireEvent.change(await screen.findByLabelText('Display name'), { target: { value: 'Alice' } })
  setup({ configured: false })
  fireEvent.click(screen.getByRole('button', { name: 'Create identity' }))
  await waitFor(() => expect(screen.getByTestId('comm-setup')).toHaveAttribute('data-state', 'no_relay'))
})
it('shows probe success without treating it as a running daemon', async () => {
  setup({ configured: false })
  render(<CommunicationPage />)
  fireEvent.click(await screen.findByRole('button', { name: 'Connect relay' }))
  expect(await screen.findByText('Last handshake succeeded')).toBeInTheDocument()
  expect(screen.getByText('Stopped')).not.toHaveClass('comm-state-running')
  expect(screen.getByTestId('comm-setup')).toHaveAttribute('data-state', 'no_relay')
})
it('keeps setup unchanged and redacts the password when import fails', async () => {
  const proxy = setup({ identities: [] })
  render(<CommunicationPage />)
  fireEvent.click(await screen.findByRole('button', { name: 'Import an existing key' }))
  fireEvent.change(screen.getByLabelText('Existing Hyphae home'), { target: { value: '/Users/alice' } })
  fireEvent.change(screen.getByLabelText('Source keystore password'), { target: { value: 'private-secret' } })
  fireEvent.click(screen.getByLabelText('I confirm importing this Hyphae home'))
  proxy.mockResolvedValueOnce({ ok: false, status: 409, data: { ok: false, error: 'locked', message: 'rejected private-secret' } })
  fireEvent.click(screen.getByRole('button', { name: 'Import' }))
  expect(await screen.findByRole('alert')).not.toHaveTextContent('private-secret')
  expect(screen.getByLabelText('Source keystore password')).toHaveValue('')
  expect(screen.getByTestId('comm-setup')).toHaveAttribute('data-state', 'no_identity')
})
it.each([
  ['zh', '显示名', '创建身份'], ['th', 'ชื่อที่แสดง', 'สร้างตัวตน'],
] as const)('renders the identity step in %s', async (locale, label, action) => {
  setLocale(locale)
  setup({ identities: [] })
  render(<CommunicationPage />)
  expect(await screen.findByLabelText(label)).toBeInTheDocument()
  expect(screen.getByRole('button', { name: action })).toBeDisabled()
})
