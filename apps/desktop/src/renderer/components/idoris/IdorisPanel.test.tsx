// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { BackendProxyRequest } from '../../../shared/ipc-types'
import { setLocale } from '../../i18n'
import IdorisPanel from './IdorisPanel'

let requests: BackendProxyRequest[]
const status = { state: 'running', version: '0.4.0', data_port: 8740, admin_port: 8741, roles: ['daily'], last_error: null, migration_pending: false, last_migration_error: null, current_operation: null, verification: null, budget_suggestion: null, budget_unavailable_reason: null }
const engine = { id: 'omlx', engine_kind: 'omlx', state: 'ready', managed: true, owned: true }
let engineResponse: { ok: boolean; status: number; data: unknown }
const proxy = vi.fn(async (request: BackendProxyRequest) => {
  requests.push(request)
  if (request.path.endsWith('/engines')) return engineResponse
  if (request.path.endsWith('/status')) return { ok: true, status: 200, data: status }
  if (request.path.startsWith('/api/v1/components')) return { ok: true, status: 200, data: { components: [{ id: 'idoris', version: '0.4.0', state: 'ready', form: 'resident', provides: [] }] } }
  return { ok: true, status: 202, data: { operation_id: 'op-1', action: 'start', phase: 'queued', result: null, error_code: null, cancellable: true } }
})

beforeEach(() => { requests = []; engineResponse = { ok: true, status: 200, data: { engines: [engine] } }; setLocale('en'); proxy.mockClear(); window.agent24 = { backendProxy: proxy } as never })
afterEach(cleanup)

describe('IdorisPanel core status', () => {
  it('shows engine states and reasons and replaces stale data on refresh', async () => {
    const external = { ...engine, id: 'external', state: 'failed', reason: 'connection refused', managed: false, owned: false }
    engineResponse = { ok: true, status: 200, data: { engines: [engine, external] } }
    render(<IdorisPanel />)
    const section = within(await screen.findByRole('region', { name: 'Engines' }))
    expect(await section.findByText('Ready')).toHaveAttribute('data-state', 'ready')
    expect(section.getByText('omlx')).toBeInTheDocument()
    expect(section.getByText('external')).toBeInTheDocument()
    expect(section.getByText('Failed')).toHaveAttribute('data-state', 'failed')
    expect(section.getByText('connection refused')).toBeInTheDocument()
    expect(section.queryByRole('button')).toBeNull()

    engineResponse = { ok: true, status: 200, data: { engines: [{ ...engine, state: 'restarting' }] } }
    fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))
    expect(await section.findByText('Restarting')).toHaveAttribute('data-state', 'restarting')
    expect(section.queryByText('connection refused')).toBeNull()

    engineResponse = { ok: false, status: 503, data: { error: { code: 'idoris_unavailable' } } }
    fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))
    expect(await section.findByText('Engine status is unavailable.')).toBeInTheDocument()
    expect(section.queryByText('Restarting')).toBeNull()
    expect(screen.getByTestId('idoris-state')).toHaveTextContent('Running')
  })
  it('shows read-only status and reconnects through the existing start operation', async () => {
    render(<IdorisPanel />)
    expect(await screen.findByTestId('idoris-state')).toHaveTextContent('Running')
    expect(screen.getByText('Version: 0.4.0')).toBeInTheDocument()
    expect(screen.getByText(/Data port: 8740/)).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /install/i })).not.toBeInTheDocument()
    expect(screen.queryByLabelText('Daily model (required)')).not.toBeInTheDocument()
    expect(screen.queryByLabelText('oMLX endpoint')).not.toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }))
    await waitFor(() => expect(requests.some(request => request.path.endsWith('/start'))).toBe(true))
  })
})
