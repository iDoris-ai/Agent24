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

  // ME4-1.2.2d — desktop forward-compat for the v3 protocol's module schedule
  // rows (design ME4-S1-scheduler-callback.md §8.2, judgement C2.10).

  it('a new-shape user row (owner null) behaves exactly as before: eff === enabled', async () => {
    mount([{ ...SCHEDULE, owner: null, effective_enabled: true, disabled_by: null, user_suspended: false, system_disabled_reason: null }])
    render(<SchedulesPage />)
    await waitFor(() => expect(screen.getByText('每日晨报')).toBeInTheDocument())
    expect(screen.getByRole('button', { name: '禁用' })).toBeInTheDocument()
    // no module-only affordances leak onto a user row
    expect(screen.queryByText(/来自模块/)).not.toBeInTheDocument()
    expect(screen.queryByText(/处理$/)).not.toBeInTheDocument()
  })

  it('old-shape row (no effective_enabled/disabled_by/owner at all) still renders by enabled — forward compat', async () => {
    // SCHEDULE as-is is exactly what a pre-1.2.2 daemon sends: no owner,
    // effective_enabled, disabled_by, user_suspended, system_disabled_reason.
    mount([{ ...SCHEDULE, enabled: true }])
    render(<SchedulesPage />)
    await waitFor(() => expect(screen.getByText('每日晨报')).toBeInTheDocument())
    // Mutation check: reading only `effective_enabled` (undefined here) would
    // be falsy and flip this to '启用' — this assertion goes red under that
    // mutation (§13 1.2.2d).
    expect(screen.getByRole('button', { name: '禁用' })).toBeInTheDocument()
  })

  it('a module row user-suspended by the user shows the reason, module source, and toggles via {enabled:true}', async () => {
    const proxy = mount([
      {
        ...SCHEDULE,
        id: 'sch_mod_1',
        name: '天气播报',
        action: null,
        owner: { module: 'weather', key: 'daily' },
        enabled: true,
        effective_enabled: false,
        disabled_by: 'user',
        user_suspended: true,
        system_disabled_reason: null,
      },
    ])
    render(<SchedulesPage />)
    await waitFor(() => expect(screen.getByText('天气播报')).toBeInTheDocument())
    expect(screen.getByText('已被你暂停')).toBeInTheDocument()
    expect(screen.getByText('来自模块 weather')).toBeInTheDocument()
    expect(screen.getByText('由模块 weather 处理')).toBeInTheDocument()
    // effectively off → button offers to turn it back on
    fireEvent.click(screen.getByRole('button', { name: '启用' }))
    await waitFor(() =>
      expect(calls).toContainEqual(
        expect.objectContaining({ method: 'PATCH', path: '/api/v1/schedules/sch_mod_1', body: { enabled: true } }),
      ),
    )
    expect(proxy).toHaveBeenCalled()
  })

  it('a module row disabled by the system shows the system reason', async () => {
    mount([
      {
        ...SCHEDULE,
        id: 'sch_mod_2',
        name: '库存同步',
        action: null,
        owner: { module: 'inventory', key: 'sync' },
        enabled: true,
        effective_enabled: false,
        disabled_by: 'system',
        user_suspended: false,
        system_disabled_reason: 'consecutive_failures',
      },
    ])
    render(<SchedulesPage />)
    await waitFor(() => expect(screen.getByText('库存同步')).toBeInTheDocument())
    expect(screen.getByText('系统因连续失败禁用')).toBeInTheDocument()
  })

  it('a module row that disabled itself shows the module reason (resume would hit 0 rows, v3.1 L5)', async () => {
    mount([
      {
        ...SCHEDULE,
        id: 'sch_mod_3',
        name: '每周报表',
        action: null,
        owner: { module: 'reports', key: 'weekly' },
        enabled: false,
        effective_enabled: false,
        disabled_by: 'module',
        user_suspended: false,
        system_disabled_reason: null,
      },
    ])
    render(<SchedulesPage />)
    await waitFor(() => expect(screen.getByText('每周报表')).toBeInTheDocument())
    expect(screen.getByText('模块自己停用')).toBeInTheDocument()
  })

  it('a healthy, enabled module row shows the source and "handled by module" line but no reason', async () => {
    mount([
      {
        ...SCHEDULE,
        id: 'sch_mod_4',
        name: '健康检查',
        action: null,
        owner: { module: 'health', key: 'check' },
        enabled: true,
        effective_enabled: true,
        disabled_by: null,
        user_suspended: false,
        system_disabled_reason: null,
      },
    ])
    render(<SchedulesPage />)
    await waitFor(() => expect(screen.getByText('健康检查')).toBeInTheDocument())
    expect(screen.getByText('来自模块 health')).toBeInTheDocument()
    expect(screen.getByText('由模块 health 处理')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: '禁用' })).toBeInTheDocument()
    // no leftover reason text for a row nothing is holding back
    expect(screen.queryByText('已被你暂停')).not.toBeInTheDocument()
    expect(screen.queryByText('系统因连续失败禁用')).not.toBeInTheDocument()
    expect(screen.queryByText('模块自己停用')).not.toBeInTheDocument()
  })

  it('run_now on a module row shows the fire_id notice, not "已触发运行 undefined"', async () => {
    const proxy = vi.fn((req: { method: string; path: string }) => {
      if (req.method === 'GET') {
        return Promise.resolve({
          ok: true,
          status: 200,
          data: {
            schedules: [
              { ...SCHEDULE, id: 'sch_mod_5', action: null, owner: { module: 'weather', key: 'daily' } },
            ],
          },
        })
      }
      if (req.path.endsWith('/run_now')) {
        return Promise.resolve({ ok: true, status: 202, data: { fire_id: 'fire_xyz789' } })
      }
      return Promise.resolve({ ok: true, status: 200, data: SCHEDULE })
    })
    window.agent24 = { backendProxy: proxy } as never
    render(<SchedulesPage />)
    await waitFor(() => expect(screen.getByText('每日晨报')).toBeInTheDocument())
    fireEvent.click(screen.getByRole('button', { name: '立即运行' }))
    await waitFor(() => expect(screen.getByText('已投递给模块（fire_id fire_xyz789）')).toBeInTheDocument())
  })
})
