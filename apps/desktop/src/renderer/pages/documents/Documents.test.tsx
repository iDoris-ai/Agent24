// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import DocumentsPage from './Documents'
import { getImportState, resetImportStore } from './importStore'

const doc = (n: number) => ({
  document_id: `doc_01K74Z3QJ8V5N2W9RTX6YB4M${String(n).padStart(2, '0')}`,
  title: `通告 ${n}`,
  media_type: 'application/pdf',
  head_revision: 1,
  created_at: '2026-10-08T09:00:00.000Z',
  updated_at: '2026-10-08T09:00:00.000Z',
})
const job = (status: string, error: unknown = null) => ({
  job_id: 'job_01K75A0B1C2D3E4F5G6H7J8K9M',
  kind: 'import',
  status,
  attempt: 1,
  error,
  result: null,
  created_at: '2026-10-08T09:00:00.000Z',
  updated_at: '2026-10-08T09:00:00.000Z',
})
type Res = { ok: boolean; status: number; data?: unknown; error?: unknown }
const ok = (data: unknown): Promise<Res> => Promise.resolve({ ok: true, status: 200, data })
const err = (status: number, code: string, message: string, details?: unknown): Promise<Res> =>
  Promise.resolve({ ok: false, status, error: { code, message, ...(details ? { details } : {}) } })
const IMPORT = { name: '导入文件（PDF、JPEG、PNG）' }

interface Mock {
  request: ReturnType<typeof vi.fn>
  importFile: ReturnType<typeof vi.fn>
  progress: ((p: { sent: number; total: number }) => void) | null
}

/** `list` answers from `lists` in turn (the last one repeats); `job` from `jobs`. */
function mount(opts: { lists?: Array<() => Promise<Res>>; jobs?: Array<() => Promise<Res>>; capabilities?: () => Promise<Res> }): Mock {
  const lists = [...(opts.lists ?? [() => ok({ documents: [], next_cursor: null })])]
  const jobs = [...(opts.jobs ?? [])]
  const m: Mock = { request: vi.fn(), importFile: vi.fn(() => Promise.resolve(null)), progress: null }
  m.request.mockImplementation((req: { op: string }) => {
    if (req.op === 'capabilities') return (opts.capabilities ?? (() => ok({ storage: { state: 'ready' } })))()
    if (req.op === 'list') return (lists.length > 1 ? lists.shift()! : lists[0]!)()
    if (req.op === 'job') return jobs.shift()!()
    return err(400, 'invalid_request', 'unexpected')
  })
  // No backendProxy at all: the page must not need it (README §10.3).
  window.agent24 = {
    documents: {
      request: m.request,
      importFile: m.importFile,
      onImportProgress: (cb: Mock['progress']) => {
        m.progress = cb
        return () => {
          m.progress = null
        }
      },
    },
  } as never
  return m
}

const page = (docs: number[], next: string | null = null) => () =>
  ok({ documents: docs.map(doc), next_cursor: next })

beforeEach(() => {
  resetImportStore()
  vi.useFakeTimers({ shouldAdvanceTime: true })
})
afterEach(() => {
  vi.useRealTimers()
})

const tick = (ms: number) => act(async () => {
  await vi.advanceTimersByTimeAsync(ms)
})

