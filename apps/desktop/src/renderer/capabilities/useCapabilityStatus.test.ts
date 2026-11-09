// @vitest-environment jsdom
import { act, cleanup, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { BackendProxyRequest } from '../../shared/ipc-types'
import { COMPONENT_STATES, type ComponentState } from '../../shared/component-state'
import { setLocale, type Locale } from '../i18n'
import { useCapabilityStatus } from './useCapabilityStatus'

interface Row { id: string; version: string | null; state: ComponentState; reason?: string; provides: [] }

let rows: Row[]
let fail = false
const proxy = vi.fn(async (req: BackendProxyRequest): Promise<{ ok: boolean; status: number; data: unknown }> => {
  if (String(req.path).includes('/report')) throw new Error('renderer must not call the report route')
  if (req.path !== '/api/v1/components') throw new Error(`unexpected ${req.path}`)
  if (fail) return { ok: false, status: 503, data: null }
  return { ok: true, status: 200, data: { components: rows } }
})

function row(id: string, state: ComponentState, reason?: string): Row {
  return { id, version: '1', state, ...(reason ? { reason } : {}), provides: [] }
}

beforeEach(() => {
  vi.useFakeTimers()
  setLocale('zh')
  rows = [row('idoris', 'absent'), row('hyphae', 'stopped'), row('agentear', 'stopped')]
  fail = false
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
  ])('shows ready as connected in $locale through /components', async ({ locale, label }) => {
    setLocale(locale)
    rows = [row('idoris', 'ready')]
    const { result } = renderHook(() => useCapabilityStatus(true))
    await act(async () => {})
    expect(proxy).toHaveBeenCalledWith({ method: 'GET', path: '/api/v1/components' })
    expect(result.current('localModels')).toBe(label)
    expect(proxy.mock.calls.every(([req]) => !String(req.path).includes('/report'))).toBe(true)
  })

  it.each<{ locale: Locale; label: string }>([
    { locale: 'zh', label: '未安装' },
    { locale: 'en', label: 'Not installed' },
    { locale: 'th', label: 'ยังไม่ติดตั้ง' },
  ])('shows absent as not installed in $locale', async ({ locale, label }) => {
    setLocale(locale)
    const { result } = renderHook(() => useCapabilityStatus(true))
    await act(async () => {})
    expect(result.current('localModels')).toBe(label)
  })

  it.each<{ state: ComponentState; reason?: string; label: string }>([
    { state: 'installed', reason: 'not_configured', label: '已安装 · 未连接' },
    { state: 'installed', label: '已安装' },
    { state: 'stopped', label: '已停止' },
    { state: 'starting', label: '正在启动' },
    { state: 'failed', label: '故障' },
    { state: 'failed', reason: 'instance_conflict', label: '实例冲突' },
    { state: 'installing', label: '安装中' },
    { state: 'degraded', label: '降级运行' },
    { state: 'stopping', label: '正在停止' },
    { state: 'incompatible', label: '不兼容' },
  ])('shows the component state $state $reason', async ({ state, reason, label }) => {
    rows = [row('idoris', state, reason)]
    const { result } = renderHook(() => useCapabilityStatus(true))
    await act(async () => {})
    expect(result.current('localModels')).toBe(label)
  })

  it.each([
    ['integrity_manifest_missing', '包版本过旧，升级 Sin90/Cos72 后重试'],
    ['hash_mismatch', '文件校验失败，请重装可信版本'],
    ['integrity_manifest_invalid', '安装内容不完整或格式不合法，请重装'],
    ['signature_rejected', '发布签名验证失败，请安装可信版本'],
    ['integrity_check_failed', '无法校验包，请检查访问权限后重试'],
  ])('shows the OS package prompt for %s', async (reason, prompt) => {
    rows = [row('idoris', 'failed', reason)]
    const { result } = renderHook(() => useCapabilityStatus(true))
    await act(async () => {})
    expect(result.current('localModels')).toBe(prompt)
  })

  it('names every component state instead of falling through', async () => {
    for (const state of COMPONENT_STATES) {
      rows = [row('idoris', state)]
      const { result, unmount } = renderHook(() => useCapabilityStatus(true))
      await act(async () => {})
      expect(result.current('localModels'), state).toEqual(expect.any(String))
      expect(result.current('localModels')).not.toBe('无法读取状态')
      unmount()
    }
  })

  it('shows an explicit error when the component list fails', async () => {
    fail = true
    const { result } = renderHook(() => useCapabilityStatus(true))
    await act(async () => {})
    expect(result.current('localModels')).toBe('无法读取状态')
    expect(result.current('communication')).toBe('无法读取状态')
    expect(result.current('voice')).toBe('无法读取状态')
  })

  it('replaces a previous status when a later poll fails', async () => {
    rows = [row('idoris', 'ready')]
    const { result } = renderHook(() => useCapabilityStatus(true))
    await act(async () => {})
    expect(result.current('localModels')).toBe('iDoris · 已连接')
    fail = true
    await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
    expect(result.current('localModels')).toBe('无法读取状态')
  })

  it('shows loading before the list arrives', async () => {
    let release: ((value: { ok: boolean; status: number; data: unknown }) => void) | undefined
    proxy.mockImplementationOnce(() => new Promise((resolve) => { release = resolve }))
    const { result } = renderHook(() => useCapabilityStatus(true))
    await act(async () => {})
    expect(result.current('localModels')).toBe('正在读取状态…')
    await act(async () => { release?.({ ok: true, status: 200, data: { components: rows } }) })
    expect(result.current('localModels')).toBe('未安装')
  })

  it('does not request status while inactive', async () => {
    const { result } = renderHook(() => useCapabilityStatus(false))
    await act(async () => {})
    expect(proxy).not.toHaveBeenCalled()
    expect(result.current('localModels')).toBeUndefined()
  })
})

