// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { BackendProxyRequest } from '../../../shared/ipc-types'
import { setLocale } from '../../i18n'
import { Sin90Page } from './Sin90Page'
import { isoWeekUTC } from './model'
import { legalPlanParents, readPeriodPlans } from './model'

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
      if (req.path.includes('/progress?')) return { ok: true, status: 200, data: { progress: [{ plan_id: 'p1', actual: 1, expected: 5, status: 'at_risk', miss_streak: 3 }] } }
      if (req.path.endsWith('/settings/review')) return { ok: true, status: 200, data: { auto: true, cadence: 'weekly', weekday: 7, hour: 18 } }
      if (req.path.includes('/tasks?')) return { ok: true, status: 200, data: { tasks } }
      if (req.path.includes('/attention')) return { ok: true, status: 200, data: { planned_min: 120, actual_min: 60, deviation_min: -60 } }
      if (req.path.endsWith('/period-plans')) return periodPlans ? { ok: true, status: 200, data: periodPlans } : { ok: false, status: 404, data: {} }
      throw new Error(req.path)
    },
  } })
})
afterEach(() => { cleanup(); setLocale('en') })

describe('Sin90 writes', () => {
  it('accepts all classify suggestions one after another', async () => {
    today = { must_do: [], deep_block: null, inbox: [task('t9', '约牙医', 'backlog')], carry_over_candidates: [] }
    proposals = ['p1', 'p2'].map((id) => ({ id, status: 'pending', source: 'local_brain', ops: [{ op: 'assign_task_direction', task_id: 't9', direction_id: 'd1' }] }))
    let inFlight = 0
    let maxInFlight = 0
    sin90Write.mockImplementation(async () => {
      inFlight += 1; maxInFlight = Math.max(maxInFlight, inFlight)
      await new Promise((resolve) => setTimeout(resolve, 5))
      inFlight -= 1
      return { ok: true, status: 200, data: {} }
    })
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('button', { name: '全部采纳' }))
    await waitFor(() => expect(sin90Write).toHaveBeenCalledTimes(2))
    expect(sin90Write.mock.calls.map((call) => call[0])).toEqual([
      { op: 'acceptProposal', id: 'p1' },
      { op: 'acceptProposal', id: 'p2' },
    ])
    expect(maxInFlight).toBe(1)
  })

  it('shows the plain delegation message for every permission-style refusal', async () => {
    for (const code of ['module_delegation_denied', 'presence_denied', 'presence_timeout', 'presence_unavailable']) {
      cleanup()
      today = { must_do: [], deep_block: null, inbox: [], carry_over_candidates: [] }
      sin90Write.mockResolvedValueOnce({ ok: false, status: 403, data: { error: { code, message: 'raw english' } } })
      render(<Sin90Page />)
      fireEvent.change(await screen.findByLabelText(/随手记/), { target: { value: '一条' } })
      fireEvent.click(screen.getByRole('button', { name: '记下' }))
      expect(await screen.findByText('桌面还不能写入 Sin90。请在确认框里允许，然后再试一次。')).toBeInTheDocument()
      expect(screen.queryByText(/raw english/)).not.toBeInTheDocument()
    }
  })
})

