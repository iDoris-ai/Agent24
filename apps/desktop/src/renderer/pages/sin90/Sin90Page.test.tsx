// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { BackendProxyRequest } from '../../../shared/ipc-types'
import { setLocale } from '../../i18n'
import { Sin90Page } from './Sin90Page'
import { isoWeekUTC } from './model'

let areas: unknown[] = []
let directions: unknown[] = []
let tasks: unknown[] = []
let today: unknown = { must_do: [], deep_block: null, inbox: [], carry_over_candidates: [] }
let weeks: unknown[] = []
let blocks: unknown[] = []
let proposals: unknown[] = []
let reviews: unknown[] = []
let periodPlans: unknown | null = null
let failToday = false
const getPaths: string[] = []
const sin90Write = vi.fn(async (_body: Record<string, unknown>) => ({ ok: true, status: 200, data: {} }))
function task(id: string, title: string, status = 'in_progress') { return { id, title, status, direction_id: 'd1', week_id: null } }

beforeEach(() => {
  setLocale('zh'); areas = []; directions = []; tasks = []; weeks = []; blocks = []; proposals = []; reviews = []; periodPlans = null; failToday = false; getPaths.length = 0
  today = { must_do: [], deep_block: null, inbox: [], carry_over_candidates: [] }
  sin90Write.mockReset(); sin90Write.mockResolvedValue({ ok: true, status: 200, data: {} })
  Object.defineProperty(window, 'agent24', { configurable: true, value: {
    sin90Write, sin90KeyStatus: async () => ({ available: true }),
    onSin90Event: () => () => undefined,
    backendProxy: async (req: BackendProxyRequest) => {
      getPaths.push(req.path)
      if (req.path.endsWith('/today')) return failToday ? { ok: false, status: 500, data: { error: { message: '不可用' } } } : { ok: true, status: 200, data: today }
      if (req.path.endsWith('/areas')) return { ok: true, status: 200, data: { areas } }
      if (req.path.endsWith('/directions')) return { ok: true, status: 200, data: { directions } }
      if (req.path.endsWith('/weeks')) return { ok: true, status: 200, data: { weeks } }
      if (req.path.endsWith('/schedule-blocks')) return { ok: true, status: 200, data: { blocks } }
      if (req.path.endsWith('/routines')) return { ok: true, status: 200, data: { routines: [] } }
      if (req.path.endsWith('/rhythms')) return { ok: true, status: 200, data: { rhythms: [] } }
      if (req.path.endsWith('/proposals')) return { ok: true, status: 200, data: { proposals } }
      if (req.path.endsWith('/reviews')) return { ok: true, status: 200, data: { reviews } }
      if (req.path.includes('/tasks?')) return { ok: true, status: 200, data: { tasks } }
      if (req.path.includes('/attention')) return { ok: true, status: 200, data: { planned_min: 120, actual_min: 60, deviation_min: -60 } }
      if (req.path.endsWith('/period-plans')) return periodPlans ? { ok: true, status: 200, data: periodPlans } : { ok: false, status: 404, data: {} }
      throw new Error(req.path)
    },
  } })
})
afterEach(() => { cleanup(); setLocale('en') })

