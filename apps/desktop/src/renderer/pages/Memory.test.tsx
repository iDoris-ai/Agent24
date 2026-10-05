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
  // Review M3's own fix (re-`refresh()` after a successful retract) means a
  // realistic mock must actually reflect the delete on the NEXT GET, not
  // keep handing back the original fixed `list` forever.
  const deletedIds = new Set<string>()
  const proxy = vi.fn((req: { method: string; path: string; body?: unknown }) => {
    calls.push(req)
    if (req.method === 'GET' && req.path.startsWith('/api/v1/memory/assertions')) {
      getCount += 1
      if (opts.getFailsAfter !== undefined && getCount > opts.getFailsAfter) {
        return Promise.resolve({ ok: false, status: 500, data: { error: { message: '服务暂不可用' } } })
      }
      const remaining = (list as { id: string }[]).filter((a) => !deletedIds.has(a.id))
      return Promise.resolve({ ok: true, status: 200, data: { assertions: remaining } })
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
      const id = req.path.split('/').pop()
      if (id) deletedIds.add(id)
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

  it('shows a paused-specific empty state when memory is paused (review Low)', async () => {
    mount([], { settingsEnabled: false })
    render(<MemoryPage />)
    await waitFor(() => expect(screen.getByText('记忆已暂停')).toBeInTheDocument())
    expect(screen.queryByText('说『记住……』让我记住')).not.toBeInTheDocument()
  })

  it('does not show the empty-state copy while a failed load leaves the list empty (review Low)', async () => {
    mount([], { getFailsAfter: 0 })
    render(<MemoryPage />)
    await waitFor(() => expect(screen.getByText('服务暂不可用')).toBeInTheDocument())
    expect(screen.queryByText('说『记住……』让我记住')).not.toBeInTheDocument()
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
    // Review H4: the switch's own meaning is "暂停记忆" — checked means
    // PAUSED, not "memory enabled". Memory starts ON (not paused), so it
    // starts UNchecked.
    await waitFor(() => expect(toggle).toHaveAttribute('aria-checked', 'false'))

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
    await waitFor(() => expect(toggle).toHaveAttribute('aria-checked', 'true'))
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
    await waitFor(() => expect(toggle).toHaveAttribute('aria-checked', 'false'))

    fireEvent.click(toggle)

    await waitFor(() => expect(screen.getByText('设置保存失败')).toBeInTheDocument())
    // Still unchecked (not paused) — a failed PUT must not look like it took effect.
    expect(toggle).toHaveAttribute('aria-checked', 'false')
  })

  it('the toggle is disabled while a PUT is in flight (review Low)', async () => {
    let resolvePut: (() => void) | undefined
    const proxy = vi.fn((req: { method: string; path: string; body?: unknown }) => {
      calls.push(req)
      if (req.method === 'GET' && req.path.startsWith('/api/v1/memory/assertions')) {
        return Promise.resolve({ ok: true, status: 200, data: { assertions: [] } })
      }
      if (req.method === 'GET' && req.path === '/api/v1/memory/settings') {
        return Promise.resolve({ ok: true, status: 200, data: { enabled: true } })
      }
      if (req.method === 'PUT') {
        return new Promise((resolve) => {
          resolvePut = () =>
            resolve({ ok: true, status: 200, data: { enabled: false } })
        })
      }
      return Promise.resolve({ ok: true, status: 200, data: null })
    })
    window.agent24 = { backendProxy: proxy } as never

    render(<MemoryPage />)
    const toggle = await screen.findByRole('switch', { name: '暂停记忆' })
    await waitFor(() => expect(toggle).not.toBeDisabled())

    fireEvent.click(toggle)
    await waitFor(() => expect(toggle).toBeDisabled())

    resolvePut?.()
    await waitFor(() => expect(toggle).not.toBeDisabled())
  })
})
