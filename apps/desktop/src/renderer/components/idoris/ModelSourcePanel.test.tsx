// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { BackendProxyRequest } from '../../../shared/ipc-types'
import { setLocale } from '../../i18n'
import ModelSourcePanel from './ModelSourcePanel'

const status = {
  state: 'not_configured', version: null, data_port: null, admin_port: null,
  roles: [], last_error: null, current_operation: null, verification: null,
  budget_suggestion: { core_reserved_bytes: 123, idoris_budget_bytes: 456 }, budget_unavailable_reason: null,
}
const operation = { operation_id: 'op', action: 'connect', phase: 'queued', result: null, error_code: null, cancellable: false }
const endpoint = 'http://localhost'
const candidate = { endpoint, identity: 'omlx', authentication: 'ready', models: ['bge-embed', 'Qwen-chat', 'Other-chat'] }
const proxy = vi.fn()
let currentStatus: object
let result: object
let candidates: object[]

beforeEach(() => {
  setLocale('zh')
  currentStatus = status; result = { ...operation, phase: 'completed', result: 'succeeded' }; candidates = [candidate]
  proxy.mockReset()
  proxy.mockImplementation(async (request: BackendProxyRequest) => {
    if (request.path.endsWith('/status')) return { ok: true, status: 200, data: currentStatus }
    if (request.path.includes('/upstream-models?')) return { ok: true, status: 200, data: { models: ['First-chat', 'Second-chat'] } }
    if (request.path.endsWith('/candidates')) return { ok: true, status: 200, data: { candidates, key_from_local_settings: true } }
    if (request.path.endsWith('/connect')) return { ok: true, status: 202, data: operation }
    if (request.path.endsWith('/operations/op')) return { ok: true, status: 200, data: result }
    throw new Error(`Unexpected route: ${request.path}`)
  })
  window.agent24 = { backendProxy: proxy } as never
})
afterEach(() => { cleanup(); vi.useRealTimers() })

async function ready() {
  const view = render(<ModelSourcePanel />)
  await waitFor(() => expect(screen.getByRole('radio', { name: '本机 oMLX（推荐）' })).toBeEnabled())
  return view
}
async function readModels() {
  fireEvent.click(screen.getByRole('radio', { name: 'LM Studio 等 OpenAI 兼容服务' }))
  fireEvent.change(screen.getByPlaceholderText('本机服务地址，例如 LM Studio 显示的地址'), { target: { value: endpoint } })
  fireEvent.click(screen.getByRole('button', { name: '读取模型' }))
  await screen.findByRole('option', { name: 'First-chat' })
}

