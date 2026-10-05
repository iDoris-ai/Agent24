// @vitest-environment jsdom
import { describe, it, expect, vi } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import MemoryPage from './Memory'

const ASSERTION = {
  id: 'a1',
  text: '我对花生过敏',
  recorded_at: '2026-07-25T08:00:00Z',
  status: 'active' as const,
  evidence: ['ev-1'],
  session: 'sess-1',
}

let calls: Array<{ method: string; path: string; body?: unknown }> = []

function mount(
  list: unknown[],
  opts: {
    settingsEnabled?: boolean
    deleteOk?: boolean
    getFailsAfter?: number
    getSettingsOk?: boolean
    putSettingsOk?: boolean
  } = {},
) {
  calls = []
  let getCount = 0
  const proxy = vi.fn((req: { method: string; path: string; body?: unknown }) => {
    calls.push(req)
    if (req.method === 'GET' && req.path.startsWith('/api/v1/memory/assertions')) {
      getCount += 1
      if (opts.getFailsAfter !== undefined && getCount > opts.getFailsAfter) {
        return Promise.resolve({ ok: false, status: 500, data: { error: { message: '服务暂不可用' } } })
      }
      return Promise.resolve({ ok: true, status: 200, data: { assertions: list } })
    }
    if (req.method === 'GET' && req.path === '/api/v1/memory/settings') {
      if (opts.getSettingsOk === false) {
        return Promise.resolve({ ok: false, status: 500, data: { error: { message: '设置读取失败' } } })
      }
      return Promise.resolve({ ok: true, status: 200, data: { enabled: opts.settingsEnabled ?? true } })
    }
    if (req.method === 'PUT' && req.path === '/api/v1/memory/settings') {
      if (opts.putSettingsOk === false) {
        return Promise.resolve({ ok: false, status: 500, data: { error: { message: '设置保存失败' } } })
      }
      return Promise.resolve({
        ok: true,
        status: 200,
        data: { enabled: (req.body as { enabled: boolean }).enabled },
      })
    }
    if (req.method === 'DELETE') {
      if (opts.deleteOk === false) {
        return Promise.resolve({ ok: false, status: 500, data: { error: { message: '撤回失败' } } })
      }
      return Promise.resolve({ ok: true, status: 204, data: null })
    }
    return Promise.resolve({ ok: true, status: 200, data: null })
  })
  window.agent24 = { backendProxy: proxy } as never
  return proxy
}

describe('MemoryPage', () => {
  it('shows the empty state when there is nothing to remember', async () => {
    mount([])
    render(<MemoryPage />)
    await waitFor(() => expect(screen.getByText('说『记住……』让我记住')).toBeInTheDocument())
  })

  it('renders the list with original sentence, session and recorded time', async () => {
    mount([ASSERTION])
    render(<MemoryPage />)
    await waitFor(() => expect(screen.getByText('我对花生过敏')).toBeInTheDocument())
    expect(screen.getByText(/来自会话 sess-1/)).toBeInTheDocument()
    expect(screen.getByText(/2026-07-25/)).toBeInTheDocument()
  })

  it('retract uses a confirmation with the exact copy, then the item disappears', async () => {
    mount([ASSERTION])
    render(<MemoryPage />)
    await waitFor(() => expect(screen.getByText('我对花生过敏')).toBeInTheDocument())

    fireEvent.click(screen.getByRole('button', { name: '撤回' }))
    expect(
      screen.getByText('撤回后不会再在新会话中被想起；原始对话仍保留在本机'),
    ).toBeInTheDocument()
    // "撤回" is used, never "删除".
    expect(screen.queryByText('删除')).not.toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: '确认撤回' }))
    await waitFor(() =>
      expect(calls).toContainEqual(
        expect.objectContaining({ method: 'DELETE', path: '/api/v1/memory/assertions/a1' }),
      ),
    )
    await waitFor(() => expect(screen.queryByText('我对花生过敏')).not.toBeInTheDocument())
    expect(screen.getByText('说『记住……』让我记住')).toBeInTheDocument()
  })

  it('a failed retract shows an error and keeps the item in the list', async () => {
    mount([ASSERTION], { deleteOk: false })
    render(<MemoryPage />)
    await waitFor(() => expect(screen.getByText('我对花生过敏')).toBeInTheDocument())

    fireEvent.click(screen.getByRole('button', { name: '撤回' }))
    fireEvent.click(screen.getByRole('button', { name: '确认撤回' }))

    await waitFor(() => expect(screen.getByText('撤回失败')).toBeInTheDocument())
    // Still there — a failed request must not silently drop it.
    expect(screen.getByText('我对花生过敏')).toBeInTheDocument()
  })

  it('a failed list refresh shows an error without clearing what is already shown', async () => {
    const proxy = mount([ASSERTION], { getFailsAfter: 1 })
    render(<MemoryPage />)
    await waitFor(() => expect(screen.getByText('我对花生过敏')).toBeInTheDocument())

    // Trigger a second GET (which this mount is wired to fail) via a search keystroke.
    fireEvent.change(screen.getByLabelText('搜索记忆'), { target: { value: '花生' } })

    await waitFor(() => expect(screen.getByText('服务暂不可用')).toBeInTheDocument())
    // The previously-rendered item must still be there.
    expect(screen.getByText('我对花生过敏')).toBeInTheDocument()
    expect(proxy).toHaveBeenCalled()
  })

  it('the pause toggle calls the settings API and reflects the result', async () => {
    mount([], { settingsEnabled: true })
    render(<MemoryPage />)
    await waitFor(() => expect(screen.getByRole('switch', { name: '暂停记忆' })).toBeInTheDocument())
    const toggle = screen.getByRole('switch', { name: '暂停记忆' })
    await waitFor(() => expect(toggle).toHaveAttribute('aria-checked', 'true'))

    fireEvent.click(toggle)

    await waitFor(() =>
      expect(calls).toContainEqual(
        expect.objectContaining({
          method: 'PUT',
          path: '/api/v1/memory/settings',
          body: { enabled: false },
        }),
      ),
    )
    await waitFor(() => expect(toggle).toHaveAttribute('aria-checked', 'false'))
  })

  it('a failed settings read shows an error and leaves the toggle disabled', async () => {
    mount([], { getSettingsOk: false })
    render(<MemoryPage />)
    await waitFor(() => expect(screen.getByText('设置读取失败')).toBeInTheDocument())
    expect(screen.getByRole('switch', { name: '暂停记忆' })).toBeDisabled()
  })

  it('a failed settings write shows an error and does not flip the toggle', async () => {
    mount([], { settingsEnabled: true, putSettingsOk: false })
    render(<MemoryPage />)
    const toggle = screen.getByRole('switch', { name: '暂停记忆' })
    await waitFor(() => expect(toggle).toHaveAttribute('aria-checked', 'true'))

    fireEvent.click(toggle)

    await waitFor(() => expect(screen.getByText('设置保存失败')).toBeInTheDocument())
    // Still "on" — a failed PUT must not look like it took effect.
    expect(toggle).toHaveAttribute('aria-checked', 'true')
  })
})
