// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { BackendProxyRequest } from '../../../shared/ipc-types'
import { setLocale } from '../../i18n'
import { Sin90Page } from './Sin90Page'
import { isoWeekUTC } from './model'
import type { CompletionEvidence } from './loop-model'
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
let priorities: Record<string, unknown> = {}
let completionDraft: unknown = {}
let keyAvailable = true
let failToday = false
let dataStatus: unknown = {
  template: null,
  backups: [],
  counts: { areas: 0, directions: 0, tasks: 0, plans: 0, routines: 0 },
}
let templates: unknown = []
const getPaths: string[] = []
const sin90Write = vi.fn(async (_body: Record<string, unknown>) => ({ ok: true, status: 200, data: {} }))
const sin90ExportToFile = vi.fn(async () => ({ ok: true, status: 200, data: { ok: true } }))
const sin90ImportFromFile = vi.fn(async () => ({ ok: true, status: 200, data: { ok: true } }))
function task(id: string, title: string, status = 'in_progress') { return { id, title, status, direction_id: 'd1', week_id: null } }

beforeEach(() => {
  priorities = {}; completionDraft = {}; keyAvailable = true
  setLocale('zh'); areas = []; directions = []; tasks = []; weeks = []; blocks = []; proposals = []; reviews = []; periodPlans = null; failToday = false; getPaths.length = 0
  today = { must_do: [], deep_block: null, inbox: [], carry_over_candidates: [] }
  dataStatus = {
    template: null,
    backups: [],
    counts: { areas: 0, directions: 0, tasks: 0, plans: 0, routines: 0 },
  }
  templates = []
  window.localStorage.clear()
  sin90Write.mockReset(); sin90Write.mockResolvedValue({ ok: true, status: 200, data: {} })
  sin90ExportToFile.mockReset(); sin90ExportToFile.mockResolvedValue({ ok: true, status: 200, data: { ok: true } })
  sin90ImportFromFile.mockReset(); sin90ImportFromFile.mockResolvedValue({ ok: true, status: 200, data: { ok: true } })
  Object.defineProperty(window, 'agent24', { configurable: true, value: {
    sin90Write, sin90KeyStatus: async () => ({ available: keyAvailable }),
    sin90ExportToFile, sin90ImportFromFile,
    onSin90Event: () => () => undefined,
    backendProxy: async (req: BackendProxyRequest) => {
      getPaths.push(req.path)
      if (req.path.endsWith('/priority')) return priorities[req.path] ? { ok: true, status: 200, data: priorities[req.path] } : { ok: false, status: 404, data: {} }
      if (req.path.includes('/review/weekly/draft?')) return { ok: true, status: 200, data: completionDraft }
      if (req.path.endsWith('/tasks')) return { ok: true, status: 200, data: { tasks } }
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
      if (req.path.endsWith('/data/status')) return { ok: true, status: 200, data: dataStatus }
      if (req.path.endsWith('/templates')) return { ok: true, status: 200, data: templates }
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
    expect(await screen.findByText('人生系统')).toBeInTheDocument()
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
    expect(await screen.findByText('人生系统')).toBeInTheDocument()
  })

  it('treats a weekly_review draft as this week’s review for summarize and the start guard', async () => {
    const period = isoWeekUTC(new Date())
    reviews = [
      { id: 'daily', kind: 'daily', period, status: 'draft', body: '' },
      { id: 'r1', kind: 'weekly_review', period, status: 'draft', body: '' },
    ]
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('button', { name: '复盘' }))
    await screen.findByText('最近的复盘')
    expect(screen.getAllByRole('button', { name: '生成复盘建议' })).toHaveLength(1)
    expect(screen.getByRole('button', { name: '开始本周复盘' })).toBeDisabled()
    fireEvent.click(screen.getByRole('button', { name: '生成复盘建议' }))
    await waitFor(() => expect(sin90Write).toHaveBeenCalledWith({ op: 'aiSummarize', reviewId: 'r1' }))
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

  it('offers a demo language switch when the loaded template differs from the UI', async () => {
    dataStatus = { template: { id: 'graduate', lang: 'en', loaded_at: '2026-11-18T00:00:00Z', title: 'Graduate', guide: 'Guide' }, backups: [], counts: { areas: 1, directions: 1, tasks: 0, plans: 0, routines: 0 } }
    render(<Sin90Page />)
    const button = await screen.findByRole('button', { name: '换成 中文 版示范' })
    fireEvent.click(button)
    // Replaces all data, so it asks first like every other reset (#1917 M2).
    expect(await screen.findByText(/加载示范前会自动保存一份当前数据/)).toBeInTheDocument()
    expect(sin90Write).not.toHaveBeenCalled()
    fireEvent.click(screen.getByRole('button', { name: '确认' }))
    await waitFor(() => expect(sin90Write).toHaveBeenCalledWith({ op: 'dataReset', template: 'graduate', lang: 'zh' }))
  })

  it('places a must-do direction label below the full-width title', async () => {
    directions = [{ id: 'd1', area_id: null, title: '一个很长很长的方向名称', status: 'active', target_window: 'ongoing' }]
    today = { must_do: [task('t1', '必做事项标题')], deep_block: null, inbox: [], carry_over_candidates: [] }
    render(<Sin90Page />)
    const row = await screen.findByRole('listitem', { name: '必做事项标题' })
    expect(within(row).getByText('一个很长很长的方向名称')).toHaveClass('sin90-task-direction')
    expect(row.querySelector('.sin90-task-direction')?.getAttribute('title')).toBe('一个很长很长的方向名称')
  })

  it('keeps area state read-only until edit is selected', async () => {
    areas = [{ id: 'a1', title: '健康', status: 'active', sort_key: 1, current_state: '偶尔散步', ideal_state: '规律运动' }]
    directions = [{ id: 'd1', area_id: 'a1', title: '多运动', status: 'active', target_window: 'ongoing' }]
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('tab', { name: '方向' }))
    expect(await screen.findByText('偶尔散步')).toBeInTheDocument()
    expect(screen.queryByLabelText('现在的状态')).not.toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: '编辑' }))
    expect(screen.getByLabelText('现在的状态')).toHaveValue('偶尔散步')
    expect(screen.getByLabelText('想要的状态')).toHaveValue('规律运动')
  })

  it('uses clear placeholders where the server has no planning data', async () => {
    areas = [{ id: 'a1', title: '健康', status: 'active', sort_key: 1 }]
    directions = [{ id: 'd1', area_id: 'a1', title: '多走路', status: 'active', target_window: 'ongoing' }]
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('tab', { name: '方向' }))
    expect(await screen.findByText('三年目标：')).toBeInTheDocument()
    expect(screen.getByLabelText('三年目标').closest('details')).not.toHaveProperty('open', true)
    expect(screen.queryByText(/半马|40 周|300/)).not.toBeInTheDocument()
    fireEvent.click(screen.getByRole('tab', { name: '节奏' }))
    expect(await screen.findByText('年度')).toBeInTheDocument()
  })

  it('shows a three-year goal when the directions response contains one', async () => {
    areas = [{ id: 'a1', title: '健康', status: 'active', sort_key: 1 }]
    directions = [{ id: 'd1', area_id: 'a1', title: '多走路', status: 'active', target_window: 'ongoing', goal_3y: '每年走 1000 公里' }]
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('tab', { name: '方向' }))
    expect(await screen.findByText('每年走 1000 公里')).toBeInTheDocument()
    expect(screen.getByLabelText('三年目标').closest('details')).not.toHaveProperty('open', true)
  })

  it('saves an edited three-year goal through the registered write operation', async () => {
    areas = [{ id: 'a1', title: '健康', status: 'active', sort_key: 1 }]
    directions = [{ id: 'd1', area_id: 'a1', title: '多走路', status: 'active', target_window: 'ongoing' }]
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('tab', { name: '方向' }))
    fireEvent.click(await screen.findByText('编辑计划'))
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
    fireEvent.click(await screen.findByText('编辑计划'))
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
    expect(screen.getByText(/有风险/)).toBeInTheDocument()
    expect(screen.getByText('已连续 3 周未达标')).toBeInTheDocument()
    expect(document.querySelector('.sin90-rhythm-direction')).toBeInTheDocument()
    expect(document.querySelector('.sin90-rhythm-target')).toHaveClass('sin90-rhythm-target')
    expect(document.querySelector('.sin90-period-no-half .sin90-half-empty')).toBeInTheDocument()
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
    fireEvent.click(screen.getByRole('button', { name: /装上人生系统/ }))
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

describe('Sin90 demo banner & data panel', () => {
  it('shows demo banner when template is present, requires confirmation before clearing', async () => {
    dataStatus = {
      template: {
        id: 'graduate',
        lang: 'zh',
        loaded_at: '2026-11-19 09:00',
        title: '示范：刚毕业的第一年',
        guide: '欢迎体验 Sin90 示范数据。',
      },
      backups: [],
      counts: { areas: 5, directions: 6, tasks: 7, plans: 10, routines: 2 },
    }
    render(<Sin90Page />)
    expect(await screen.findByText('你正在看示范数据：示范：刚毕业的第一年')).toBeInTheDocument()
    expect(screen.getByText('欢迎体验 Sin90 示范数据。')).toBeInTheDocument()

    // Click clear button on banner
    fireEvent.click(screen.getByRole('button', { name: '清空，从我自己开始' }))
    expect(await screen.findByText(/清空前会自动保存一份当前数据/)).toBeInTheDocument()
    expect(sin90Write).not.toHaveBeenCalled()

    // Confirm
    fireEvent.click(screen.getByRole('button', { name: '确认' }))
    await waitFor(() => expect(sin90Write).toHaveBeenCalledWith({ op: 'dataReset' }))
  })

  it('dismisses demo banner on "Got it" and persists state to localStorage', async () => {
    dataStatus = {
      template: {
        id: 'graduate',
        lang: 'zh',
        loaded_at: '2026-11-19 09:00',
        title: '示范：刚毕业的第一年',
        guide: '欢迎体验 Sin90 示范数据。',
      },
      backups: [],
      counts: { areas: 5, directions: 6, tasks: 7, plans: 10, routines: 2 },
    }
    render(<Sin90Page />)
    expect(await screen.findByText('你正在看示范数据：示范：刚毕业的第一年')).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: '知道了' }))
    await waitFor(() => {
      expect(screen.queryByText('你正在看示范数据：示范：刚毕业的第一年')).not.toBeInTheDocument()
    })
    expect(window.localStorage.getItem('sin90:demo-banner-dismissed')).toBe('1')
  })

  it('data panel lists backups and triggers dataRestoreBackup after confirmation', async () => {
    dataStatus = {
      template: null,
      backups: [{ name: 'auto-20261119-01.json', at: '2026-11-19 10:00:00', bytes: 2048 }],
      counts: { areas: 5, directions: 4, tasks: 3, plans: 2, routines: 1 },
    }
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('button', { name: '数据' }))

    expect(await screen.findByText('Sin90 数据管理')).toBeInTheDocument()
    expect(screen.getByText(/5 个系统 · 4 个方向 · 2 条计划 · 3 个任务/)).toBeInTheDocument()
    expect(screen.getByText('当前为个人数据（非示范）')).toBeInTheDocument()
    expect(screen.getByText('auto-20261119-01.json')).toBeInTheDocument()

    // Click restore
    fireEvent.click(screen.getByRole('button', { name: '恢复到这个时间点' }))
    expect(await screen.findByText(/确定要恢复备份「auto-20261119-01.json」吗/)).toBeInTheDocument()
    expect(sin90Write).not.toHaveBeenCalled()

    // Confirm
    fireEvent.click(screen.getByRole('button', { name: '确认' }))
    await waitFor(() => expect(sin90Write).toHaveBeenCalledWith({ op: 'dataRestoreBackup', name: 'auto-20261119-01.json' }))
  })

  it('data panel triggers export and import IPCs', async () => {
    dataStatus = {
      template: null,
      backups: [],
      counts: { areas: 0, directions: 0, tasks: 0, plans: 0, routines: 0 },
    }
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('button', { name: '数据' }))

    // Export
    fireEvent.click(await screen.findByRole('button', { name: '导出备份' }))
    await waitFor(() => expect(sin90ExportToFile).toHaveBeenCalled())

    // Import
    fireEvent.click(screen.getByRole('button', { name: '从文件恢复' }))
    expect(await screen.findByText(/确定要从所选文件恢复吗/)).toBeInTheDocument()
    expect(sin90ImportFromFile).not.toHaveBeenCalled()

    fireEvent.click(screen.getByRole('button', { name: '确认' }))
    await waitFor(() => expect(sin90ImportFromFile).toHaveBeenCalled())
  })

  it('data panel supports loading demo with language and clearing to empty', async () => {
    dataStatus = {
      template: null,
      backups: [],
      counts: { areas: 0, directions: 0, tasks: 0, plans: 0, routines: 0 },
    }
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('button', { name: '数据' }))

    // Load demo
    fireEvent.click(await screen.findByRole('button', { name: '加载示范数据' }))
    expect(await screen.findByText(/确定要加载示范数据吗/)).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: '确认' }))
    await waitFor(() => expect(sin90Write).toHaveBeenCalledWith({ op: 'dataReset', template: 'graduate', lang: 'zh' }))

    // Clear to empty
    fireEvent.click(screen.getByRole('button', { name: '清空成空白' }))
    expect(await screen.findByText(/确定要清空为全新空白吗/)).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: '确认' }))
    await waitFor(() => expect(sin90Write).toHaveBeenCalledWith({ op: 'dataReset' }))
  })
})