describe('模型来源', () => {
  it.each([['en', 'Model source'], ['th', 'แหล่งโมเดล']] as const)('renders the %s translation', async (locale, title) => {
    setLocale(locale)
    render(<ModelSourcePanel />)
    expect(screen.getByRole('heading', { name: title })).toBeInTheDocument()
    await waitFor(() => expect(screen.getByRole('button', { name: locale === 'en' ? 'Apply' : 'นำไปใช้' })).toBeEnabled())
  })
  it('defaults to oMLX, loads upstream models through the proxy and waits for connect success', async () => {
    await ready()
    expect(screen.getByRole('radio', { name: '本机 oMLX（推荐）' })).toBeChecked()
    expect(screen.getByText('对话只经 iDoris；这里配置的是 iDoris 的上游。LM Studio 暂不支持向量召回。')).toBeInTheDocument()
    await readModels()
    expect(proxy).toHaveBeenCalledWith({ method: 'GET', path: `/api/v1/idoris/upstream-models?endpoint=${encodeURIComponent(endpoint)}` })
    fireEvent.change(screen.getByRole('combobox', { name: '日常对话模型' }), { target: { value: 'Second-chat' } })
    fireEvent.click(screen.getByRole('button', { name: '应用' }))
    expect(await screen.findByRole('status')).toHaveTextContent('正在应用')
    expect(screen.queryByText('已切换')).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: '读取模型' })).toBeDisabled()
    await waitFor(() => expect(proxy).toHaveBeenCalledWith(expect.objectContaining({
      method: 'POST', path: '/api/v1/idoris/connect', idempotencyKey: expect.any(String),
      body: expect.objectContaining({ source: 'openai_compat', endpoint, daily_model: 'Second-chat', replace: false, core_reserved_bytes: 123 }),
    })))
    expect(await screen.findByText('已切换', {}, { timeout: 3000 })).toBeInTheDocument()
    expect(proxy.mock.calls.filter(([request]) => request.path.endsWith('/connect'))).toHaveLength(1)
  })

  it.each([{ source: 'openai_compat' }, { connection: { source: 'openai_compat' } }])('reads current source from status %j', async extra => {
    currentStatus = { ...status, ...extra, state: 'stopped', roles: ['daily'] }
    await ready()
    expect(screen.getByRole('radio', { name: 'LM Studio 等 OpenAI 兼容服务' })).toBeChecked()
    fireEvent.click(screen.getByRole('radio', { name: '本机 oMLX（推荐）' }))
    fireEvent.click(screen.getByRole('button', { name: '应用' }))
    expect(await screen.findByText('已切换', {}, { timeout: 3000 })).toBeInTheDocument()
    const body = proxy.mock.calls.find(([request]) => request.path.endsWith('/connect'))?.[0].body
    expect(body).toMatchObject({ source: 'omlx', endpoint, daily_model: 'Qwen-chat', replace: true })
    expect(body).not.toHaveProperty('core_reserved_bytes')
  })

  it('uses the first ready oMLX candidate and rejects discovery without a chat model', async () => {
    candidates = [{ ...candidate, identity: 'unknown_service' }, { ...candidate, authentication: 'auth_required' }, candidate]
    await ready()
    fireEvent.click(screen.getByRole('button', { name: '应用' }))
    expect(await screen.findByText('已切换', {}, { timeout: 3000 })).toBeInTheDocument()
    expect(proxy).toHaveBeenCalledWith(expect.objectContaining({ body: expect.objectContaining({ source: 'omlx', daily_model: 'Qwen-chat', core_reserved_bytes: 123 }) }))
    candidates = [{ ...candidate, models: ['bge-embed'] }]
    fireEvent.click(screen.getByRole('button', { name: '应用' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('未检测到本机 oMLX')
  })

  it.each([['model_not_found', '模型不存在'], ['future_error', 'future_error']])('shows terminal error %s', async (code, message) => {
    result = { ...operation, phase: 'completed', result: 'failed', error_code: code }
    await ready()
    fireEvent.click(screen.getByRole('button', { name: '应用' }))
    expect(await screen.findByRole('alert', {}, { timeout: 3000 })).toHaveTextContent(message)
    expect(screen.queryByText('已切换')).not.toBeInTheDocument()
  })

  it('does not reuse model IDs after the endpoint changes', async () => {
    await ready(); await readModels()
    fireEvent.change(screen.getByRole('textbox'), { target: { value: 'http://127.0.0.1' } })
    expect(screen.queryByRole('option', { name: 'First-chat' })).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: '应用' })).toBeDisabled()
  })

  it('does not submit a first connection without a budget suggestion', async () => {
    currentStatus = { ...status, budget_suggestion: null }
    await ready()
    fireEvent.click(screen.getByRole('button', { name: '应用' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('无法获取内存预算')
    expect(proxy.mock.calls.some(([request]) => request.path.endsWith('/connect'))).toBe(false)
  })

  it('keeps polling nonterminal operations, blocks duplicates, and stops after unmount', async () => {
    result = operation
    const view = await ready()
    vi.useFakeTimers()
    await act(async () => { fireEvent.click(screen.getByRole('button', { name: '应用' })) })
    expect(screen.getByRole('button', { name: '应用' })).toBeDisabled()
    await act(async () => { await vi.advanceTimersByTimeAsync(1000) })
    await act(async () => { await vi.advanceTimersByTimeAsync(1000) })
    expect(proxy.mock.calls.filter(([request]) => request.path.endsWith('/operations/op')).length).toBeGreaterThan(1)
    view.unmount()
    const count = proxy.mock.calls.length
    await act(async () => { await vi.advanceTimersByTimeAsync(5000) })
    expect(proxy).toHaveBeenCalledTimes(count)
    expect(proxy.mock.calls.filter(([request]) => request.path.endsWith('/connect'))).toHaveLength(1)
  })
})
