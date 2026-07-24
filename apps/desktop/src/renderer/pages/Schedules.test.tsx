// @vitest-environment jsdom
import { describe, it, expect, vi } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import SchedulesPage from './Schedules'

const SCHEDULE = {
  id: 'sch_1',
  name: '每日晨报',
  enabled: true,
  spec: { type: 'cron', expr: '0 8 * * *', tz: 'UTC' },
  action: { type: 'agent_run', prompt: 'digest' },
  delivery: [],
  last_run_at: null,
  next_run_at: '2026-07-25T08:00:00Z',
  consecutive_failures: 0,
}

let calls: Array<{ method: string; path: string; body?: unknown }> = []

function mount(list: unknown[]) {
  calls = []
  const proxy = vi.fn((req: { method: string; path: string; body?: unknown }) => {
    calls.push(req)
    if (req.method === 'GET') return Promise.resolve({ ok: true, status: 200, data: { schedules: list } })
    if (req.path.endsWith('/run_now')) return Promise.resolve({ ok: true, status: 202, data: { run_id: 'run_9' } })
    if (req.method === 'POST') return Promise.resolve({ ok: true, status: 201, data: SCHEDULE })
    return Promise.resolve({ ok: true, status: 200, data: SCHEDULE })
  })
  window.agent24 = { backendProxy: proxy } as never
  return proxy
}