describe('Sin90 quadrant fallback', () => {
  it('shows four named quadrants and real unevaluated tasks once on an old module', async () => {
    directions = [{ id: 'd1', title: '产品', status: 'active', target_window: 'ongoing' }]
    tasks = [task('t1', '写完方案'), task('t2', '访谈用户', 'backlog')]
    today = { must_do: [task('t1', '写完方案')], deep_block: null, inbox: [task('t2', '访谈用户', 'backlog')], carry_over_candidates: [] }
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('tab', { name: '四象限' }))
    const board = await screen.findByRole('region', { name: '优先级四象限' })
    for (const name of ['重要 · 紧急', '重要 · 不紧急', '不重要 · 紧急', '不重要 · 不紧急', '未评估']) {
      expect(within(board).getByRole('region', { name })).toBeInTheDocument()
    }
    const unassessed = within(board).getByRole('region', { name: '未评估' })
    expect(within(unassessed).getAllByRole('listitem')).toHaveLength(2)
    expect(screen.getAllByText('写完方案')).toHaveLength(1)
    expect(screen.getAllByText('访谈用户')).toHaveLength(1)
    expect(within(board).getByText(/当前模块未提供优先级评估/)).toBeInTheDocument()
    expect(within(board).queryByLabelText('评估建议（可选）')).not.toBeInTheDocument()
    expect(within(board).queryByLabelText('可选背景')).not.toBeInTheDocument()
    expect(within(board).queryByRole('button', { name: '开始评估' })).not.toBeInTheDocument()
    expect(within(board).getByRole('button', { name: '重新评估优先级' })).toBeDisabled()
    expect(sin90Write).not.toHaveBeenCalled()
  })

  it('keeps capture and long-term edits when returning to daily work', async () => {
    render(<Sin90Page />)
    fireEvent.change(await screen.findByLabelText(/随手记/), { target: { value: '待整理的上下文' } })
    fireEvent.click(screen.getByRole('tab', { name: '方向' }))
    fireEvent.click(screen.getByRole('tab', { name: '节奏' }))
    fireEvent.click(screen.getByRole('tab', { name: '今天' }))
    expect(screen.getByLabelText(/随手记/)).toHaveValue('待整理的上下文')
    expect(screen.getByRole('tab', { name: '今日概览' })).toHaveAttribute('aria-selected', 'true')
    fireEvent.click(screen.getByRole('tab', { name: '四象限' }))
    expect(await screen.findByRole('region', { name: '优先级四象限' })).toBeInTheDocument()
    expect(screen.getByLabelText(/随手记/)).toHaveValue('待整理的上下文')
    fireEvent.click(screen.getByRole('tab', { name: '今日概览' }))
    for (const title of ['下一段专注', '今天先做这三件', '收件箱', '本周节奏', '方向']) expect(screen.getAllByText(title).length).toBeGreaterThan(0)
    expect(screen.getByRole('button', { name: '看本周' })).toBeInTheDocument()
    expect(sin90Write).not.toHaveBeenCalled()
  })

  it('completes an unevaluated backlog task without notes or AI', async () => {
    today = { must_do: [], deep_block: null, inbox: [task('t2', '访谈用户', 'backlog')], carry_over_candidates: [] }
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('tab', { name: '四象限' }))
    const row = await screen.findByRole('listitem', { name: '访谈用户' })
    fireEvent.click(within(row).getByRole('button', { name: '完成' }))
    await waitFor(() => expect(sin90Write).toHaveBeenCalledTimes(3))
    expect(sin90Write.mock.calls.map(([body]) => body)).toEqual([
      { op: 'transitionTask', id: 't2', to: 'planned' },
      { op: 'transitionTask', id: 't2', to: 'in_progress' },
      { op: 'transitionTask', id: 't2', to: 'done' },
    ])
  })
})