describe('communication capability status', () => {
  it.each<{ state: ComponentState; reason?: string; label: string }>([
    { state: 'ready', label: '运行中' },
    { state: 'starting', label: '正在启动…' },
    { state: 'stopped', label: '已停止' },
    { state: 'stopped', reason: 'not_configured', label: '未配置' },
    { state: 'failed', reason: 'locked', label: '已锁定' },
    { state: 'stopped', reason: 'locked', label: '已锁定' },
    { state: 'incompatible', reason: 'component_contract_mismatch', label: '不兼容' },
    { state: 'failed', reason: 'gave_up', label: '已停止重试' },
    { state: 'failed', reason: 'backoff', label: '重试中…' },
    { state: 'failed', reason: 'starting', label: '正在启动…' },
    { state: 'failed', label: '故障' },
  ])('shows hyphae $state $reason as $label', async ({ state, reason, label }) => {
    rows = [row('hyphae', state, reason)]
    const { result } = renderHook(() => useCapabilityStatus(true))
    await act(async () => {})
    expect(result.current('communication')).toBe(label)
  })

  it('shows missing when hyphae is absent from the list', async () => {
    rows = []
    const { result } = renderHook(() => useCapabilityStatus(true))
    await act(async () => {})
    expect(result.current('communication')).toBe('未上报')
  })
})

describe('OS package capability status', () => {
  it.each([
    ['sin90', 'integrity_manifest_missing', '包版本过旧，升级 Sin90/Cos72 后重试'],
    ['cos72', 'hash_mismatch', '文件校验失败，请重装可信版本'],
    ['documents', 'integrity_manifest_invalid', '安装内容不完整或格式不合法，请重装'],
    ['fieldnote', 'signature_rejected', '发布签名验证失败，请安装可信版本'],
    ['fieldnote', 'integrity_check_failed', '无法校验包，请检查访问权限后重试'],
  ])('shows the section 4 prompt for refused %s (%s)', async (id, reason, prompt) => {
    rows = [row(id, 'failed', reason)]
    const { result } = renderHook(() => useCapabilityStatus(true))
    await act(async () => {})
    expect(proxy).toHaveBeenCalledWith({ method: 'GET', path: '/api/v1/components' })
    expect(result.current(id)).toBe(prompt)
  })

  it('leaves the default label when the package is not an integrity refusal', async () => {
    rows = [row('sin90', 'failed', 'refused'), row('cos72', 'ready')]
    const { result } = renderHook(() => useCapabilityStatus(true))
    await act(async () => {})
    expect(result.current('sin90')).toBeUndefined()
    expect(result.current('cos72')).toBeUndefined()
  })
})

describe('voice capability status', () => {
  describe.each<{ locale: Locale; labels: string[] }>([
    { locale: 'zh', labels: ['已连接', '已配对但未连接 · 打开 AgentEar', '已停用', '未配对'] },
    { locale: 'en', labels: ['Connected', 'Paired but disconnected · Open AgentEar', 'Disabled', 'Not paired'] },
    { locale: 'th', labels: ['เชื่อมต่อแล้ว', 'จับคู่แล้วแต่ไม่ได้เชื่อมต่อ · เปิด AgentEar', 'ปิดใช้งาน', 'ยังไม่ได้จับคู่'] },
  ])('$locale', ({ locale, labels }) => {
    it.each(['ready', 'stopped', 'disabled', 'missing'] as const)('shows %s for AgentEar only', async (state) => {
      setLocale(locale)
      rows = state === 'missing' ? []
        : state === 'disabled' ? [row('agentear', 'stopped', 'disabled')]
        : [row('agentear', state)]
      const { result } = renderHook(() => useCapabilityStatus(true))
      await act(async () => {})
      expect(proxy).toHaveBeenCalledWith({ method: 'GET', path: '/api/v1/components' })
      expect(result.current('voice')).toBe(labels[['ready', 'stopped', 'disabled', 'missing'].indexOf(state)])
      expect(proxy.mock.calls.every(([req]) => !String(req.path).includes('/report'))).toBe(true)
    })
  })
})
