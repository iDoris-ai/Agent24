// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { BackendProxyRequest } from '../../../shared/ipc-types'
import { setLocale } from '../../i18n'
import { isoWeekUTC } from './model'
import { Sin90Page } from './Sin90Page'

const emptyToday = {
  must_do: [], deep_block: null, inbox: [], carry_over_candidates: [],
  fired_routines: [], kernel_suspended_routines: [],
}

let directions: unknown[] = []
let tasksByDirection: Record<string, unknown[]> = {}
let today: unknown = emptyToday
let weeks: unknown[] = []
let blocks: unknown[] = []
let routines: unknown[] = []
let rhythms: unknown[] = []
let attentionReads = 0
let keyAvailable = false
const gets: string[] = []
const sin90Write = vi.fn(async () => ({ ok: true, status: 201, data: { id: 'new' } }))

function task(id: string, title: string, direction_id: string | null, status = 'backlog') {
  return { id, title, direction_id, week_id: null, status, kind: 'other', energy: 'mid', est_minutes: null, carried_from: null, created_at: 't', updated_at: 't' }
}

beforeEach(() => {
  setLocale('en')
  directions = []
  tasksByDirection = {}
  today = emptyToday
  weeks = []
  blocks = []
  routines = []
  rhythms = []
  attentionReads = 0
  gets.length = 0
  keyAvailable = false
  sin90Write.mockClear()
  Object.defineProperty(window, 'agent24', {
    configurable: true,
    value: {
      sin90Write,
      sin90KeyStatus: async () => ({ available: keyAvailable }),
      backendProxy: async (req: BackendProxyRequest) => {
        gets.push(req.path)
        if (req.path === '/api/v1/sin90/today') return { ok: true, status: 200, data: today }
        if (req.path === '/api/v1/sin90/directions') return { ok: true, status: 200, data: { directions } }
        if (req.path === '/api/v1/sin90/areas') return { ok: true, status: 200, data: { areas: [] } }
        if (req.path === '/api/v1/sin90/weeks') return { ok: true, status: 200, data: { weeks } }
        if (req.path === '/api/v1/sin90/schedule-blocks') return { ok: true, status: 200, data: { blocks } }
        if (req.path === '/api/v1/sin90/routines') return { ok: true, status: 200, data: { routines } }
        if (req.path === '/api/v1/sin90/rhythms') return { ok: true, status: 200, data: { rhythms } }
        if (req.path.includes('/attention')) {
          attentionReads += 1
          const actual = attentionReads === 1 ? 0 : 90
          return { ok: true, status: 200, data: { week_id: 'w1', planned_min: 90, actual_min: actual, deviation_min: actual - 90 } }
        }
        if (req.path.startsWith('/api/v1/sin90/tasks')) {
          const id = new URL(req.path, 'http://local').searchParams.get('direction_id') ?? ''
          return { ok: true, status: 200, data: { tasks: tasksByDirection[id] ?? [] } }
        }
        throw new Error(req.path)
      },
    },
  })
})

afterEach(() => { cleanup(); setLocale('en') })

