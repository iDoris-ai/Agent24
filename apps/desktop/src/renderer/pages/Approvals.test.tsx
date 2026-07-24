// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import ApprovalsPage from './Approvals'

const APPROVAL = {
  id: 'apr_1',
  run_id: 'run_1',
  tool_call_id: 'tc_1',
  kind: 'exec',
  summary: 'run rm -rf /tmp/x',
  payload: {},
  available_decisions: ['approve', 'approve_for_session', 'deny', 'abort'],
  status: 'pending',
  expires_at: '2026-07-24T10:05:00Z',
  created_at: '2026-07-24T10:00:00Z',
}

let posted: Array<{ path: string; body?: unknown }> = []

function mount(list: unknown[]) {
  posted = []
  const proxy = vi.fn((req: { method: string; path: string; body?: unknown }) => {
    if (req.method === 'GET') {
      return Promise.resolve({ ok: true, status: 200, data: { approvals: list } })
    }
    posted.push({ path: req.path, body: req.body })
    return Promise.resolve({ ok: true, status: 200, data: {} })
  })
  window.agent24 = { backendProxy: proxy } as never
  return proxy
}

beforeEach(() => {
  // Notification stub — permission granted, record constructions
  const g = globalThis as unknown as { Notification: unknown }
  g.Notification = class {
    static permission = 'granted'
    static requestPermission = vi.fn().mockResolvedValue('granted')
    constructor(
      public title: string,
      public opts: unknown,
    ) {}
  }
})

describe('ApprovalsPage', () => {
  it('renders pending approvals with their offered decisions', async () => {
    mount([APPROVAL])
    render(<ApprovalsPage />)
    await waitFor(() => expect(screen.getByText('run rm -rf /tmp/x')).toBeInTheDocument())
    expect(screen.getByRole('button', { name: '批准' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: '中止运行' })).toBeInTheDocument()
  })

  it('approve posts the exact decision type', async () => {
    mount([APPROVAL])
    render(<ApprovalsPage />)
    await waitFor(() => screen.getByText('run rm -rf /tmp/x'))
    fireEvent.click(screen.getByRole('button', { name: '批准' }))
    await waitFor(() =>
      expect(posted).toContainEqual({ path: '/api/v1/approvals/apr_1', body: { type: 'approve', reason: undefined } }),
    )
  })

  it('deny requires a reason before it can be submitted', async () => {
    mount([APPROVAL])
    render(<ApprovalsPage />)
    await waitFor(() => screen.getByText('run rm -rf /tmp/x'))
    fireEvent.click(screen.getByRole('button', { name: '拒绝' }))
    // confirm button disabled until a reason is typed
    const confirm = screen.getByRole('button', { name: '确认拒绝' })
    expect(confirm).toBeDisabled()
    fireEvent.change(screen.getByLabelText('拒绝原因'), { target: { value: 'too risky' } })
    expect(confirm).not.toBeDisabled()
    fireEvent.click(confirm)
    await waitFor(() =>
      expect(posted).toContainEqual({ path: '/api/v1/approvals/apr_1', body: { type: 'deny', reason: 'too risky' } }),
    )
  })

  it('fires a desktop notification for a newly-seen approval', async () => {
    const spy = vi.spyOn(
      globalThis as unknown as { Notification: new (t: string, o: unknown) => unknown },
      'Notification',
    )
    mount([APPROVAL])
    render(<ApprovalsPage />)
    await waitFor(() => screen.getByText('run rm -rf /tmp/x'))
    expect(spy).toHaveBeenCalledWith('需要审批', expect.objectContaining({ tag: 'apr_1' }))
  })

  it('shows empty state when nothing pending', async () => {
    mount([])
    render(<ApprovalsPage />)
    await waitFor(() => expect(screen.getByText('没有待处理的审批')).toBeInTheDocument())
  })

  it('surfaces a decide error', async () => {
    const proxy = vi.fn((req: { method: string; path: string }) => {
      if (req.method === 'GET') return Promise.resolve({ ok: true, status: 200, data: { approvals: [APPROVAL] } })
      return Promise.resolve({ ok: false, status: 400, data: { error: { message: 'rejected' } } })
    })
    window.agent24 = { backendProxy: proxy } as never
    render(<ApprovalsPage />)
    await waitFor(() => screen.getByText('run rm -rf /tmp/x'))
    fireEvent.click(screen.getByRole('button', { name: '批准' }))
    await waitFor(() => expect(screen.getByText('rejected')).toBeInTheDocument())
  })

  it('deny reason entry can be cancelled with 返回', async () => {
    mount([APPROVAL])
    render(<ApprovalsPage />)
    await waitFor(() => screen.getByText('run rm -rf /tmp/x'))
    fireEvent.click(screen.getByRole('button', { name: '拒绝' }))
    expect(screen.getByLabelText('拒绝原因')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: '返回' }))
    // back to the decision buttons, no reason input
    expect(screen.queryByLabelText('拒绝原因')).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: '拒绝' })).toBeInTheDocument()
  })

  it('works when the Notification API is unavailable', async () => {
    delete (globalThis as unknown as { Notification?: unknown }).Notification
    mount([APPROVAL])
    render(<ApprovalsPage />)
    await waitFor(() => expect(screen.getByText('run rm -rf /tmp/x')).toBeInTheDocument())
  })

  it('surfaces a list fetch error', async () => {
    window.agent24 = {
      backendProxy: vi.fn(() =>
        Promise.resolve({ ok: false, status: 503, data: { error: { message: 'unavailable' } } }),
      ),
    } as never
    render(<ApprovalsPage />)
    await waitFor(() => expect(screen.getByText('unavailable')).toBeInTheDocument())
  })

  it('does not notify when permission is denied', async () => {
    const g = globalThis as unknown as { Notification: unknown }
    g.Notification = class {
      static permission = 'denied'
      static requestPermission = vi.fn()
      constructor() {
        throw new Error('should not construct when denied')
      }
    }
    mount([APPROVAL])
    render(<ApprovalsPage />)
    // renders without throwing despite a new approval
    await waitFor(() => expect(screen.getByText('run rm -rf /tmp/x')).toBeInTheDocument())
  })
})