describe('Sin90 three layers', () => {
  it('starts on Today and switches among the three tabs', async () => {
    render(<Sin90Page />)
    await screen.findByRole('tab', { name: '今天' })
    expect(screen.getByRole('tab', { name: '今天' })).toHaveAttribute('aria-selected', 'true')
    fireEvent.click(screen.getByRole('tab', { name: '方向' }))
    expect(await screen.findByText('五个人生系统')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('tab', { name: '节奏' }))
    expect(await screen.findByText('年度')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('tab', { name: '今天' }))
    expect(await screen.findByLabelText(/随手记/)).toBeInTheDocument()
  })

  it('opens the separate review view from the review button', async () => {
    render(<Sin90Page />)
    await screen.findByRole('button', { name: '复盘' })
    fireEvent.click(screen.getByRole('button', { name: '复盘' }))
    expect(await screen.findByText('最近的复盘')).toBeInTheDocument()
    expect(screen.getByLabelText('频率')).toBeInTheDocument()
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
    fireEvent.click(await screen.findByText('多走路'))
    expect(await screen.findByLabelText('三年目标')).toBeInTheDocument()
    expect(screen.queryByText(/半马|40 周|300/)).not.toBeInTheDocument()
    fireEvent.click(screen.getByRole('tab', { name: '节奏' }))
    expect(await screen.findByText('年度')).toBeInTheDocument()
  })

  it('shows a three-year goal when the directions response contains one', async () => {
    areas = [{ id: 'a1', title: '健康', status: 'active', sort_key: 1 }]
    directions = [{ id: 'd1', area_id: 'a1', title: '多走路', status: 'active', target_window: 'ongoing', goal_3y: '每年走 1000 公里' }]
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('tab', { name: '方向' }))
    fireEvent.click(await screen.findByText('多走路'))
    expect(await screen.findByLabelText('三年目标')).toHaveValue('每年走 1000 公里')
  })

  it('saves an edited three-year goal through the registered write operation', async () => {
    areas = [{ id: 'a1', title: '健康', status: 'active', sort_key: 1 }]
    directions = [{ id: 'd1', area_id: 'a1', title: '多走路', status: 'active', target_window: 'ongoing' }]
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('tab', { name: '方向' }))
    fireEvent.click(await screen.findByText('多走路'))
    fireEvent.change(await screen.findByLabelText('三年目标'), { target: { value: '每周徒步一次' } })
    fireEvent.click(screen.getAllByRole('button', { name: '保存' }).at(-1)!)
    await waitFor(() => expect(sin90Write).toHaveBeenCalledWith({ op: 'updateDirection', id: 'd1', goal3y: '每周徒步一次', goalFromYear: null }))
  })

  it('offers only same-direction plans at valid upper levels as parents', async () => {
    areas = [{ id: 'a1', title: '健康', status: 'active', sort_key: 1 }]
    directions = [{ id: 'd1', area_id: 'a1', title: '多走路', status: 'active', target_window: 'ongoing' }]
    periodPlans = { period_plans: [
      { id: 'year1', direction_id: 'd1', kind: 'year', period: String(new Date().getFullYear()), target: '年度', metric: 'none', status: 'active' },
      { id: 'foreign', direction_id: 'd2', kind: 'year', period: String(new Date().getFullYear()), target: '别的方向', metric: 'none', status: 'active' },
      { id: 'half1', direction_id: 'd1', kind: 'half', period: `${new Date().getFullYear()}-H${new Date().getMonth() < 6 ? 1 : 2}`, target: '半年', metric: 'none', status: 'active' },
    ] }
    expect(readPeriodPlans(periodPlans)).toHaveLength(3)
    expect(legalPlanParents('quarter', `${new Date().getFullYear()}-Q${Math.floor(new Date().getMonth() / 3) + 1}`,  readPeriodPlans(periodPlans), 'd1')).toHaveLength(2)
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('tab', { name: '方向' }))
    fireEvent.click(await screen.findByText('多走路'))
    fireEvent.change(screen.getByLabelText('计划层级'), { target: { value: 'quarter' } })
    const parent = screen.getAllByRole('combobox')[1]!
    expect(within(parent).getByRole('option', { name: /年度/ })).toBeInTheDocument()
    expect(within(parent).getByRole('option', { name: /半年/ })).toBeInTheDocument()
    expect(within(parent).queryByRole('option', { name: /别的方向/ })).not.toBeInTheDocument()
  })

  it('shows real rhythm columns and a consecutive-miss risk marker', async () => {
    areas = [{ id: 'a1', title: '健康', status: 'active', sort_key: 1 }]
    directions = [{ id: 'd1', area_id: 'a1', title: '多走路', status: 'active', target_window: 'ongoing' }]
    periodPlans = { period_plans: [{ id: 'p1', direction_id: 'd1', kind: 'month', period: '2026-11', target: '走路 20 次', metric: 'count', metric_target: 20, status: 'active' }] }
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('tab', { name: '节奏' }))
    expect(await screen.findByText(/走路 20 次/)).toBeInTheDocument()
    expect(screen.getByText('有风险')).toBeInTheDocument()
    expect(screen.getByText('已连续 3 周未达标')).toBeInTheDocument()
  })

  it('saves review settings with PUT semantics through the dedicated write op', async () => {
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('button', { name: '复盘' }))
    fireEvent.change(await screen.findByLabelText('频率'), { target: { value: 'monthly' } })
    fireEvent.click(screen.getAllByRole('button', { name: '保存' }).at(-1)!)
    await waitFor(() => expect(sin90Write).toHaveBeenCalledWith({ op: 'putReviewSettings', auto: true, cadence: 'monthly', weekday: 7, hour: 18 }))
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