describe('DocumentsPage: the list', () => {
  it('lists documents and loads the next page once, however often it is clicked', async () => {
    let release: () => void = () => {}
    const m = mount({
      lists: [page([1, 2], 'ab12'), () => new Promise((r) => (release = () => r({ ok: true, status: 200, data: { documents: [doc(3)], next_cursor: null } })))],
    })
    render(<DocumentsPage />)
    await waitFor(() => expect(screen.getByText('通告 2')).toBeInTheDocument())
    expect(screen.getByRole('heading', { name: '文档' })).toBeInTheDocument()
    expect(screen.getByText('本地单用户：文档只保存在这台电脑上。')).toBeInTheDocument()
    const more = screen.getByRole('button', { name: '加载更多' })
    fireEvent.click(more)
    fireEvent.click(more)
    expect(more).toBeDisabled()
    await act(async () => release())
    await waitFor(() => expect(screen.getByText('通告 3')).toBeInTheDocument())
    expect(m.request.mock.calls.filter(([r]) => r.op === 'list' && r.cursor === 'ab12')).toHaveLength(1)
    expect(screen.getAllByText(/^通告 \d$/)).toHaveLength(3)
    expect(screen.queryByRole('button', { name: '加载更多' })).not.toBeInTheDocument()
  })

  it('says when storage is unavailable, with the cause, apart from the list', async () => {
    mount({ capabilities: () => err(503, 'storage_unavailable', 'storage is not available', { retryable: true, cause: 'locked' }) })
    render(<DocumentsPage />)
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('storage_unavailable：locked'))
    expect(screen.getByText('还没有文档。')).toBeInTheDocument()
  })

  it('a page from before a reload is dropped, not mixed into the new list', async () => {
    let releaseOld: () => void = () => {}
    mount({
      lists: [
        page([1], 'ab12'),
        () => new Promise((r) => (releaseOld = () => r({ ok: true, status: 200, data: { documents: [doc(5)], next_cursor: null } }))),
        page([9, 1]),
      ],
      jobs: [() => ok(job('succeeded'))],
    })
    const m = window.agent24.documents as unknown as Mock
    ;(m.importFile as ReturnType<typeof vi.fn>).mockImplementation(() => ok(job('queued')))
    render(<DocumentsPage />)
    await waitFor(() => expect(screen.getByText('通告 1')).toBeInTheDocument())
    fireEvent.click(screen.getByRole('button', { name: '加载更多' })) // stays pending
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100) // the import succeeds and the list reloads
    await waitFor(() => expect(screen.getByText('通告 9')).toBeInTheDocument())
    await act(async () => releaseOld())
    expect(screen.queryByText('通告 5')).not.toBeInTheDocument()
  })

  it('no paging while the list reloads: an old cursor never continues the new list', async () => {
    let releaseNew: () => void = () => {}
    mount({
      lists: [
        page([2, 1], 'c1'),
        () => new Promise((r) => (releaseNew = () => r({ ok: true, status: 200, data: { documents: [doc(9), doc(2)], next_cursor: 'c2' } }))),
        page([0]),
      ],
      jobs: [() => ok(job('succeeded'))],
    })
    const m = window.agent24.documents as unknown as Mock
    ;(m.importFile as ReturnType<typeof vi.fn>).mockImplementation(() => ok(job('queued')))
    render(<DocumentsPage />)
    await waitFor(() => expect(screen.getByRole('button', { name: '加载更多' })).toBeInTheDocument())
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100) // the import succeeds; the reload is pending
    await waitFor(() => expect(m.request.mock.calls.filter(([r]) => r.op === 'list')).toHaveLength(2))
    expect(screen.queryByRole('button', { name: '加载更多' })).not.toBeInTheDocument()
    await act(async () => releaseNew())
    fireEvent.click(screen.getByRole('button', { name: '加载更多' }))
    await waitFor(() => expect(screen.getByText('通告 0')).toBeInTheDocument())
    const cursors = m.request.mock.calls.filter(([r]) => r.op === 'list').map(([r]) => r.cursor)
    expect(cursors).toEqual([undefined, undefined, 'c2'])
  })
})

