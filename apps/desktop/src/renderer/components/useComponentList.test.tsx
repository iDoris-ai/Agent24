// @vitest-environment jsdom
import { act, cleanup, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { COMPONENT_STATES } from '../../shared/component-state'
import type { BackendProxyRequest } from '../../shared/ipc-types'
import { useComponentList } from './useComponentList'

let rows: unknown[] = []
let fail = false
const proxy = vi.fn(async (req: BackendProxyRequest): Promise<{ ok: boolean; status: number; data: unknown }> => {
  if (fail) return { ok: false, status: 503, data: null }
  if (req.method !== 'GET' || req.path !== '/api/v1/components') throw new Error(`unexpected ${req.method} ${req.path}`)
  return { ok: true, status: 200, data: { components: rows } }
})

function view(state: string, id = 'idoris') {
  return { id, version: '1', state, form: 'resident', provides: [{ id: 'localModels' }] }
}

beforeEach(() => {
  vi.useFakeTimers()
  rows = []
  fail = false
  proxy.mockClear()
  Object.defineProperty(window, 'agent24', { value: { backendProxy: proxy }, writable: true, configurable: true })
})
afterEach(() => { cleanup(); vi.useRealTimers() })

describe('useComponentList', () => {
  it('stays idle and does not fetch while inactive', async () => {
    const { result } = renderHook(() => useComponentList(false))
    await act(async () => {})
    expect(result.current.status).toBe('idle')
    expect(proxy).not.toHaveBeenCalled()
  })

  it('shows loading until the list arrives, then each component state', async () => {
    let release: ((value: { ok: boolean; status: number; data: unknown }) => void) | undefined
    proxy.mockImplementationOnce(() => new Promise((resolve) => { release = resolve }))
    const { result } = renderHook(() => useComponentList(true))
    await act(async () => {})
    expect(result.current.status).toBe('loading')
    rows = COMPONENT_STATES.map((state) => view(state, state))
    await act(async () => { release?.({ ok: true, status: 200, data: { components: rows } }) })
    expect(result.current.status).toBe('ready')
    if (result.current.status !== 'ready') return
    expect(result.current.components.map((item) => item.state)).toEqual([...COMPONENT_STATES])
    expect(proxy.mock.calls.every(([req]) => req.method === 'GET' && !String(req.path).includes('/report'))).toBe(true)
  })

  it('shows an error when the read fails and does not keep a stale ready list', async () => {
    rows = [view('ready')]
    const { result } = renderHook(() => useComponentList(true))
    await act(async () => {})
    expect(result.current).toMatchObject({ status: 'ready' })
    fail = true
    await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
    expect(result.current.status).toBe('error')
  })

  it('rejects a body that is not a component list', async () => {
    proxy.mockResolvedValueOnce({ ok: true, status: 200, data: { relays: [] } })
    const { result } = renderHook(() => useComponentList(true))
    await act(async () => {})
    expect(result.current.status).toBe('error')
  })
})