describe('Sin90 loop desktop contract', () => {
  const assessment = (quadrant: string, rationale: string) => ({ quadrant, rationale, importance: 'high', urgency: quadrant === 'do_first' ? 'high' : 'low', evaluated_at: '2026-11-24T00:00:00Z' })
  it('allows advice before evaluation and leaves it optional', async () => {
    tasks = [task('t1', '写完方案')]
    priorities['/api/v1/sin90/tasks/t1/priority'] = { task_id: 't1', evaluation_status: 'unevaluated', ai: null, human_override: null, effective: null }
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('tab', { name: '四象限' }))
    const board = await screen.findByRole('region', { name: '优先级四象限' })
    expect(within(board).getByRole('region', { name: '未评估' })).toBeInTheDocument()
    expect(within(board).queryByLabelText('可选背景')).not.toBeInTheDocument()
    const evaluate = within(board).getByRole('button', { name: '重新评估优先级' })
    await waitFor(() => expect(evaluate).toBeEnabled())
    fireEvent.click(evaluate)
    expect(sin90Write).not.toHaveBeenCalled()
    expect(evaluate).toHaveAttribute('aria-expanded', 'true')
    expect(within(board).getByText('AI 会参考你补充的背景，为任务建议象限。你手动指定的象限会保留。')).toBeInTheDocument()
    fireEvent.change(within(board).getByLabelText('可选背景'), { target: { value: '先记下再取消' } })
    fireEvent.click(within(board).getByRole('button', { name: '取消' }))
    expect(sin90Write).not.toHaveBeenCalled()
    expect(within(board).queryByLabelText('可选背景')).not.toBeInTheDocument()
    expect(within(board).getByRole('region', { name: '未评估' })).toBeInTheDocument()
    fireEvent.click(evaluate)
    fireEvent.change(within(board).getByLabelText('可选背景'), { target: { value: '先安排用户访谈，再写方案' } })
    const start = within(board).getByRole('button', { name: '开始评估' })
    let release: (value: { ok: boolean; status: number; data: Record<string, never> }) => void = () => undefined
    sin90Write.mockImplementation(() => new Promise((resolve) => { release = resolve }))
    fireEvent.click(start)
    fireEvent.click(start)
    expect(sin90Write).toHaveBeenCalledTimes(1)
    expect(sin90Write).toHaveBeenCalledWith({ op: 'evaluatePriority', advice: '先安排用户访谈，再写方案' })
    expect(start).toBeDisabled()
    release({ ok: true, status: 200, data: {} })
    await waitFor(() => expect(within(board).getByRole('button', { name: '开始评估' })).toBeEnabled())
    sin90Write.mockResolvedValue({ ok: true, status: 200, data: {} })
    fireEvent.change(within(board).getByLabelText('可选背景'), { target: { value: '   ' } })
    fireEvent.click(within(board).getByRole('button', { name: '开始评估' }))
    await waitFor(() => expect(sin90Write).toHaveBeenLastCalledWith({ op: 'evaluatePriority' }))
  })

  it('preserves a human decision after evaluation and displays AI and human reasons separately', async () => {
    tasks = [task('t1', '写完方案')]
    const ai = assessment('do_first', '今天交付')
    let human: { quadrant: string; rationale: string } | null = null
    const update = () => { priorities['/api/v1/sin90/tasks/t1/priority'] = { task_id: 't1', evaluation_status: 'evaluated', ai, human_override: human, effective: human ?? ai } }
    update()
    sin90Write.mockImplementation(async (body) => {
      if (body['op'] === 'overridePriority') human = { quadrant: String(body['quadrant']), rationale: String(body['rationale']) }
      if (body['op'] === 'clearPriorityOverride') human = null
      update()
      return { ok: true, status: 200, data: {} }
    })
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('tab', { name: '四象限' }))
    let row = await screen.findByRole('listitem', { name: '写完方案' })
    expect(within(screen.getByRole('region', { name: '重要 · 紧急' })).getByText('写完方案')).toBeInTheDocument()
    fireEvent.change(within(row).getByLabelText('我的判断'), { target: { value: 'schedule' } })
    fireEvent.change(within(row).getByLabelText('判断依据 / 建议'), { target: { value: '先访谈再交付' } })
    fireEvent.click(within(row).getByRole('button', { name: '保存我的判断' }))
    await waitFor(() => expect(within(screen.getByRole('region', { name: '重要 · 不紧急' })).getByText('写完方案')).toBeInTheDocument())
    fireEvent.click(screen.getByRole('button', { name: '重新评估优先级' }))
    expect(sin90Write).not.toHaveBeenCalledWith({ op: 'evaluatePriority' })
    fireEvent.click(screen.getByRole('button', { name: '开始评估' }))
    await waitFor(() => expect(sin90Write).toHaveBeenCalledWith({ op: 'evaluatePriority' }))
    row = await screen.findByRole('listitem', { name: '写完方案' })
    expect(within(row).getByText(/AI 判断：重要 · 紧急 · 今天交付/)).toBeInTheDocument()
    expect(within(row).getByText(/我的判断：重要 · 不紧急 · 先访谈再交付/)).toBeInTheDocument()
    fireEvent.click(within(row).getByRole('button', { name: '清除我的判断' }))
    await waitFor(() => expect(within(screen.getByRole('region', { name: '重要 · 紧急' })).getByText('写完方案')).toBeInTheDocument())
  })

  it('completes without a note, enriches the same weekly event and changes plans only on acceptance', async () => {
    const completed: CompletionEvidence = { task_id: 't1', title: '写完方案', completed_at: '2026-11-24T00:00:00Z', quadrant: null, note: null }
    tasks = [task('t1', '写完方案')]
    completionDraft = { completed_tasks: [] }
    today = { must_do: [task('t1', '写完方案')], deep_block: null, inbox: [], carry_over_candidates: [] }
    sin90Write.mockImplementation(async (body) => {
      if (body['op'] === 'transitionTask' && body['to'] === 'done') {
        tasks = [task('t1', '写完方案', 'done')]
        today = { must_do: [task('t1', '写完方案', 'done')], deep_block: null, inbox: [], carry_over_candidates: [] }
        completionDraft = { completed_tasks: [completed] }
      }
      if (body['op'] === 'completionNote') { completed.note = String(body['note']) }
      if (body['op'] === 'createWeeklyReview') reviews = [{ id: 'r1', kind: 'weekly', period: isoWeekUTC(new Date()), status: 'draft', body: '' }]
      if (body['op'] === 'aiSummarize') proposals = [{ id: 'p1', status: 'pending', source: 'local_brain', ops: [{ op: 'reorder_week_tasks', week_id: 'w1', task_ids: ['t2'] }] }]
      return { ok: true, status: 200, data: {} }
    })
    render(<Sin90Page />)
    fireEvent.click(await screen.findByRole('tab', { name: '四象限' }))
    fireEvent.click(within(await screen.findByRole('listitem', { name: '写完方案' })).getByRole('button', { name: '完成' }))
    await screen.findByText('尚无备注，不推断成效。')
    fireEvent.change(screen.getByLabelText('完成备注'), { target: { value: '得到用户反馈' } })
    fireEvent.click(screen.getByRole('button', { name: '保存备注' }))
    await waitFor(() => expect(sin90Write).toHaveBeenCalledWith({ op: 'completionNote', id: 't1', note: '得到用户反馈' }))
    fireEvent.click(screen.getByRole('button', { name: '复盘' }))
    const material = await screen.findByRole('region', { name: '本周完成素材' })
    expect(within(material).getAllByRole('article')).toHaveLength(1)
    expect(await within(material).findByText('得到用户反馈', { selector: 'p' })).toBeInTheDocument()
    expect(sin90Write.mock.calls.map(([body]) => body['op'])).toEqual(['transitionTask', 'completionNote'])
    fireEvent.click(screen.getByRole('button', { name: '开始本周复盘' }))
    const summarize = await screen.findByRole('button', { name: '生成复盘建议' })
    await waitFor(() => expect(summarize).toBeEnabled())
    fireEvent.click(summarize)
    await screen.findByRole('button', { name: '采纳' })
    expect(sin90Write.mock.calls.map(([body]) => body['op'])).toEqual(['transitionTask', 'completionNote', 'createWeeklyReview', 'aiSummarize'])
    fireEvent.click(screen.getByRole('button', { name: '采纳' }))
    await waitFor(() => expect(sin90Write).toHaveBeenCalledWith({ op: 'acceptProposal', id: 'p1' }))
    fireEvent.click(screen.getByRole('tab', { name: '今天' }))
    fireEvent.click(screen.getByRole('tab', { name: '今日概览' }))
    const finishedRow = screen.getByRole('listitem', { name: '写完方案' })
    expect(within(finishedRow).getByRole('button', { name: '已完成' })).toBeDisabled()
  })

  it('retains overview actions and disabled states after a round trip on an old module', async () => {
    keyAvailable = false
    today = { must_do: [task('t1', '写完方案', 'planned')], deep_block: { id: 'b1', task_id: 't1', direction_id: 'd1', status: 'planned', planned_minutes: 90 }, inbox: [], carry_over_candidates: [] }
    render(<Sin90Page />)
    const overviewRow = await screen.findByRole('listitem', { name: '写完方案' })
    expect(within(overviewRow).getByRole('button', { name: '完成' })).toBeDisabled()
    fireEvent.click(screen.getByRole('tab', { name: '四象限' }))
    await screen.findByText(/当前模块未提供优先级评估/)
    expect(screen.getByRole('button', { name: '重新评估优先级' })).toBeDisabled()
    fireEvent.click(screen.getByRole('tab', { name: '今日概览' }))
    expect(screen.getByRole('button', { name: '开始专注' })).toBeDisabled()
    const row = screen.getByRole('listitem', { name: '写完方案' })
    expect(within(row).getByLabelText('更多任务操作')).toBeInTheDocument()
    expect(sin90Write).not.toHaveBeenCalled()
  })
})


