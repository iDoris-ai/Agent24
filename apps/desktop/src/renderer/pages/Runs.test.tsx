// @vitest-environment jsdom
import { describe, it, expect, vi } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import RunsPage from './Runs'

function proxyMock(handler: (req: { method: string; path: string }) => unknown) {
  return vi.fn((req: { method: string; path: string }) =>
    Promise.resolve(handler(req)),
  )
}

const RUN = {
  id: 'run_1',
  session_id: null,
  status: 'running',
  input: { prompt: 'do the thing' },
  usage: { total_tokens: 12 },
  schedule_id: 'sch_1',
  created_at: '2026-07-24T10:00:00Z',
}

describe('RunsPage', () => {
  it('renders the run list and selects a run to show detail + cancel', async () => {
    const proxy = proxyMock((req) => {
      if (req.path === '/api/v1/runs') return { ok: true, status: 200, data: { runs: [RUN] } }
      if (req.path === '/api/v1/runs/run_1/cancel') return { ok: true, status: 202, data: {} }
      return { ok: false, status: 404, data: {} }
    })
    window.agent24 = { backendProxy: proxy } as never

    render(<RunsPage />)
    await waitFor(() => expect(screen.getByText('do the thing')).toBeInTheDocument())
    // schedule marker present
    expect(screen.getByTitle('由调度触发')).toBeInTheDocument()

    fireEvent.click(screen.getByText('do the thing'))
    // detail shows the id + a cancel button (run is non-terminal)
    expect(screen.getByText('run_1')).toBeInTheDocument()
    const cancelBtn = screen.getByRole('button', { name: '取消' })
    fireEvent.click(cancelBtn)
    await waitFor(() =>
      expect(proxy).toHaveBeenCalledWith(
        expect.objectContaining({ path: '/api/v1/runs/run_1/cancel', method: 'POST' }),
      ),
    )
  })

  it('shows an error when the list fetch fails', async () => {
    window.agent24 = {
      backendProxy: proxyMock(() => ({
        ok: false,
        status: 503,
        data: { error: { message: 'backend down' } },
      })),
    } as never
    render(<RunsPage />)
    await waitFor(() => expect(screen.getByText('backend down')).toBeInTheDocument())
  })

  it('hides cancel for terminal runs', async () => {
    window.agent24 = {
      backendProxy: proxyMock(() => ({
        ok: true,
        status: 200,
        data: { runs: [{ ...RUN, status: 'completed', output: { text: 'done' }, ended_at: '2026-07-24T10:01:00Z', schedule_id: null }] },
      })),
    } as never
    render(<RunsPage />)
    await waitFor(() => expect(screen.getByText('do the thing')).toBeInTheDocument())
    fireEvent.click(screen.getByText('do the thing'))
    expect(screen.getByText('done')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: '取消' })).not.toBeInTheDocument()
  })
})