describe('Sin90 read-only page', () => {
  it('shows the empty direction form when nothing is stored', async () => {
    render(<Sin90Page />)
    expect(await screen.findByText('Set a direction first')).toBeInTheDocument()
    expect(screen.getByLabelText('Title')).toBeInTheDocument()
    expect(screen.getByLabelText('Window')).toBeInTheDocument()
    expect(screen.queryByText('Not set up')).not.toBeInTheDocument()
    expect(screen.getByLabelText('Inbox')).toBeInTheDocument()
  })

  it('renders three must-do tasks from today', async () => {
    today = {
      ...emptyToday,
      must_do: [task('a', 'Write the plan', 'd1'), task('b', 'Review the diff', 'd1'), task('c', 'Ship the page', 'd1')],
      deep_block: { id: 'blk', direction_id: 'd1', task_id: 'a', status: 'planned', planned_minutes: 90, created_at: 't', updated_at: 't' },
    }
    directions = [{ id: 'd1', area_id: null, title: 'Q4 delivery', status: 'draft', target_window: '2026-Q4', created_at: 't', updated_at: 't' }]
    tasksByDirection = { d1: [task('a', 'Write the plan', 'd1')] }
    render(<Sin90Page />)
    expect((await screen.findAllByText('Write the plan')).length).toBeGreaterThan(0)
    expect(screen.getByText('Review the diff')).toBeInTheDocument()
    expect(screen.getByText('Ship the page')).toBeInTheDocument()
    expect(screen.getByText(/90 min/)).toBeInTheDocument()
  })

  it('filters tasks to the direction that was selected', async () => {
    directions = [
      { id: 'd1', area_id: null, title: 'Delivery', status: 'active', target_window: '2026-Q4', created_at: 't', updated_at: 't' },
      { id: 'd2', area_id: null, title: 'Study', status: 'draft', target_window: 'ongoing', created_at: 't', updated_at: 't' },
    ]
    tasksByDirection = {
      d1: [task('a', 'Write the plan', 'd1')],
      d2: [task('b', 'Read the notes', 'd2')],
    }
    render(<Sin90Page />)
    expect(await screen.findByText('Write the plan')).toBeInTheDocument()
    expect(screen.queryByText('Read the notes')).not.toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Study' }))
    await waitFor(() => expect(screen.getByText('Read the notes')).toBeInTheDocument())
    expect(screen.queryByText('Write the plan')).not.toBeInTheDocument()
  })

  it('uses the same keys in Chinese and Thai', async () => {
    render(<Sin90Page />)
    expect(await screen.findByText('Set a direction first')).toBeInTheDocument()
    act(() => setLocale('zh'))
    expect(screen.getByText('先定一个方向')).toBeInTheDocument()
    expect(screen.getByText('必做')).toBeInTheDocument()
    act(() => setLocale('th'))
    expect(screen.getByText('กำหนดทิศทางก่อน')).toBeInTheDocument()
    expect(screen.getByText('ต้องทำ')).toBeInTheDocument()
    expect(screen.getByText('อ่านกุญแจมนุษย์ไม่ได้ การเขียนจึงปิดอยู่')).toBeInTheDocument()
  })

  it('renders only the legal button for each task status', async () => {
    keyAvailable = true
    directions = [{ id: 'd1', area_id: null, title: 'Delivery', status: 'active', target_window: '2026-Q4', created_at: 't', updated_at: 't' }]
    tasksByDirection = {
      d1: [
        task('b', 'Backlog item', 'd1', 'backlog'),
        task('p', 'Planned item', 'd1', 'planned'),
        task('r', 'Running item', 'd1', 'in_progress'),
        task('n', 'Done item', 'd1', 'done'),
      ],
    }
    render(<Sin90Page />)
    const backlog = within(await screen.findByRole('listitem', { name: 'Backlog item' }))
    expect(backlog.getByRole('button', { name: 'Plan it' })).toBeEnabled()
    expect(within(screen.getByRole('listitem', { name: 'Backlog item' })).getByRole('button', { name: 'Drop' })).toBeEnabled()
    expect(within(screen.getByRole('listitem', { name: 'Backlog item' })).queryByRole('button', { name: 'Start' })).not.toBeInTheDocument()
    expect(within(screen.getByRole('listitem', { name: 'Backlog item' })).queryByRole('button', { name: 'Done' })).not.toBeInTheDocument()
    expect(within(screen.getByRole('listitem', { name: 'Planned item' })).getByRole('button', { name: 'Start' })).toBeEnabled()
    expect(within(screen.getByRole('listitem', { name: 'Planned item' })).queryByRole('button', { name: 'Plan it' })).not.toBeInTheDocument()
    expect(within(screen.getByRole('listitem', { name: 'Planned item' })).queryByRole('button', { name: 'Done' })).not.toBeInTheDocument()
    expect(within(screen.getByRole('listitem', { name: 'Running item' })).getByRole('button', { name: 'Done' })).toBeEnabled()
    expect(within(screen.getByRole('listitem', { name: 'Running item' })).queryByRole('button', { name: 'Start' })).not.toBeInTheDocument()
    expect(within(screen.getByRole('listitem', { name: 'Done item' })).queryByRole('button')).not.toBeInTheDocument()
  })

  it('does not send a write while the human key is missing', async () => {
    directions = [{ id: 'd1', area_id: null, title: 'Delivery', status: 'active', target_window: '2026-Q4', created_at: 't', updated_at: 't' }]
    tasksByDirection = { d1: [task('b', 'Backlog item', 'd1', 'backlog')] }
    render(<Sin90Page />)
    const button = await within(await screen.findByRole('listitem', { name: 'Backlog item' })).findByRole('button', { name: 'Plan it' })
    expect(button).toBeDisabled()
    expect(screen.getByText('Writes are off because the human key could not be read.')).toBeInTheDocument()
    fireEvent.click(button)
    expect(sin90Write).not.toHaveBeenCalled()
  })

  it('shows an empty week in three languages and no day grid', async () => {
    directions = [{ id: 'd1', area_id: null, title: 'Delivery', status: 'active', target_window: '2026-Q4', created_at: 't', updated_at: 't' }]
    render(<Sin90Page />)
    expect(await screen.findByText('Nothing is planned this week')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Open a week' })).toBeInTheDocument()
    expect(screen.queryByRole('columnheader')).not.toBeInTheDocument()
    expect(screen.queryByText('Monday')).not.toBeInTheDocument()
    expect(document.querySelector('.sin90-calendar')).toBeNull()
    act(() => setLocale('zh'))
    expect(screen.getByText('这周还没有安排')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: '开一周' })).toBeInTheDocument()
    act(() => setLocale('th'))
    expect(screen.getByText('สัปดาห์นี้ยังไม่ได้จัด')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'เปิดสัปดาห์' })).toBeInTheDocument()
  })

  it('renders blocks as a queue and requests attention again after one is completed', async () => {
    keyAvailable = true
    directions = [{ id: 'd1', area_id: null, title: 'Delivery', status: 'active', target_window: '2026-Q4', created_at: 't', updated_at: 't' }]
    tasksByDirection = { d1: [task('a', 'Write the plan', 'd1')] }
    weeks = [{ id: 'w1', status: 'planning', iso_week: isoWeekUTC(new Date()), created_at: 't', updated_at: 't' }]
    blocks = [{ id: 'blk', direction_id: 'd1', task_id: 'a', status: 'started', planned_minutes: 90, created_at: 't', updated_at: 't' }]
    routines = [{ id: 'r1', direction_id: 'd1', title: 'Deep work', kind: 'deep_work', status: 'active', cron: '0 9 * * *', tz: 'UTC', created_at: 't', updated_at: 't' }]
    rhythms = [{ id: 'rh', status: 'active', created_at: 't2', allocations: [{ direction_id: 'd1', pct: 70 }] }]
    render(<Sin90Page />)
    const queue = await screen.findByRole('list', { name: 'Blocks' })
    expect(queue.className).toContain('sin90-queue')
    expect(within(queue).getAllByRole('listitem')).toHaveLength(1)
    expect(screen.queryByRole('columnheader')).not.toBeInTheDocument()
    expect(screen.queryByText('Monday')).not.toBeInTheDocument()
    expect(screen.queryByText('Sunday')).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Start this week' })).toBeEnabled()
    expect(screen.getByText(/actual 0/)).toBeInTheDocument()
    expect(screen.getByText(/Delivery 70%/)).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /retire/i })).not.toBeInTheDocument()
    expect(within(screen.getByRole('listitem', { name: 'Deep work' })).getByRole('button', { name: 'Pause' })).toBeEnabled()
    const before = gets.filter((path) => path.includes('/attention')).length
    expect(before).toBe(1)
    fireEvent.click(within(queue).getByRole('button', { name: 'Complete' }))
    await waitFor(() => expect(screen.getByText(/actual 90/)).toBeInTheDocument())
    expect(gets.filter((path) => path.includes('/attention')).length).toBeGreaterThan(before)
    expect(sin90Write).toHaveBeenCalledWith({ op: 'transitionBlock', id: 'blk', to: 'completed' })
    act(() => setLocale('zh'))
    expect(screen.getByRole('button', { name: '开始这周' })).toBeInTheDocument()
    expect(screen.getByRole('list', { name: '块' })).toBeInTheDocument()
    act(() => setLocale('th'))
    expect(screen.getByRole('button', { name: 'เริ่มสัปดาห์นี้' })).toBeInTheDocument()
    expect(screen.getByRole('list', { name: 'บล็อก' })).toBeInTheDocument()
  })
})
