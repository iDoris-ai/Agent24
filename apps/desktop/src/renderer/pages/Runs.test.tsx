// @vitest-environment jsdom
import { beforeEach, describe, it, expect, vi } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import RunsPage from './Runs'
import { setLocale } from '../i18n'
import { runsMessages } from './Runs.i18n'

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

it('renders the English runs title by default', () => {
  window.agent24 = { backendProxy: proxyMock(() => ({ ok: true, status: 200, data: { runs: [] } })) } as never
  render(<RunsPage />)
  expect(screen.getByText('Runs')).toBeInTheDocument()
})

describe('ID-HR-2 run confirmation', () => {
  const failedRun = { ...RUN, status: 'failed', session_id: 'session_original', input: { prompt: 'original prompt', model_override: 'local-model', mode: 'plan', workspace_id: 'ws_original' }, error: { code: 'idoris_headroom_confirmation_required', message: 'reserved memory' } }

  it('reruns only after confirmation with the original input and selects the new run', async () => {
    const rerun = { ...failedRun, id: 'run_2', status: 'running', error: null }
    const proxy = vi.fn(async (req: { method: string; path: string; body?: unknown }) => {
      if (req.method === 'POST') return { ok: true, status: 202, data: rerun }
      return { ok: true, status: 200, data: { runs: [failedRun] } }
    })
    window.agent24 = { backendProxy: proxy } as never
    render(<RunsPage />)
    fireEvent.click(await screen.findByText('original prompt'))
    expect(screen.getByText(/iDoris needs to use memory reserved for the system/)).toBeInTheDocument()
    expect(proxy.mock.calls.filter(([req]) => req.method === 'POST')).toHaveLength(0)
    const button = screen.getByRole('button', { name: 'Rerun and confirm' })
    fireEvent.click(button)
    fireEvent.click(button)
    await waitFor(() => expect(screen.getByText('run_2')).toBeInTheDocument())
    expect(proxy.mock.calls.filter(([req]) => req.method === 'POST')).toEqual([[{ method: 'POST', path: '/api/v1/runs', body: { ...failedRun.input, session_id: 'session_original', idoris_headroom_confirmed: true } }]])
  })

  it('explains why a Knowledge host run cannot be rerun through the desktop proxy', async () => {
    window.agent24 = { backendProxy: proxyMock(() => ({ ok: true, status: 200, data: { runs: [{ ...failedRun, input: { ...failedRun.input, knowledge_binding_id: 'binding_1' } }] } })) } as never
    render(<RunsPage />)
    fireEvent.click(await screen.findByText('original prompt'))
    expect(screen.getByText(/iDoris needs to use memory reserved for the system/)).toBeInTheDocument()
    expect(screen.getByText(/Knowledge host/)).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Rerun and confirm' })).not.toBeInTheDocument()
  })

  it('shows supervisor busy as retryable without headroom confirmation', async () => {
    window.agent24 = { backendProxy: proxyMock(() => ({ ok: true, status: 200, data: { runs: [{ ...failedRun, error: { code: 'idoris_supervisor_busy', message: 'busy' } }] } })) } as never
    render(<RunsPage />)
    fireEvent.click(await screen.findByText('original prompt'))
    expect(screen.getByText(/iDoris is busy.*try again/i)).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Rerun and confirm' })).not.toBeInTheDocument()
  })

  it.each(['en', 'zh', 'th'] as const)('has complete nonempty run confirmation translations in %s', locale => {
    for (const key of ['headroom.explanation', 'headroom.confirm', 'headroom.hostOnly', 'error.supervisorBusy']) {
      expect(runsMessages[locale]).toHaveProperty([key], expect.stringMatching(/\S/))
    }
  })
})

describe('RunsPage', () => {
  beforeEach(() => { setLocale('zh') })
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

  it('shows the empty state when there are no runs', async () => {
    window.agent24 = {
      backendProxy: proxyMock(() => ({ ok: true, status: 200, data: { runs: [] } })),
    } as never
    render(<RunsPage />)
    await waitFor(() => expect(screen.getByText('暂无运行任务')).toBeInTheDocument())
    // and the detail panel prompts for a selection
    expect(screen.getByText('选择一个任务查看详情')).toBeInTheDocument()
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