describe('Sin90 three layers', () => {
  it('starts on Today and switches among the three tabs', async () => {
    render(<Sin90Page />)
    await screen.findByRole('tab', { name: '今天' })
    expect(screen.getByRole('tab', { name: '今天' })).toHaveAttribute('aria-selected', 'true')
    fireEvent.click(screen.getByRole('tab', { name: '方向' }))
    expect(await screen.findByText('五个人生系统')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('tab', { name: '节奏' }))
    expect(await screen.findByText('年 → 半年 → 季 → 月 的拆分将在 Sin90 v0.6 提供')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('tab', { name: '今天' }))
    expect(await screen.findByLabelText(/随手记/)).toBeInTheDocument()
  })

  it('opens the separate review view from the review button', async () => {
    render(<Sin90Page />)
    await screen.findByRole('button', { name: '复盘' })
    fireEvent.click(screen.getByRole('button', { name: '复盘' }))
    expect(await screen.findByText('最近的复盘')).toBeInTheDocument()
    expect(screen.getByText('每周日')).toBeInTheDocument()
    expect(screen.getByRole('tab', { name: '今天' })).toHaveAttribute('aria-selected', 'false')
    expect(screen.getByRole('tab', { name: '方向' })).toHaveAttribute('aria-selected', 'false')
    expect(screen.getByRole('button', { name: '复盘' })).toHaveClass('ui-button--primary')
    fireEvent.click(screen.getByRole('tab', { name: '方向' }))
    expect(await screen.findByText('五个人生系统')).toBeInTheDocument()
  })

  it('captures a quick note and completes a must-do item using the legal transition', async () => {
    today = { must_do: [task('t1', '写完方案')], deep_block: null, inbox: [], carry_over_candidates: [] }
    render(<Sin90Page />)
    fireEvent.change(await screen.findByLabelText(/随手记/), { target: { value: '给同事回信' } })
    fireEvent.click(screen.getByRole('button', { name: '记下' }))
    await waitFor(() => expect(sin90Write).toHaveBeenCalledWith({ op: 'capture', text: '给同事回信' }))
    fireEvent.click(within(screen.getByRole('listitem', { name: '写完方案' })).getByLabelText('完成'))
    await waitFor(() => expect(sin90Write).toHaveBeenCalledWith({ op: 'transitionTask', id: 't1', to: 'done' }))
  })

  it('uses clear placeholders where the server has no planning data', async () => {
    areas = [{ id: 'a1', title: '健康', status: 'active', sort_key: 1 }]
    directions = [{ id: 'd1', area_id: 'a1', title: '多走路', status: 'active', target_window: 'ongoing' }]
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('tab', { name: '方向' }))
    expect(await screen.findByText('现在 → 理想、三年目标将在 Sin90 v0.6 提供。')).toBeInTheDocument()
    expect(screen.queryByText(/半马|40 周|300/)).not.toBeInTheDocument()
    fireEvent.click(screen.getByRole('tab', { name: '节奏' }))
    expect(await screen.findByText('年 → 半年 → 季 → 月 的拆分将在 Sin90 v0.6 提供')).toBeInTheDocument()
  })

  it('shows a three-year goal when the directions response contains one', async () => {
    areas = [{ id: 'a1', title: '健康', status: 'active', sort_key: 1 }]
    directions = [{ id: 'd1', area_id: 'a1', title: '多走路', status: 'active', target_window: 'ongoing', goal_3y: '每年走 1000 公里' }]
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('tab', { name: '方向' }))
    expect(await screen.findByText('每年走 1000 公里')).toBeInTheDocument()
  })

  it('offers to install the five systems when areas are empty', async () => {
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('tab', { name: '方向' }))
    expect(await screen.findByText('还没有人生系统')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: /装上五个系统/ }))
    await waitFor(() => expect(sin90Write).toHaveBeenCalledWith({ op: 'installPack' }))
  })

  it('hides the seed triage direction and translates queued block status', async () => {
    const weekLabel = isoWeekUTC(new Date())
    areas = [{ id: 'a1', title: '工作', status: 'active', sort_key: 1 }]
    directions = [{ id: 'd1', area_id: 'a1', title: '交付', status: 'active', target_window: 'ongoing' }, { id: 'sin90-triage', area_id: null, title: '待定', status: 'active', target_window: 'n/a' }]
    tasks = [task('t1', '写方案')]
    weeks = [{ id: 'w1', status: 'planning', iso_week: weekLabel }]
    blocks = [{ id: 'b1', direction_id: 'd1', task_id: 't1', status: 'planned', planned_minutes: 40 }]
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('tab', { name: '方向' }))
    expect(await screen.findByText('交付')).toBeInTheDocument()
    expect(screen.queryByText('待定')).not.toBeInTheDocument()
    fireEvent.click(screen.getByRole('tab', { name: '节奏' }))
    expect(await screen.findByText('排队中')).toBeInTheDocument()
    expect(screen.queryByText('planned')).not.toBeInTheDocument()
  })

  it('uses ErrorNotice when loading fails', async () => {
    failToday = true
    render(<Sin90Page />)
    expect(await screen.findByRole('alert')).toHaveTextContent('不可用')
    expect(screen.getByRole('button', { name: '重试' })).toBeInTheDocument()
  })
})
