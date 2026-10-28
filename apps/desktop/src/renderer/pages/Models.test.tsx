// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import ModelsPage from './Models'
import { setLocale } from '../i18n'

describe('ModelsPage', () => {
  beforeEach(() => { setLocale('zh') })
  beforeEach(() => {
    window.agent24 = {
      backendProxy: vi.fn().mockResolvedValue({ ok: false, status: 503, data: null }),
      idorisStatus: vi.fn().mockResolvedValue({ state: 'unavailable', reason: 'handoff-missing' }),
    } as never
  })

  it('renders model list title', () => {
    render(<ModelsPage />)
    expect(screen.getByText('模型管理')).toBeInTheDocument()
  })

  it('renders all model entries', () => {
    render(<ModelsPage />)
    expect(screen.getByText('Qwen3-30B-A3B')).toBeInTheDocument()
    expect(screen.getByText('bge-m3')).toBeInTheDocument()
    expect(screen.getByText('FLUX.1-schnell')).toBeInTheDocument()
  })

  it('renders download button', () => {
    render(<ModelsPage />)
    expect(screen.getByRole('button', { name: '+ 下载模型' })).toBeInTheDocument()
  })

  it('renders model badges', () => {
    render(<ModelsPage />)
    const badges = screen.getAllByText('常驻')
    expect(badges.length).toBeGreaterThan(0)
    const ondemand = screen.getAllByText('按需')
    expect(ondemand.length).toBeGreaterThan(0)
  })

  // AUDIT-1: requests must hit the daemon's real route, `/api/v1/models`
  // (`rust/apps/agent24d/src/routes.rs` `get_models`) — not the nonexistent
  // `/api/llm/models` that always 404'd and made this page claim "oMLX 未运行"
  // even while oMLX was running.
  it('requests /api/v1/models, not /api/llm/models', () => {
    render(<ModelsPage />)
    expect(window.agent24.backendProxy).toHaveBeenCalledWith({ method: 'GET', path: '/api/v1/models' })
  })

  it('renders the live oMLX model list from the { models: [...] } envelope', async () => {
    window.agent24 = {
      backendProxy: vi.fn().mockResolvedValue({
        ok: true,
        status: 200,
        data: {
          models: [
            { id: 'Qwen3-8B-4bit', provider: 'omlx', tier: 'local', loaded: true },
            { id: 'Qwen3-0.6B-4bit', provider: 'omlx', tier: 'local', loaded: false },
          ],
          default_model: 'Qwen3-8B-4bit',
        },
      }),
      idorisStatus: vi.fn().mockResolvedValue({ state: 'unavailable', reason: 'handoff-missing' }),
    } as never

    render(<ModelsPage />)

    await waitFor(() => {
      expect(screen.getByText('Qwen3-8B-4bit')).toBeInTheDocument()
    })
    expect(screen.getByText('Qwen3-0.6B-4bit')).toBeInTheDocument()
    expect(screen.getByText('已加载')).toBeInTheDocument()
    expect(screen.getByText('未加载')).toBeInTheDocument()
    expect(screen.queryByText('oMLX 未运行 — 启动后此处显示实时模型状态')).not.toBeInTheDocument()
  })

  it('iDoris never configured (handoff-missing): neutral optional note, not "disconnected"', async () => {
    render(<ModelsPage />)
    const note = await screen.findByTestId('idoris-not-configured')
    expect(note).toHaveTextContent('未连接 iDoris（可选）')
    expect(screen.queryByText(/Admin session 未连接/)).toBeNull()
    expect(screen.queryByTestId('idoris-disconnected')).toBeNull()
  })

  it.each(['unreachable', 'http-error', 'handoff-invalid', 'invalid-response', 'invalid-config'] as const)(
    'iDoris configured but %s: still shown as disconnected',
    async (reason) => {
      window.agent24 = {
        backendProxy: vi.fn().mockResolvedValue({ ok: false, status: 503, data: null }),
        idorisStatus: vi.fn().mockResolvedValue({ state: 'unavailable', reason }),
      } as never
      render(<ModelsPage />)
      expect(await screen.findByTestId('idoris-disconnected')).toHaveTextContent('状态不可用 — Admin session 未连接')
      expect(screen.queryByTestId('idoris-not-configured')).toBeNull()
    },
  )

  it('renders the frozen iDoris Admin v0 status without exposing credentials', async () => {
    window.agent24 = {
      backendProxy: vi.fn().mockResolvedValue({ ok: false, status: 503, data: null }),
      idorisStatus: vi.fn().mockResolvedValue({
        state: 'available',
        status: {
          status: 'ok', service: 'idoris', version: '1.2.3', contract_version: 'admin-v0', instance_id: 'instance-1',
          components: 4, runtimes: 2, subscriptions: 0, budget_configured: true, audit_configured: true,
          capacity: { state: 'observed', entries: [] },
        },
        backends: [
          { provider_id: 'omlx-local', locality: 'loopback', form: 'http_service', lifecycle_runtime_bound: true },
          { provider_id: 'llama-local', locality: 'loopback', form: 'spawn_cli', lifecycle_runtime_bound: true },
        ],
        models: { sources: [
          { provider_id: 'omlx-local', source: 'http_models_endpoint', state: 'observed', models: ['qwen3-8b', 'bge-m3'] },
          { provider_id: 'subscription', source: 'subscription_registration', state: 'configured', models: ['remote-coding'] },
        ] },
      }),
    } as never
    render(<ModelsPage />)
    expect(await screen.findByTestId('idoris-status-available')).toHaveTextContent('v1.2.3 · 4 个组件 · 2 个运行时 · 容量 已观测')
    expect(screen.getByTestId('idoris-status-resources')).toHaveTextContent('2 个后端 · 3 个模型')
  })
})

describe('ModelsPage English default', () => {
  it('shows the English page title', () => {
    window.agent24 = {
      backendProxy: vi.fn().mockResolvedValue({ ok: false, status: 503, data: null }),
      idorisStatus: vi.fn().mockResolvedValue({ state: 'unavailable', reason: 'handoff-missing' }),
    } as never
    render(<ModelsPage />)
    expect(screen.getByText('Model management')).toBeInTheDocument()
  })
})