describe('DocumentsPage: importing', () => {
  it('shows progress, follows the job to the end, then shows the new list', async () => {
    const m = mount({ lists: [page([]), page([9])], jobs: [() => ok(job('running')), () => ok(job('succeeded'))] })
    let resolveImport: (v: unknown) => void = () => {}
    m.importFile.mockImplementation(() => new Promise((r) => (resolveImport = r)))
    render(<DocumentsPage />)
    await waitFor(() => expect(screen.getByText('还没有文档。')).toBeInTheDocument())
    fireEvent.click(screen.getByRole('button', IMPORT))
    expect(screen.getByRole('button', IMPORT)).toBeDisabled()
    act(() => m.progress?.({ sent: 50, total: 200 }))
    expect(screen.getByText('上传 25%')).toBeInTheDocument()
    await act(async () => resolveImport({ ok: true, status: 202, data: job('queued') }))
    expect(screen.getByText('排队中')).toBeInTheDocument()
    await tick(2100)
    await waitFor(() => expect(screen.getByText('通告 9')).toBeInTheDocument())
    expect(screen.getByText('已导入')).toBeInTheDocument()
    expect(m.request.mock.calls.filter(([r]) => r.op === 'job')).toHaveLength(2)
    expect(screen.getByRole('button', IMPORT)).not.toBeDisabled()
  })

  it('keeps following through transient read errors, and stops on a definite one', async () => {
    const m = mount({
      lists: [page([]), page([9])],
      jobs: [() => err(503, 'backend_unreachable', 'down'), () => err(504, 'backend_timeout', 'slow'), () => ok(job('succeeded'))],
    })
    m.importFile.mockImplementation(() => ok(job('queued')))
    render(<DocumentsPage />)
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100 + 2100 + 4100)
    await waitFor(() => expect(screen.getByText('通告 9')).toBeInTheDocument())
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
    // A job the OS no longer knows: no more polling, and it says so.
    m.request.mockImplementation((req: { op: string }) =>
      req.op === 'job' ? err(404, 'not_found', 'no such job') : ok({ documents: [], next_cursor: null }),
    )
    m.importFile.mockImplementation(() => ok(job('queued')))
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100)
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('no such job（not_found）'))
    const reads = m.request.mock.calls.filter(([r]) => r.op === 'job').length
    await tick(5000)
    expect(m.request.mock.calls.filter(([r]) => r.op === 'job').length).toBe(reads)
  })

  it('a read error the OS says is not retryable stops following and says why', async () => {
    const m = mount({
      jobs: [() => err(503, 'storage_unavailable', 'storage is not available', { retryable: false, cause: 'corrupt' })],
    })
    m.importFile.mockImplementation(() => ok(job('queued')))
    render(<DocumentsPage />)
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100)
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('storage_unavailable：corrupt'))
    await tick(20_000)
    expect(m.request.mock.calls.filter(([r]) => r.op === 'job')).toHaveLength(1)
    expect(screen.getByRole('button', IMPORT)).not.toBeDisabled()
  })

  it('an import goes on while the page is away, and is shown on return', async () => {
    const m = mount({ lists: [page([]), page([9])], jobs: [() => ok(job('succeeded'))] })
    m.importFile.mockImplementation(() => ok(job('queued')))
    const first = render(<DocumentsPage />)
    fireEvent.click(screen.getByRole('button', IMPORT))
    await waitFor(() => expect(getImportState().job?.status).toBe('queued'))
    first.unmount()
    await tick(1100)
    expect(getImportState().job?.status).toBe('succeeded')
    render(<DocumentsPage />)
    await waitFor(() => expect(screen.getByText('通告 9')).toBeInTheDocument())
    expect(screen.getByText('已导入')).toBeInTheDocument()
  })

  it('a new import clears the last one; a refused import and a failed job say why', async () => {
    const m = mount({ lists: [page([]), page([9])], jobs: [() => ok(job('succeeded'))] })
    m.importFile.mockImplementation(() => ok(job('queued')))
    render(<DocumentsPage />)
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100)
    await waitFor(() => expect(screen.getByText('已导入')).toBeInTheDocument())
    m.importFile.mockImplementation(() => Promise.resolve(null)) // the dialog is cancelled
    fireEvent.click(screen.getByRole('button', IMPORT))
    await waitFor(() => expect(screen.queryByText('已导入')).not.toBeInTheDocument())
    m.importFile.mockImplementation(() => err(400, 'file_unreadable', 'the file could not be read'))
    fireEvent.click(screen.getByRole('button', IMPORT))
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('the file could not be read（file_unreadable）'))
    m.importFile.mockImplementation(() => ok(job('queued')))
    m.request.mockImplementation((req: { op: string }) =>
      req.op === 'job'
        ? ok(job('failed', { code: 'unsupported_format', message: '不支持这种格式' }))
        : ok({ documents: [], next_cursor: null }),
    )
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100)
    await waitFor(() => expect(screen.getByText('导入失败：不支持这种格式')).toBeInTheDocument())
  })
})
