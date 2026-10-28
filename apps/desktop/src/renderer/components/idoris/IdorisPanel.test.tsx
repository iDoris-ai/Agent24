// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { components } from '@agent24/api-client'
import type { BackendProxyRequest } from '../../../shared/ipc-types'
import { setLocale } from '../../i18n'
import IdorisPanel from './IdorisPanel'
import { idorisErrors } from './IdorisPanel.i18n'

type Status = components['schemas']['IdorisStatus']
type Operation = components['schemas']['IdorisOperation']
const initial: Status = { state: 'not_installed', version: null, data_port: null, admin_port: null, roles: [], last_error: null, current_operation: null, verification: null, budget_suggestion: null, budget_unavailable_reason: 'hardware_memory_unavailable' }
const installed: Status = { ...initial, state: 'not_configured', version: '0.4.0' }
const running: Status = { ...installed, state: 'running', data_port: 8740, admin_port: 8741, roles: ['daily'] }
const candidate = (endpoint: string) => ({ endpoint, identity: 'omlx', authentication: 'ready', models: ['real/daily', 'real/deep'] })
const operation = (action: Operation['action']): Operation => ({ operation_id: 'op-1', action, phase: 'queued', result: null, error_code: null, cancellable: true, idempotency_key: 'test' })
let status: Status
let op: Operation
let candidates: unknown
let failure: string | null
const respond = async (req: BackendProxyRequest) => {
  if (req.path.endsWith('/status')) return { ok: true, status: 200, data: status }
  if (req.path.includes('/omlx/candidates')) return { ok: true, status: 200, data: candidates }
  if (req.method === 'GET') return { ok: true, status: 200, data: op }
  if (failure) return { ok: false, status: 503, data: { error: { code: failure, message: 'DO NOT DISPLAY RAW SERVER TEXT' } } }
  if (req.path.endsWith('/cancel')) {
    op = { ...op, result: 'cancelled', error_code: 'cancelled', cancellable: false }
    status = { ...status, current_operation: null }
  } else {
    const action = req.path.split('/').at(-1)
    if (action === 'install' || action === 'connect' || action === 'start' || action === 'stop') op = operation(action)
    status = { ...status, current_operation: op.operation_id }
  }
  return { ok: true, status: 202, data: op }
}
const proxy = vi.fn(respond)
beforeEach(() => {
  status = initial; op = operation('install'); failure = null
  candidates = { key_from_local_settings: true, candidates: [candidate('http://127.0.0.1:8088'), candidate('http://127.0.0.1:8000')] }
  proxy.mockReset(); proxy.mockImplementation(respond)
  window.agent24 = { backendProxy: proxy } as never
})
afterEach(() => { cleanup(); vi.useRealTimers() })
const click = (name: string) => fireEvent.click(screen.getByRole('button', { name }))
async function configure() {
  status = installed
  render(<IdorisPanel />)
  await within(await screen.findByLabelText('oMLX endpoint')).findByRole('option', { name: /http:\/\/127.0.0.1:8088/ })
  fireEvent.change(screen.getByLabelText('oMLX endpoint'), { target: { value: 'http://127.0.0.1:8088' } })
  await within(screen.getByLabelText('Daily model (required)')).findByRole('option', { name: 'real/daily' })
  fireEvent.change(screen.getByLabelText('Daily model (required)'), { target: { value: 'real/daily' } })
  fireEvent.change(screen.getByLabelText('Core reserved memory (bytes)'), { target: { value: '4294967296' } })
}
describe('iDoris daemon projection', () => {
  it('prefills the hardware estimate but requires confirmation and preserves edits on refresh', async () => {
    status = { ...installed, budget_suggestion: { core_reserved_bytes: 123456, idoris_budget_bytes: 789012 }, budget_unavailable_reason: null }
    render(<IdorisPanel />)
    const input = await screen.findByLabelText('Core reserved memory (bytes)')
    expect(input).toHaveValue('123456')
    expect(screen.getByText(/789012 B/)).toBeInTheDocument()
    await within(screen.getByLabelText('oMLX endpoint')).findByRole('option', { name: /http:\/\/127.0.0.1:8088/ })
    fireEvent.change(screen.getByLabelText('oMLX endpoint'), { target: { value: 'http://127.0.0.1:8088' } })
    await within(screen.getByLabelText('Daily model (required)')).findByRole('option', { name: 'real/daily' })
    fireEvent.change(screen.getByLabelText('Daily model (required)'), { target: { value: 'real/daily' } })
    expect(screen.getByLabelText(/I agree to share/)).not.toBeChecked()
    expect(screen.getByRole('button', { name: 'Connect' })).toBeDisabled()
    expect(proxy.mock.calls.every(([req]) => req.method === 'GET')).toBe(true)
    fireEvent.click(screen.getByLabelText(/I agree to share/))
    expect(screen.getByRole('button', { name: 'Connect' })).toBeEnabled()
    fireEvent.change(input, { target: { value: '' } })
    expect(screen.getByLabelText(/I agree to share/)).not.toBeChecked()
    click('Refresh')
    await waitFor(() => expect(proxy.mock.calls.filter(([req]) => req.path.endsWith('/status')).length).toBe(2))
    expect(input).toHaveValue('')
  })
  it.each([1024, undefined])('renders streamed download bytes with total %s', async (total) => {
    status = { ...initial, current_operation: 'op-1' }
    op = { ...operation('install'), phase: 'downloading', ...{ download_progress: { bytes_done: 512, ...(total === undefined ? {} : { bytes_total: total }) } } }
    render(<IdorisPanel />)
    const progress = await screen.findByRole('progressbar')
    if (total === undefined) expect(progress).not.toHaveAttribute('value')
    else { expect(progress).toHaveAttribute('value', '512'); expect(progress).toHaveAttribute('max', '1024') }
    expect(screen.getByText(/512 B/)).toBeInTheDocument()
  })
  it('shows the persisted verification receipt on a running card after remount', async () => {
    status = { ...running, ...{ verification: { model_id: 'verified/model', record_id: 'record-123', locality: 'loopback', verified_at: '2026-10-28T12:00:00Z' } } }
    render(<IdorisPanel />)
    expect(await screen.findByText(/verified\/model/)).toBeInTheDocument()
    expect(screen.getByText(/record-123/)).toBeInTheDocument()
    expect(screen.getByText(/loopback/)).toBeInTheDocument()
    expect(screen.getByText(/2026-10-28T12:00:00Z/)).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Send a test message' })).toBeNull()
  })
  it('renders not installed and starts consented installation through backendProxy', async () => {
    render(<IdorisPanel />)
    await screen.findByRole('button', { name: 'Download and install' }); click('Download and install')
    await waitFor(() => expect(proxy).toHaveBeenCalledWith(expect.objectContaining({ method: 'POST', path: '/api/v1/idoris/install', body: { consent: true }, idempotencyKey: expect.any(String) })))
    expect(await screen.findByRole('button', { name: 'Cancel' })).toBeEnabled()
  })
  it.each<Status['state']>(['not_configured', 'stopped', 'starting', 'running', 'failed', 'instance_conflict'])('renders daemon state %s', async (state) => {
    status = { ...running, state, last_error: state === 'instance_conflict' ? 'instance_conflict' : null }
    render(<IdorisPanel />)
    expect(await screen.findByTestId('idoris-state')).toHaveAttribute('data-state', state)
    expect(screen.getByText(/0.4.0/)).toBeInTheDocument()
  })
  it('polls install phases and cancels the current daemon operation', async () => {
    vi.useFakeTimers()
    status = { ...initial, current_operation: 'op-1' }; op = { ...operation('install'), phase: 'downloading' }
    render(<IdorisPanel />)
    await act(async () => {})
    expect(screen.getByRole('progressbar')).toHaveAccessibleName('Downloading')
    op = { ...op, phase: 'verifying' }
    await act(async () => { await vi.advanceTimersByTimeAsync(1500) })
    expect(screen.getByRole('progressbar')).toHaveAccessibleName('Verifying download')
    click('Cancel')
    await act(async () => {})
    expect(proxy).toHaveBeenCalledWith(expect.objectContaining({ method: 'POST', path: '/api/v1/idoris/operations/op-1/cancel' }))
    expect(screen.queryByRole('button', { name: 'Cancel' })).toBeNull()
  })
  it('recovers an in-flight connection after remount and respects non-cancellable phases', async () => {
    status = { ...installed, current_operation: 'op-1' }; op = { ...operation('connect'), phase: 'issuing_key', cancellable: false }
    render(<IdorisPanel />)
    expect(await screen.findByRole('progressbar')).toHaveAccessibleName('Storing data key in Keychain')
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeDisabled()
  })
  it('requires candidate choice, re-probes the confirmed endpoint, and never defaults daily to the first model', async () => {
    status = installed; render(<IdorisPanel />)
    const endpoint = await screen.findByLabelText('oMLX endpoint')
    await within(endpoint).findByRole('option', { name: /http:\/\/127.0.0.1:8088/ })
    expect(endpoint).toHaveValue('')
    expect(screen.getAllByRole('option').map(o => o.textContent).join(' ')).toContain('http://127.0.0.1:8000')
    fireEvent.change(endpoint, { target: { value: 'http://127.0.0.1:8088' } })
    await within(screen.getByLabelText('Daily model (required)')).findByRole('option', { name: 'real/daily' })
    expect(screen.getByLabelText('Daily model (required)')).toHaveValue('')
    expect(screen.getByRole('button', { name: 'Connect' })).toBeDisabled()
    expect(proxy).toHaveBeenCalledWith({ method: 'GET', path: '/api/v1/idoris/omlx/candidates?endpoint=http%3A%2F%2F127.0.0.1%3A8088' })
    expect(screen.getByText('Read from local oMLX settings')).toBeInTheDocument()
  })
  it('requires consent and sends exact models, budget, save choice and a fresh idempotency key', async () => {
    await configure()
    expect(screen.getByRole('button', { name: 'Connect' })).toBeDisabled()
    fireEvent.change(screen.getByLabelText('Deep model (optional)'), { target: { value: 'real/deep' } })
    fireEvent.click(screen.getByLabelText('Save oMLX key to Keychain'))
    fireEvent.click(screen.getByLabelText(/I agree to share/))
    click('Connect')
    await waitFor(() => expect(proxy).toHaveBeenCalledWith({ method: 'POST', path: '/api/v1/idoris/connect', idempotencyKey: expect.stringMatching(/^[a-f0-9-]{36}$/), body: {
      endpoint: 'http://127.0.0.1:8088', daily_model: 'real/daily', deep_model: 'real/deep', save_omlx_key: true, replace: false, core_reserved_bytes: 4294967296,
      consent: { share_omlx_key: true, daily_model: 'real/daily', deep_model: 'real/deep' },
    } }))
  })
  it('keeps zero explicit and rejects blank, negative, fractional and unsafe budgets', async () => {
    await configure(); fireEvent.click(screen.getByLabelText(/I agree to share/))
    for (const value of ['', '-1', '0.5', '9007199254740992']) {
      fireEvent.change(screen.getByLabelText('Core reserved memory (bytes)'), { target: { value } })
      expect(screen.getByRole('button', { name: 'Connect' })).toBeDisabled()
    }
    fireEvent.change(screen.getByLabelText('Core reserved memory (bytes)'), { target: { value: '0' } })
    fireEvent.click(screen.getByLabelText(/I agree to share/))
    expect(screen.getByRole('button', { name: 'Connect' })).toBeEnabled()
  })
  it.each(['auth_required', 'auth_rejected'])('guides %s users to oMLX settings without accepting a key', async (authentication) => {
    status = installed; candidates = { key_from_local_settings: false, candidates: [{ ...candidate('http://127.0.0.1:8088'), authentication, models: [] }] }
    render(<IdorisPanel />)
    const endpoint = await screen.findByLabelText('oMLX endpoint')
    await within(endpoint).findByRole('option', { name: /http:\/\/127.0.0.1:8088/ })
    fireEvent.change(endpoint, { target: { value: 'http://127.0.0.1:8088' } })
    expect(await screen.findByText(/Open oMLX settings/)).toBeInTheDocument()
    expect(screen.queryByLabelText('API key')).toBeNull()
    expect(screen.getByRole('button', { name: 'Connect' })).toBeDisabled()
  })
  it('polls connection success and displays its verification receipt', async () => {
    status = installed; render(<IdorisPanel />)
    const endpoint = await screen.findByLabelText('oMLX endpoint')
    await within(endpoint).findByRole('option', { name: /http:\/\/127.0.0.1:8088/ })
    fireEvent.change(endpoint, { target: { value: 'http://127.0.0.1:8088' } })
    await within(screen.getByLabelText('Daily model (required)')).findByRole('option', { name: 'real/daily' })
    fireEvent.change(screen.getByLabelText('Daily model (required)'), { target: { value: 'real/daily' } })
    fireEvent.change(screen.getByLabelText('Core reserved memory (bytes)'), { target: { value: '4294967296' } }); fireEvent.click(screen.getByLabelText(/I agree to share/)); click('Connect')
    await screen.findByRole('progressbar')
    op = { ...op, result: 'succeeded', phase: 'done', cancellable: false, verification: { model_id: 'real/daily', record_id: 'polled-record', locality: 'loopback', verified_at: '2026-10-28T12:00:00Z' } }; status = running
    expect(await screen.findByText(/Daily chat verified/)).toBeInTheDocument()
    expect(screen.getByText(/polled-record/)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Stop' })).toBeEnabled()
  })
  it('start and stop are daemon operations, without optimistic runtime state', async () => {
    status = { ...running, state: 'stopped' }; render(<IdorisPanel />)
    await screen.findByRole('button', { name: 'Start' }); click('Start')
    await screen.findByRole('progressbar')
    expect(screen.getByTestId('idoris-state')).toHaveAttribute('data-state', 'stopped')
    expect(proxy).toHaveBeenCalledWith(expect.objectContaining({ method: 'POST', path: '/api/v1/idoris/start', idempotencyKey: expect.any(String) }))
  })
  it('cancels connection and renders the daemon cancellation result', async () => {
    await configure(); fireEvent.click(screen.getByLabelText(/I agree to share/)); click('Connect')
    await screen.findByRole('progressbar'); click('Cancel')
    expect(await screen.findByRole('alert')).toHaveTextContent('Operation cancelled.')
    expect(proxy).toHaveBeenCalledWith({ method: 'POST', path: '/api/v1/idoris/operations/op-1/cancel' })
    expect(screen.getByRole('button', { name: 'Connect' })).toBeDisabled()
  })
  it('stops a running instance through backendProxy', async () => {
    status = running; render(<IdorisPanel />)
    await screen.findByRole('button', { name: 'Stop' }); click('Stop')
    await screen.findByRole('progressbar')
    expect(proxy).toHaveBeenCalledWith(expect.objectContaining({ method: 'POST', path: '/api/v1/idoris/stop', idempotencyKey: expect.any(String) }))
    expect(screen.getByTestId('idoris-state')).toHaveAttribute('data-state', 'running')
  })
  it('does not connect unknown services even when they advertise models', async () => {
    status = installed; candidates = { key_from_local_settings: false, candidates: [{ ...candidate('http://127.0.0.1:8088'), identity: 'unknown_service' }] }
    render(<IdorisPanel />)
    await within(await screen.findByLabelText('oMLX endpoint')).findByRole('option', { name: /http:\/\/127.0.0.1:8088/ })
    fireEvent.change(screen.getByLabelText('oMLX endpoint'), { target: { value: 'http://127.0.0.1:8088' } })
    await within(screen.getByLabelText('Daily model (required)')).findByRole('option', { name: 'real/daily' })
    fireEvent.change(screen.getByLabelText('Daily model (required)'), { target: { value: 'real/daily' } })
    fireEvent.change(screen.getByLabelText('Core reserved memory (bytes)'), { target: { value: '0' } })
    fireEvent.click(screen.getByLabelText(/I agree to share/))
    expect(screen.getByRole('button', { name: 'Connect' })).toBeDisabled()
    expect(screen.queryByText('Read from local oMLX settings')).toBeNull()
  })
  it('does not require saving the oMLX key and sends no deep binding by default', async () => {
    await configure(); fireEvent.click(screen.getByLabelText(/I agree to share/)); click('Connect')
    await waitFor(() => expect(proxy).toHaveBeenCalledWith(expect.objectContaining({ path: '/api/v1/idoris/connect', body: expect.objectContaining({ deep_model: null, save_omlx_key: false, consent: { share_omlx_key: true, daily_model: 'real/daily', deep_model: null } }) })))
  })
  it('invalidates model selection and consent when the endpoint changes, ignoring late probe replies', async () => {
    let resolveFirst: ((value: Awaited<ReturnType<typeof respond>>) => void) | undefined
    const first = new Promise<Awaited<ReturnType<typeof respond>>>(resolve => { resolveFirst = resolve })
    proxy.mockImplementation(req => req.path.endsWith('8088') ? first : respond(req))
    status = installed; render(<IdorisPanel />)
    const select = await screen.findByLabelText('oMLX endpoint')
    await within(select).findByRole('option', { name: /http:\/\/127.0.0.1:8088/ })
    fireEvent.change(select, { target: { value: 'http://127.0.0.1:8088' } })
    fireEvent.change(select, { target: { value: 'http://127.0.0.1:8000' } })
    await within(screen.getByLabelText('Daily model (required)')).findByRole('option', { name: 'real/daily' })
    fireEvent.change(screen.getByLabelText('Daily model (required)'), { target: { value: 'real/daily' } })
    fireEvent.click(screen.getByLabelText(/I agree to share/))
    await act(async () => {
      if (!resolveFirst) throw new Error('Missing deferred probe')
      resolveFirst({ ok: true, status: 200, data: { key_from_local_settings: false, candidates: [{ ...candidate('http://127.0.0.1:8088'), models: ['stale-model'] }] } })
    })
    expect(select).toHaveValue('http://127.0.0.1:8000')
    expect(screen.queryByRole('option', { name: 'stale-model' })).toBeNull()
    fireEvent.change(select, { target: { value: 'http://127.0.0.1:8088' } })
    expect(screen.getByLabelText('Daily model (required)')).toHaveValue('')
    expect(screen.getByLabelText(/I agree to share/)).not.toBeChecked()
  })
  it('stops polling when unmounted', async () => {
    vi.useFakeTimers(); const view = render(<IdorisPanel />)
    await act(async () => {})
    view.unmount(); const count = proxy.mock.calls.length
    await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
    expect(proxy).toHaveBeenCalledTimes(count)
  })
  it('rejects malformed status rather than treating it as installed', async () => {
    proxy.mockResolvedValueOnce({ ok: true, status: 200, data: { state: 'running' } })
    render(<IdorisPanel />)
    expect(await screen.findByRole('alert')).toHaveTextContent('Cannot read iDoris state.')
    expect(screen.queryByRole('button', { name: 'Stop' })).toBeNull()
  })
  it.each(Object.entries(idorisErrors).flatMap(([code, translations]) => (['en', 'zh', 'th'] as const).map(locale => ({ code, locale, message: translations[locale] }))))('renders $code in $locale from the stable error code', async ({ code, locale, message }) => {
    setLocale(locale)
    proxy.mockResolvedValueOnce({ ok: false, status: 503, data: { error: { code, message: 'DO NOT DISPLAY RAW SERVER TEXT' } } })
    render(<IdorisPanel />)
    expect(await screen.findByRole('alert')).toHaveTextContent(message)
    expect(screen.queryByText('DO NOT DISPLAY RAW SERVER TEXT')).toBeNull()
  })
  it.each([
    ['en', 'instance_conflict', 'Another iDoris instance owns this database. Stop it before retrying.'],
    ['zh', 'keychain_locked', '请解锁钥匙串并允许 Agent24 访问。'],
    ['th', 'omlx_auth_required', 'oMLX ต้องการการยืนยันตัวตน โปรดตั้งค่าคีย์ใน oMLX แล้วลองอีกครั้ง'],
  ] as const)('localizes %s / %s instead of displaying daemon messages', async (locale, code, message) => {
    setLocale(locale); status = { ...running, last_error: code }; render(<IdorisPanel />)
    expect(await screen.findByRole('alert')).toHaveTextContent(message)
    expect(screen.queryByText('DO NOT DISPLAY RAW SERVER TEXT')).toBeNull()
  })
})
