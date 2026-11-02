// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { setLocale } from '../../i18n'
import CommunicationPage from './Communication'

const fixtures = vi.hoisted(() => ({ aa: undefined as string | undefined }))
vi.mock('./relayDefaults', async (original) => {
  const actual = await original<typeof import('./relayDefaults')>()
  return { ...actual, getDefaultRelays: () => actual.getDefaultRelays(fixtures.aa) }
})
const identity = { nickname: 'Alice', npub: 'npub1alice', default: true, encrypted: true }
function setup({ installed = true, identities = [identity], configured = true, connected = true, relayList = ['wss://relay.aastar.io'] } = {}) {
  const proxy = vi.fn(async ({ method, path, body }: { method: string; path: string; body?: unknown }) => {
    let data: unknown = {}
    if (method === 'GET') {
      if (path.endsWith('/daemon')) data = {
        installable: true, installed_version: installed ? 'v0.28.1' : null,
        process: { state: 'stopped', generation: 0, consecutive_failures: 0, reason: null },
        relay_probe: null, catch_up: { state: 'unknown', last_incomplete_at_ms: null },
      }
      else if (!installed) return { ok: false, status: 409, data: { ok: false, error: 'not_configured', message: 'no Hyphae binary is configured' } }
      else if (path.endsWith('/identity')) data = identities
      else if (path.endsWith('/contact')) data = []
      else data = { relays: relayList, source: configured ? 'config' : 'default', configured }
    } else if (path.endsWith('/identity')) data = identity
    else if (method === 'PUT') data = { relays: ['wss://relay.aastar.io'], source: 'config', configured: true }
    else if (path.endsWith('/probe')) data = { url: (body as { url: string }).url, connected, error: connected ? null : 'connection refused' }
    else if (path.endsWith('/import')) data = { identities: 1, contacts: 0, outbox: 0, db_files: [] }
    return { ok: true, status: 200, data: { ok: true, data } }
  })
  // The relay list and the whole-home import have their own IPC (not the generic
  // proxy); both are recorded on `proxy` so tests can see every write in order.
  const commRelaySet = vi.fn((relays: string[]) => proxy({ method: 'PUT', path: '/api/v1/comm/relay', body: { relays } }))
  const commHomeImport = vi.fn(() => proxy({ method: 'POST', path: '/api/v1/comm/import' }))
  window.agent24 = { ...window.agent24, backendProxy: proxy, commRelaySet, commHomeImport } as never
  return proxy
}
beforeEach(() => { setLocale('en'); fixtures.aa = undefined })
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
it('offers the Hyphae relay as the only default', async () => {
  setup({ configured: false })
  render(<CommunicationPage />)
  expect(await screen.findByRole('combobox', { name: 'Relay server' })).toHaveValue('wss://relay.aastar.io')
  expect(screen.getByRole('option', { name: /^Hyphae \(AAStar\)/ })).toBeInTheDocument()
  expect(screen.queryByRole('option', { name: /agent-speaker-relay/ })).toBeNull()
})
it('offers no default when the relay url is empty', async () => {
  fixtures.aa = ''
  setup({ configured: false })
  render(<CommunicationPage />)
  await screen.findByRole('combobox', { name: 'Relay server' })
  expect(screen.queryByRole('option', { name: /^Hyphae/ })).toBeNull()
})
it('saves the selected relay (dedicated IPC) then probes through backendProxy and shows handshake failure', async () => {
  const proxy = setup({ configured: false, connected: false })
  render(<CommunicationPage />)
  fireEvent.click(await screen.findByRole('button', { name: 'Connect relay' }))
  await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('connection refused'))
  expect(proxy.mock.calls.map(([call]) => call).filter((call) => call.method !== 'GET')).toEqual([
    { method: 'PUT', path: '/api/v1/comm/relay', body: { relays: ['wss://relay.aastar.io'] } },
    { method: 'POST', path: '/api/v1/comm/relay/probe', body: { url: 'wss://relay.aastar.io' } },
  ])
})
const A = 'wss://a.example'
const B = 'wss://b.example'
const writes = (proxy: ReturnType<typeof setup>) => proxy.mock.calls.map(([call]) => call).filter((call) => call.method !== 'GET')
it('B1: connecting the already-first relay skips the PUT (no daemon restart) and only probes', async () => {
  const proxy = setup({ relayList: [A, B] })
  render(<CommunicationPage />)
  fireEvent.click(await screen.findByRole('button', { name: 'Connect relay' }))
  await waitFor(() => expect(writes(proxy)).toHaveLength(1))
  expect(writes(proxy)).toEqual([{ method: 'POST', path: '/api/v1/comm/relay/probe', body: { url: A } }])
})
it('B1: choosing another configured relay moves it first and keeps the rest of the list', async () => {
  const proxy = setup({ relayList: [A, B] })
  render(<CommunicationPage />)
  fireEvent.change(await screen.findByRole('combobox', { name: 'Relay server' }), { target: { value: B } })
  fireEvent.click(screen.getByRole('button', { name: 'Connect relay' }))
  await waitFor(() => expect(writes(proxy)).toHaveLength(2))
  expect(writes(proxy)[0]).toEqual({ method: 'PUT', path: '/api/v1/comm/relay', body: { relays: [B, A] } })
})
it('B1: the saved list is capped at 8 entries', async () => {
  const many = Array.from({ length: 8 }, (_, i) => `wss://r${i}.example`)
  const proxy = setup({ relayList: many })
  render(<CommunicationPage />)
  fireEvent.change(await screen.findByRole('combobox', { name: 'Relay server' }), { target: { value: 'wss://relay.aastar.io' } })
  fireEvent.click(screen.getByRole('button', { name: 'Connect relay' }))
  await waitFor(() => expect(writes(proxy).length).toBeGreaterThan(0))
  const put = writes(proxy)[0].body as { relays: string[] }
  expect(put.relays).toHaveLength(8)
  expect(put.relays[0]).toBe('wss://relay.aastar.io')
})
it('F1: a failed handshake removes the relay check mark and the "setup is complete" claim', async () => {
  setup({ connected: false })
  render(<CommunicationPage />)
  const card = await screen.findByTestId('comm-setup')
  await waitFor(() => expect(card.textContent).toContain('Connect relay ✓'))
  expect(card.querySelectorAll('.comm-step-mark')).toHaveLength(3)
  expect(card.textContent).toContain('Setup is complete')
  fireEvent.click(screen.getByRole('button', { name: 'Connect relay' }))
  await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('connection refused'))
  expect(card.textContent).not.toContain('Connect relay ✓')
  expect(card.querySelectorAll('.comm-step-mark')).toHaveLength(2)
  expect(card.textContent).not.toContain('Setup is complete')
  expect(card.textContent).toContain('Relay saved, but the handshake failed')
})
it('N2: a stale relay choice no longer offered after refresh falls back to the default option', async () => {
  setup({ relayList: [A, B] })
  render(<CommunicationPage />)
  fireEvent.change(await screen.findByRole('combobox', { name: 'Relay server' }), { target: { value: B } })
  const C = 'wss://c.example'
  const proxy = setup({ relayList: [C] })
  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))
  await waitFor(() => expect(screen.getByRole('combobox', { name: 'Relay server' })).toHaveValue(C))
  fireEvent.click(screen.getByRole('button', { name: 'Connect relay' }))
  await waitFor(() => expect(writes(proxy)).toHaveLength(1))
  expect(writes(proxy)).toEqual([{ method: 'POST', path: '/api/v1/comm/relay/probe', body: { url: C } }])
})
it('imports an existing Hyphae home through its dedicated IPC: the page supplies no path or password', async () => {
  const proxy = setup({ identities: [] })
  render(<CommunicationPage />)
  fireEvent.click(await screen.findByRole('button', { name: 'Import an existing key' }))
  expect(screen.queryByLabelText(/keystore password/i)).toBeNull()
  fireEvent.click(screen.getByRole('button', { name: 'Choose folder and import…' }))
  await waitFor(() => expect(window.agent24.commHomeImport).toHaveBeenCalledTimes(1))
  expect(window.agent24.commHomeImport).toHaveBeenCalledWith()
  expect(proxy).toHaveBeenCalledWith({ method: 'POST', path: '/api/v1/comm/import' })
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
it('keeps setup unchanged and shows the reason when the home import fails; a declined step is silent', async () => {
  setup({ identities: [] })
  render(<CommunicationPage />)
  fireEvent.click(await screen.findByRole('button', { name: 'Import an existing key' }))
  const failing = vi.fn()
    .mockResolvedValueOnce({ ok: false, status: 409, data: { ok: false, error: 'locked', message: 'keychain_unavailable: denied' } })
    .mockResolvedValueOnce({ ok: false, status: 499, data: { error: { code: 'cancelled', message: 'c' } } })
  window.agent24 = { ...window.agent24, commHomeImport: failing } as never
  fireEvent.click(screen.getByRole('button', { name: 'Choose folder and import…' }))
  expect(await screen.findByRole('alert')).toHaveTextContent('keychain_unavailable')
  expect(screen.getByTestId('comm-setup')).toHaveAttribute('data-state', 'no_identity')
  fireEvent.click(screen.getByRole('button', { name: 'Choose folder and import…' }))
  await waitFor(() => expect(failing).toHaveBeenCalledTimes(2))
  await waitFor(() => expect(screen.queryByRole('alert')).toBeNull())
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