describe('SchedulesPage', () => {
  it('renders a live cron next-fire preview that updates with the expression', async () => {
    mount([])
    render(<SchedulesPage />)
    // default expr "0 8 * * *" → preview shows a UTC time
    const preview = screen.getByTestId('preview')
    expect(preview.textContent).toMatch(/下次触发：\d{4}-\d{2}-\d{2} 08:00:00 UTC/)
    // switch to "every" 3600 → preview changes to +1h from now
    fireEvent.click(screen.getByRole('button', { name: '每隔' }))
    expect(screen.getByTestId('preview').textContent).toMatch(/下次触发：/)
  })

  it('marks a non-UTC cron preview approximate', () => {
    mount([])
    render(<SchedulesPage />)
    fireEvent.change(screen.getByLabelText('时区'), { target: { value: 'Asia/Shanghai' } })
    expect(screen.getByTestId('preview').textContent).toContain('近似')
  })

  it('blocks create when name/prompt are empty', async () => {
    const proxy = mount([])
    render(<SchedulesPage />)
    fireEvent.click(screen.getByRole('button', { name: '创建' }))
    expect(screen.getByText('名称和 prompt 必填')).toBeInTheDocument()
    // no POST issued
    expect(proxy.mock.calls.every((c) => c[0].method !== 'POST')).toBe(true)
  })

  it('creates a schedule with the built spec', async () => {
    mount([])
    render(<SchedulesPage />)
    fireEvent.change(screen.getByLabelText('名称'), { target: { value: '晨报' } })
    fireEvent.change(screen.getByLabelText('prompt'), { target: { value: '抓取 RSS' } })
    fireEvent.click(screen.getByRole('button', { name: '创建' }))
    await waitFor(() =>
      expect(calls).toContainEqual(
        expect.objectContaining({
          method: 'POST',
          path: '/api/v1/schedules',
          body: expect.objectContaining({
            name: '晨报',
            spec: { type: 'cron', expr: '0 8 * * *', tz: 'UTC' },
            action: { type: 'agent_run', prompt: '抓取 RSS' },
          }),
        }),
      ),
    )
  })

  it('at-type preview and invalid-spec create are blocked', async () => {
    const proxy = mount([])
    render(<SchedulesPage />)
    fireEvent.change(screen.getByLabelText('名称'), { target: { value: 'once' } })
    fireEvent.change(screen.getByLabelText('prompt'), { target: { value: 'do it' } })
    fireEvent.click(screen.getByRole('button', { name: '一次性' }))
    // past timestamp → preview error → create blocked
    fireEvent.change(screen.getByLabelText('触发时间'), { target: { value: '2000-01-01T00:00:00Z' } })
    expect(screen.getByTestId('preview').textContent).toContain('已过')
    fireEvent.click(screen.getByRole('button', { name: '创建' }))
    expect(screen.getByText(/调度规格无效/)).toBeInTheDocument()
    expect(proxy.mock.calls.every((c) => c[0].method !== 'POST')).toBe(true)
  })

  it('summarizes every / at / cron spec types in the list', async () => {
    mount([
      { ...SCHEDULE, id: 's1', name: 'A', spec: { type: 'every', secs: 300 } },
      { ...SCHEDULE, id: 's2', name: 'B', spec: { type: 'at', ts: '2026-08-01T00:00:00Z' }, next_run_at: null },
      { ...SCHEDULE, id: 's3', name: 'C', spec: { type: 'cron', expr: '0 8 * * *', tz: null }, consecutive_failures: 2, last_run_at: '2026-07-24T08:00:00Z' },
    ])
    render(<SchedulesPage />)
    await waitFor(() => expect(screen.getByText('A')).toBeInTheDocument())
    expect(screen.getByText('每 300s')).toBeInTheDocument()
    expect(screen.getByText(/一次性 2026-08-01/)).toBeInTheDocument()
    expect(screen.getByText('cron 0 8 * * *')).toBeInTheDocument()
    // failure counter + "no further trigger" branch
    expect(screen.getByText('连续失败 2')).toBeInTheDocument()
    expect(screen.getByText('（无后续触发）')).toBeInTheDocument()
  })

  it('non-Error rejections still surface a message', async () => {
    const proxy = vi.fn((req: { method: string }) =>
      req.method === 'GET'
        ? Promise.resolve({ ok: true, status: 200, data: { schedules: [SCHEDULE] } })
        : Promise.reject('string failure'),
    )
    window.agent24 = { backendProxy: proxy } as never
    render(<SchedulesPage />)
    await waitFor(() => screen.getByText('每日晨报'))
    fireEvent.click(screen.getByRole('button', { name: '删除' }))
    await waitFor(() => expect(screen.getByText('string failure')).toBeInTheDocument())
  })

  it('surfaces a create error from the server', async () => {
    const proxy = vi.fn((req: { method: string }) =>
      req.method === 'GET'
        ? Promise.resolve({ ok: true, status: 200, data: { schedules: [] } })
        : Promise.resolve({ ok: false, status: 400, data: { error: { message: 'server said no' } } }),
    )
    window.agent24 = { backendProxy: proxy } as never
    render(<SchedulesPage />)
    fireEvent.change(screen.getByLabelText('名称'), { target: { value: 'x' } })
    fireEvent.change(screen.getByLabelText('prompt'), { target: { value: 'p' } })
    fireEvent.click(screen.getByRole('button', { name: '创建' }))
    await waitFor(() => expect(screen.getByText('server said no')).toBeInTheDocument())
  })

  it('lists schedules and supports run_now / toggle / delete', async () => {
    mount([SCHEDULE])
    render(<SchedulesPage />)
    await waitFor(() => expect(screen.getByText('每日晨报')).toBeInTheDocument())

    fireEvent.click(screen.getByRole('button', { name: '立即运行' }))
    await waitFor(() =>
      expect(calls).toContainEqual(expect.objectContaining({ path: '/api/v1/schedules/sch_1/run_now' })),
    )
    expect(screen.getByText(/已触发运行 run_9/)).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: '禁用' }))
    await waitFor(() =>
      expect(calls).toContainEqual(
        expect.objectContaining({ method: 'PATCH', path: '/api/v1/schedules/sch_1', body: { enabled: false } }),
      ),
    )

    fireEvent.click(screen.getByRole('button', { name: '删除' }))
    await waitFor(() =>
      expect(calls).toContainEqual(
        expect.objectContaining({ method: 'DELETE', path: '/api/v1/schedules/sch_1' }),
      ),
    )
  })
})