it('keeps focus, more task actions and classification proposals reachable after returning to overview', async () => {
  today = { must_do: [task('t1', '写完方案', 'planned')], deep_block: { id: 'b1', task_id: 't1', direction_id: 'd1', status: 'planned', planned_minutes: 90 }, inbox: [task('t2', '访谈用户', 'backlog')], carry_over_candidates: [] }
  proposals = [{ id: 'p1', status: 'pending', source: 'local_brain', ops: [{ op: 'assign_task_direction', task_id: 't2', direction_id: 'd1' }] }]
  render(<Sin90Page />)
  fireEvent.click(await screen.findByRole('tab', { name: '四象限' }))
  await screen.findByRole('region', { name: '未评估' })
  fireEvent.click(screen.getByRole('tab', { name: '今日概览' }))
  const row = screen.getByRole('listitem', { name: '写完方案' })
  fireEvent.click(within(row).getByLabelText('更多任务操作'))
  fireEvent.click(within(row).getByRole('button', { name: '开始' }))
  await waitFor(() => expect(sin90Write).toHaveBeenCalledWith({ op: 'transitionTask', id: 't1', to: 'in_progress' }))
  await waitFor(() => expect(screen.getByRole('button', { name: '开始专注' })).toBeEnabled())
  fireEvent.click(screen.getByRole('button', { name: '开始专注' }))
  await waitFor(() => expect(sin90Write).toHaveBeenCalledWith({ op: 'transitionBlock', id: 'b1', to: 'started' }))
  fireEvent.click(screen.getByRole('button', { name: '逐条看看' }))
  const accept = await screen.findByRole('button', { name: '采纳' })
  await waitFor(() => expect(accept).toBeEnabled())
  fireEvent.click(accept)
  await waitFor(() => expect(sin90Write).toHaveBeenCalledWith({ op: 'acceptProposal', id: 'p1' }))
})
