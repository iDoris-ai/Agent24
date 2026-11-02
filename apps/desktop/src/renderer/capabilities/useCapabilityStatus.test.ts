// @vitest-environment jsdom
import { act, cleanup, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { BackendProxyRequest } from '../../shared/ipc-types'
import type { IdorisStatus } from '../components/idoris/api'
import type { AttachedView } from '../pages/voice/api'
import { setLocale, type Locale } from '../i18n'
import { useCapabilityStatus } from './useCapabilityStatus'

const initial: IdorisStatus = {
  state: 'not_installed', version: null, data_port: null, admin_port: null,
  roles: [], last_error: null, migration_pending: false, last_migration_error: null, current_operation: null, verification: null,
  budget_suggestion: null, budget_unavailable_reason: 'hardware_memory_unavailable',
}
let status: IdorisStatus
let modules: AttachedView[]
let attachmentFails: boolean
const proxy = vi.fn(async (req: BackendProxyRequest) => {
  if (req.path === '/api/v1/idoris/status') return { ok: true, status: 200, data: status }
  if (req.path === '/api/v1/attached' && !attachmentFails) return { ok: true, status: 200, data: { modules } }
  throw new Error('unavailable')
})

beforeEach(() => {
  vi.useFakeTimers()
  setLocale('zh')
  status = initial
  modules = []
  attachmentFails = false
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

describe('voice capability status', () => {
  const attachment: AttachedView = {
    name: 'agentear', manifest_digest: 'sha256:test', token_id: 'tok_1',
    created_at: '2026-09-27T00:00:00Z', attach_status: 'attached',
  }
  describe.each<{ locale: Locale; labels: string[] }>([
    { locale: 'zh', labels: ['已连接', '已配对但未连接 · 打开 AgentEar', '已停用', '未配对'] },
    { locale: 'en', labels: ['Connected', 'Paired but disconnected · Open AgentEar', 'Disabled', 'Not paired'] },
    { locale: 'th', labels: ['เชื่อมต่อแล้ว', 'จับคู่แล้วแต่ไม่ได้เชื่อมต่อ · เปิด AgentEar', 'ปิดใช้งาน', 'ยังไม่ได้จับคู่'] },
  ])('$locale', ({ locale, labels }) => {
    it.each(['attached', 'detached', 'disabled', 'unpaired'])('shows %s for AgentEar only', async (state) => {
      setLocale(locale)
      modules = [{ ...attachment, name: 'another-module' }]
      if (state !== 'unpaired') modules.push({ ...attachment, attach_status: state })
      const { result } = renderHook(() => useCapabilityStatus(true))
      await act(async () => {})
      expect(proxy).toHaveBeenCalledWith({ method: 'GET', path: '/api/v1/attached' })
      expect(result.current('voice')).toBe(labels[['attached', 'detached', 'disabled', 'unpaired'].indexOf(state)])
    })
  })

  it.each([false, true])('hides status on request failure (previous success: %s)', async (previousSuccess) => {
    modules = [attachment]
    attachmentFails = !previousSuccess
    const { result } = renderHook(() => useCapabilityStatus(true))
    await act(async () => {})
    if (previousSuccess) {
      expect(result.current('voice')).toBe('已连接')
      attachmentFails = true
      await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
    }
    expect(result.current('voice')).toBeUndefined()
  })
})
