// @vitest-environment jsdom
import { act, cleanup, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { BackendProxyRequest } from '../../shared/ipc-types'
import type { IdorisStatus } from '../components/idoris/api'
import { setLocale, type Locale } from '../i18n'
import { useCapabilityStatus } from './useCapabilityStatus'

const initial: IdorisStatus = {
  state: 'not_installed', version: null, data_port: null, admin_port: null,
  roles: [], last_error: null, current_operation: null, verification: null,
  budget_suggestion: null, budget_unavailable_reason: 'hardware_memory_unavailable',
}
let status: IdorisStatus
const proxy = vi.fn(async (req: BackendProxyRequest) => {
  if (req.path === '/api/v1/idoris/status') return { ok: true, status: 200, data: status }
  throw new Error('unavailable')
})

beforeEach(() => {
  vi.useFakeTimers()
  setLocale('zh')
  status = initial
  proxy.mockClear()
  Object.defineProperty(window, 'agent24', {
    value: { backendProxy: proxy }, writable: true, configurable: true,
  })
})
afterEach(() => { cleanup(); vi.useRealTimers() })

describe('local model capability status', () => {
  it.each<{ locale: Locale; label: string }>([
    { locale: 'zh', label: 'iDoris · 已连接' },
    { locale: 'en', label: 'iDoris · Connected' },
    { locale: 'th', label: 'iDoris · เชื่อมต่อแล้ว' },
  ])('shows running as connected in $locale through the daemon API', async ({ locale, label }) => {
    setLocale(locale)
    status = { ...initial, state: 'running' }
    const { result } = renderHook(() => useCapabilityStatus(true))
    await act(async () => {})
    expect(proxy).toHaveBeenCalledWith({ method: 'GET', path: '/api/v1/idoris/status' })
    expect(result.current('localModels')).toBe(label)
  })

  it.each<{ locale: Locale; label: string }>([
    { locale: 'zh', label: '未安装' },
    { locale: 'en', label: 'Not installed' },
    { locale: 'th', label: 'ยังไม่ติดตั้ง' },
  ])('shows not installed in $locale', async ({ locale, label }) => {
    setLocale(locale)
    const { result } = renderHook(() => useCapabilityStatus(true))
    await act(async () => {})
    expect(result.current('localModels')).toBe(label)
  })

  it.each<{ state: IdorisStatus['state']; label: string }>([
    { state: 'not_configured', label: '已安装 · 未连接' },
    { state: 'stopped', label: '已停止' },
    { state: 'starting', label: '正在启动' },
    { state: 'failed', label: '故障' },
    { state: 'instance_conflict', label: '实例冲突' },
  ])('shows the daemon state $state', async ({ state, label }) => {
    status = { ...initial, state }
    const { result } = renderHook(() => useCapabilityStatus(true))
    await act(async () => {})
    expect(result.current('localModels')).toBe(label)
  })

  it('hides the status when the request fails', async () => {
    proxy.mockRejectedValueOnce(new Error('daemon offline'))
    const { result } = renderHook(() => useCapabilityStatus(true))
    await act(async () => {})
    expect(proxy).toHaveBeenCalledWith({ method: 'GET', path: '/api/v1/idoris/status' })
    expect(result.current('localModels')).toBeUndefined()
  })

  it('clears the previous status when a later poll fails', async () => {
    status = { ...initial, state: 'running' }
    const { result } = renderHook(() => useCapabilityStatus(true))
    await act(async () => {})
    expect(result.current('localModels')).toBe('iDoris · 已连接')
    proxy.mockRejectedValueOnce(new Error('daemon offline'))
    await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
    expect(result.current('localModels')).toBeUndefined()
  })

  it('does not request status while inactive', async () => {
    const { result } = renderHook(() => useCapabilityStatus(false))
    await act(async () => {})
    expect(proxy).not.toHaveBeenCalled()
    expect(result.current('localModels')).toBeUndefined()
  })
})
