// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import DocumentsPage from './Documents'
import { readableSize } from './DocumentDetail'
import { cancelImport, getImportState, resetImportStore } from './importStore'
import { setLocale } from '../../i18n'

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
  event: ((e: { kind: string; payload: Record<string, unknown> }) => void) | null
}

/** `list` answers from `lists` in turn (the last one repeats); `job` from `jobs`. */
function mount(opts: { lists?: Array<() => Promise<Res>>; jobs?: Array<() => Promise<Res>>; capabilities?: () => Promise<Res> }): Mock {
  const lists = [...(opts.lists ?? [() => ok({ documents: [], next_cursor: null })])]
  const jobs = [...(opts.jobs ?? [])]
  const m: Mock = { request: vi.fn(), importFile: vi.fn(() => Promise.resolve(null)), progress: null, event: null }
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
      onEvent: (cb: Mock['event']) => {
        m.event = cb
        return () => {
          m.event = null
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

it('renders the English documents title by default', () => {
  mount({})
  render(<DocumentsPage />)
  expect(screen.getByRole('heading', { name: 'Documents' })).toBeInTheDocument()
})

describe('DocumentsPage: the list', () => {
  beforeEach(() => { setLocale('zh') })
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
  beforeEach(() => { setLocale('zh') })
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

describe('DocumentsPage: cancel and retry', () => {
  beforeEach(() => { setLocale('zh') })
  const CANCEL = { name: '取消' }
  const RETRY = { name: '重试' }
  const ops = (m: Mock, op: string) => m.request.mock.calls.filter(([r]) => r.op === op)
  /** `job` reads answer from `reads` in turn (the last one repeats); `cancelJob` / `retryJob` from `acts`. */
  const bases = new WeakMap<Mock, (req: { op: string }) => Promise<Res>>()
  function answer(m: Mock, reads: Array<() => Promise<Res>>, acts: Record<string, () => Promise<Res>> = {}) {
    if (!bases.has(m)) bases.set(m, m.request.getMockImplementation() as (req: { op: string }) => Promise<Res>)
    const base = bases.get(m)!
    m.request.mockImplementation((req: { op: string }) => {
      if (req.op === 'job') return (reads.length > 1 ? reads.shift()! : reads[0]!)()
      if (acts[req.op]) return acts[req.op]!()
      return base(req)
    })
  }
  const attempt = (status: string, n: number, error: unknown = null) => ({ ...job(status, error), attempt: n })

  it('cancels the job being followed, which ends cancelled and can then be restarted', async () => {
    const m = mount({})
    m.importFile.mockImplementation(() => ok(job('queued')))
    let ended: () => void = () => {}
    const cancelled = () => new Promise<Res>((r) => (ended = () => r({ ok: true, status: 200, data: job('cancelled', { code: 'cancelled', message: 'cancelled' }) })))
    answer(m, [() => ok(job('running')), cancelled], { cancelJob: () => ok(job('cancelling')) })
    render(<DocumentsPage />)
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100)
    await waitFor(() => expect(screen.getByText('导入中')).toBeInTheDocument())
    expect(screen.queryByRole('button', RETRY)).not.toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', CANCEL))
    void cancelImport(window.agent24.documents) // while the first is on its way
    await waitFor(() => expect(screen.getByText('取消中')).toBeInTheDocument())
    expect(ops(m, 'cancelJob')).toEqual([[{ op: 'cancelJob', jobId: job('x').job_id }]])
    expect(screen.queryByRole('button', CANCEL)).not.toBeInTheDocument()
    await act(async () => ended())
    await waitFor(() => expect(screen.getByRole('button', RETRY)).toBeInTheDocument())
    expect(screen.getByRole('button', IMPORT)).not.toBeDisabled()
  })

  it('restarts a job that failed for a passing cause and follows it again; not one the file itself fails', async () => {
    const m = mount({ lists: [page([]), page([9])] })
    m.importFile.mockImplementation(() => ok(job('queued')))
    answer(m, [() => ok(job('failed', { code: 'engine_unavailable', message: '引擎不可用' })), () => ok(attempt('succeeded', 2))], {
      retryJob: () => ok(attempt('queued', 2)),
    })
    render(<DocumentsPage />)
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100)
    await waitFor(() => expect(screen.getByText('导入失败：引擎不可用')).toBeInTheDocument())
    fireEvent.click(screen.getByRole('button', RETRY))
    await waitFor(() => expect(screen.getByText('排队中')).toBeInTheDocument())
    expect(screen.getByRole('button', IMPORT)).toBeDisabled()
    await tick(1100)
    await waitFor(() => expect(screen.getByText('通告 9')).toBeInTheDocument())
    expect(ops(m, 'retryJob')).toHaveLength(1)
    expect(screen.queryByRole('button', RETRY)).not.toBeInTheDocument()

    for (const [code, message] of [['unsupported_format', '不支持这种格式'], ['not_found', '上传已过期']]) {
      answer(m, [() => ok(job('failed', { code, message }))])
      fireEvent.click(screen.getByRole('button', IMPORT))
      await tick(1100)
      await waitFor(() => expect(screen.getByText(`导入失败：${message}`)).toBeInTheDocument())
      expect(screen.queryByRole('button', RETRY)).not.toBeInTheDocument()
    }
  })

  it('a retry whose answer is lost is read again a while, never sent again: a later restart is followed', async () => {
    const m = mount({})
    m.importFile.mockImplementation(() => ok(job('queued')))
    answer(m, [() => ok(job('interrupted')), () => ok(job('interrupted')), () => ok(attempt('running', 2)), () => ok(attempt('succeeded', 2))], {
      retryJob: () => err(504, 'backend_timeout', 'slow'),
    })
    render(<DocumentsPage />)
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100)
    fireEvent.click(await screen.findByRole('button', RETRY))
    expect(screen.queryByRole('button', RETRY)).not.toBeInTheDocument()
    await tick(1100) // the restart lands only now
    await waitFor(() => expect(screen.getByText('导入中')).toBeInTheDocument())
    await tick(1100)
    await waitFor(() => expect(screen.getByText('已导入')).toBeInTheDocument())
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
    expect(ops(m, 'retryJob')).toHaveLength(1)
  })

  it('a lost retry answer with no restart in the window says so; a refused retry says why at once', async () => {
    const m = mount({})
    m.importFile.mockImplementation(() => ok(job('queued')))
    answer(m, [() => ok(job('interrupted'))], { retryJob: () => err(504, 'backend_timeout', 'slow') })
    render(<DocumentsPage />)
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100)
    fireEvent.click(await screen.findByRole('button', RETRY))
    await tick(5000)
    expect(screen.queryByRole('button', RETRY)).not.toBeInTheDocument() // still reading
    await tick(6000)
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('无法确定重试是否生效，请稍后再看：slow（backend_timeout）'))
    expect(ops(m, 'retryJob')).toHaveLength(1)
    expect(ops(m, 'job')).toHaveLength(11)

    answer(m, [() => ok(job('interrupted'))], { retryJob: () => err(400, 'invalid_request', '不能重试', { retryable: false }) })
    fireEvent.click(screen.getByRole('button', RETRY))
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('不能重试（invalid_request）'))
    expect(ops(m, 'job')).toHaveLength(11)
  })

  it('a cancel answer older than what was read does not take the job back', async () => {
    const m = mount({})
    m.importFile.mockImplementation(() => ok(job('queued')))
    let answered: () => void = () => {}
    answer(m, [() => ok(job('running')), () => ok(job('cancelled', { code: 'cancelled', message: 'cancelled' }))], {
      cancelJob: () => new Promise<Res>((r) => (answered = () => r({ ok: true, status: 200, data: job('cancelling') }))),
    })
    render(<DocumentsPage />)
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100)
    fireEvent.click(await screen.findByRole('button', CANCEL))
    await tick(1100) // the job is read cancelled before the cancel answers
    await waitFor(() => expect(screen.getByText(/^已取消/)).toBeInTheDocument())
    await act(async () => answered())
    expect(screen.getByText(/^已取消/)).toBeInTheDocument()
    expect(screen.getByRole('button', RETRY)).toBeInTheDocument()
  })

  it('a cancel after following stopped follows the job to its end', async () => {
    const m = mount({})
    m.importFile.mockImplementation(() => ok(job('queued')))
    answer(m, [() => ok(job('running')), () => err(503, 'storage_unavailable', 'storage is not available', { retryable: false, cause: 'corrupt' })])
    render(<DocumentsPage />)
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(2200)
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('storage_unavailable：corrupt'))
    answer(m, [() => ok(job('cancelled', { code: 'cancelled', message: 'cancelled' }))], { cancelJob: () => ok(job('cancelling')) })
    fireEvent.click(screen.getByRole('button', CANCEL))
    await waitFor(() => expect(screen.getByText('取消中')).toBeInTheDocument())
    await tick(1100)
    await waitFor(() => expect(screen.getByText(/^已取消/)).toBeInTheDocument())
    expect(screen.getByRole('button', RETRY)).toBeInTheDocument()
  })
  it('a cancel answering after the job was read failed shows it cancelled, which can be restarted', async () => {
    const m = mount({})
    m.importFile.mockImplementation(() => ok(job('queued')))
    let answered: () => void = () => {}
    answer(m, [() => ok(job('running')), () => ok(job('failed', { code: 'unsupported_format', message: '不支持这种格式' }))], {
      cancelJob: () => new Promise<Res>((r) => (answered = () => r({ ok: true, status: 200, data: job('cancelled', { code: 'cancelled', message: 'cancelled' }) }))),
    })
    render(<DocumentsPage />)
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100)
    fireEvent.click(await screen.findByRole('button', CANCEL))
    await tick(1100) // the job is read failed before the cancel answers
    await waitFor(() => expect(screen.getByText('导入失败：不支持这种格式')).toBeInTheDocument())
    await act(async () => answered())
    expect(screen.getByText(/^已取消/)).toBeInTheDocument()
    expect(screen.getByRole('button', RETRY)).toBeInTheDocument()
    expect(ops(m, 'job')).toHaveLength(2)
  })

  it('a read older than the cancel answer does not take the job back', async () => {
    const m = mount({})
    m.importFile.mockImplementation(() => ok(job('queued')))
    let read: () => void = () => {}
    answer(m, [
      () => ok(job('running')),
      () => new Promise<Res>((r) => (read = () => r({ ok: true, status: 200, data: job('failed', { code: 'engine_unavailable', message: '引擎不可用' }) }))),
    ], { cancelJob: () => ok(job('cancelled', { code: 'cancelled', message: 'cancelled' })) })
    render(<DocumentsPage />)
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100)
    await screen.findByRole('button', CANCEL)
    await tick(1100) // a read is on its way
    fireEvent.click(screen.getByRole('button', CANCEL))
    await waitFor(() => expect(screen.getByText(/^已取消/)).toBeInTheDocument())
    await act(async () => read())
    await tick(1100)
    expect(screen.getByText(/^已取消/)).toBeInTheDocument()
    expect(screen.getByRole('button', RETRY)).toBeInTheDocument()
    expect(ops(m, 'job')).toHaveLength(2)
  })

  it('a read sent before the cancel answered does not take the job back to running', async () => {
    const m = mount({})
    m.importFile.mockImplementation(() => ok(job('queued')))
    let read: () => void = () => {}
    answer(m, [
      () => ok(job('running')),
      () => new Promise<Res>((r) => (read = () => r({ ok: true, status: 200, data: job('running') }))),
      () => ok(job('cancelled', { code: 'cancelled', message: 'cancelled' })),
    ], { cancelJob: () => ok(job('cancelling')) })
    render(<DocumentsPage />)
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100)
    await screen.findByRole('button', CANCEL)
    await tick(1100) // a read is on its way
    fireEvent.click(screen.getByRole('button', CANCEL))
    await waitFor(() => expect(screen.getByText('取消中')).toBeInTheDocument())
    await act(async () => read())
    expect(screen.getByText('取消中')).toBeInTheDocument()
    expect(screen.queryByRole('button', CANCEL)).not.toBeInTheDocument()
    await tick(1100)
    await waitFor(() => expect(screen.getByText(/^已取消/)).toBeInTheDocument())
  })

  it('a read of an earlier attempt does not take a restarted job back', async () => {
    const m = mount({})
    m.importFile.mockImplementation(() => ok(job('queued')))
    answer(m, [() => ok(job('interrupted')), () => ok(job('interrupted')), () => ok(attempt('succeeded', 2))], {
      retryJob: () => ok(attempt('running', 2)),
    })
    render(<DocumentsPage />)
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100)
    fireEvent.click(await screen.findByRole('button', RETRY))
    await tick(1100) // the read still shows attempt 1
    expect(screen.getByText('导入中')).toBeInTheDocument()
    expect(screen.queryByRole('button', RETRY)).not.toBeInTheDocument()
    await tick(1100)
    await waitFor(() => expect(screen.getByText('已导入')).toBeInTheDocument())
  })

  it('a cancel answering after another import started leaves that import shown', async () => {
    const m = mount({})
    m.importFile.mockImplementation(() => ok(job('queued')))
    let answered: () => void = () => {}
    answer(m, [() => ok(job('interrupted')), () => ok(attempt('running', 2)), () => ok(attempt('cancelled', 2, { code: 'cancelled', message: 'cancelled' }))], {
      retryJob: () => ok(attempt('queued', 2)),
      cancelJob: () => new Promise<Res>((r) => (answered = () => r({ ok: true, status: 200, data: attempt('cancelled', 2, { code: 'cancelled', message: 'cancelled' }) }))),
    })
    render(<DocumentsPage />)
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100)
    fireEvent.click(await screen.findByRole('button', RETRY))
    await tick(1100)
    fireEvent.click(await screen.findByRole('button', CANCEL))
    await tick(1100) // the retry's following ends before the cancel answers
    await waitFor(() => expect(screen.getByText(/^已取消/)).toBeInTheDocument())
    const other = (status: string) => ({ ...job(status), job_id: 'job_01K75A0B1C2D3E4F5G6H7J8K00' })
    m.importFile.mockImplementation(() => ok(other('queued')))
    answer(m, [() => ok(other('succeeded'))])
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100)
    await waitFor(() => expect(screen.getByText('已导入')).toBeInTheDocument())
    await act(async () => answered())
    await tick(1100)
    expect(screen.getByText('已导入')).toBeInTheDocument()
    expect(screen.queryByRole('button', RETRY)).not.toBeInTheDocument()
  })

  it('an import is counted once, whether its success is first heard from a read or from a cancel', async () => {
    const lists = () => m.request.mock.calls.filter(([r]) => r.op === 'list')
    const m = mount({})
    m.importFile.mockImplementation(() => ok(job('queued')))
    let answered: () => void = () => {}
    answer(m, [() => ok(job('running')), () => ok(job('succeeded'))], {
      cancelJob: () => new Promise<Res>((r) => (answered = () => r({ ok: true, status: 200, data: job('succeeded') }))),
    })
    render(<DocumentsPage />)
    await waitFor(() => expect(lists()).toHaveLength(1))
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(1100)
    fireEvent.click(await screen.findByRole('button', CANCEL))
    await tick(1100)
    await waitFor(() => expect(screen.getByText('已导入')).toBeInTheDocument())
    await waitFor(() => expect(lists()).toHaveLength(2))
    await act(async () => answered())
    await tick(1100)
    expect(lists()).toHaveLength(2)

    // Following stopped on a read error; the cancel is the first to hear of the success.
    const other = (status: string) => ({ ...job(status), job_id: 'job_01K75A0B1C2D3E4F5G6H7J8K00' })
    m.importFile.mockImplementation(() => ok(other('queued')))
    answer(m, [() => ok(other('running')), () => err(503, 'storage_unavailable', 'storage is not available', { retryable: false, cause: 'corrupt' })], {
      cancelJob: () => ok(other('succeeded')),
    })
    fireEvent.click(screen.getByRole('button', IMPORT))
    await tick(2200)
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('storage_unavailable：corrupt'))
    fireEvent.click(screen.getByRole('button', CANCEL))
    await waitFor(() => expect(screen.getByText('已导入')).toBeInTheDocument())
    await waitFor(() => expect(lists()).toHaveLength(3))
    await tick(1100)
    expect(lists()).toHaveLength(3)
  })
})

describe('DocumentsPage: a document and its text', () => {
  beforeEach(() => { setLocale('zh') })
  const detail = (n: number, size = 2048) => ({
    ...doc(n),
    head: { revision: 2, content_sha256: `sha256:${'a'.repeat(64)}`, size, media_type: 'application/pdf' },
  })
  const block = (id: string, text: string, page?: number) =>
    ({ block_id: id, text, text_sha256: `sha256:${'b'.repeat(64)}`, ...(page ? { page, geometry: { x: 0, y: 0, width: 1, height: 1 } } : {}) })
  const text = (blocks: unknown[], next: string | null = null, regions: unknown[] = []) => ({
    document_id: doc(1).document_id, revision: 2, content_sha256: `sha256:${'a'.repeat(64)}`, media_type: 'application/pdf',
    text_layer_sha256: `sha256:${'c'.repeat(64)}`, engine: { name: 'pdfkit', version: '1' },
    parse_status: regions.length ? 'partial' : 'complete', unparsed_regions: regions, blocks, next_cursor: next,
  })
  /** Answers `get` and `readText` from the queues, the rest as `mount` does. */
  function reading(m: Mock, gets: Array<() => Promise<Res>>, texts: Array<() => Promise<Res>>) {
    const base = m.request.getMockImplementation() as (req: { op: string }) => Promise<Res>
    m.request.mockImplementation((req: { op: string }) => {
      if (req.op === 'get') return gets.shift()!()
      if (req.op === 'readText') return texts.shift()!()
      return base(req)
    })
  }
  const reads = (m: Mock) => m.request.mock.calls.filter(([r]) => r.op === 'readText').map(([r]) => r)

  it('opens a document: its details, its head revision\'s text by page, then the next page on demand', async () => {
    const m = mount({ lists: [page([1])] })
    reading(m, [() => ok(detail(1))], [
      () => ok(text([block('b1', '第一段', 1), block('b2', '第二段', 1), block('b3', '第三段', 2)], 'cc01', [{ reason: 'image', page: 2 }, { reason: 'image', page: 3 }])),
      // Page 3 was cut short and is read on: its region comes again.
      () => ok(text([block('b4', '第四段', 3)], null, [{ reason: 'image', page: 3 }, { reason: 'image', page: 4 }])),
    ])
    render(<DocumentsPage />)
    const title = await screen.findByRole('button', { name: '通告 1' })
    title.focus()
    fireEvent.click(title)
    const panel = await screen.findByRole('region', { name: '文档详情' })
    expect(within(panel).getByRole('heading')).toHaveFocus()
    await waitFor(() => expect(panel).toHaveTextContent('第三段'))
    expect(panel).toHaveTextContent('application/pdf · 2.0 KB · 第 2 版')
    expect(screen.getByRole('note')).toHaveTextContent('部分内容未能识别（涉及 2 页）')
    expect(screen.getAllByText(/^第 \d 页$/).map((e) => e.textContent)).toEqual(['第 1 页', '第 2 页'])
    expect(reads(m)).toEqual([{ op: 'readText', documentId: doc(1).document_id, revision: 2 }])
    fireEvent.click(screen.getByRole('button', { name: '加载更多正文' }))
    await waitFor(() => expect(panel).toHaveTextContent('第四段'))
    expect(screen.getByRole('note')).toHaveTextContent('部分内容未能识别（涉及 3 页）')
    expect(reads(m)[1]).toEqual({ op: 'readText', documentId: doc(1).document_id, revision: 2, cursor: 'cc01' })
    expect(panel).toHaveTextContent('第一段')
    fireEvent.click(screen.getByRole('button', { name: '关闭' }))
    expect(screen.queryByRole('region', { name: '文档详情' })).not.toBeInTheDocument()
    expect(title).toHaveFocus()
  })

  it('a next page read after a failed one clears what the failure said', async () => {
    const m = mount({ lists: [page([1])] })
    reading(m, [() => ok(detail(1))], [
      () => ok(text([block('b1', '第一段', 1)], 'cc01')),
      () => err(503, 'engine_unavailable', '引擎忙'),
      () => ok(text([block('b2', '第二段', 2)])),
    ])
    render(<DocumentsPage />)
    fireEvent.click(await screen.findByRole('button', { name: '通告 1' }))
    fireEvent.click(await screen.findByRole('button', { name: '加载更多正文' }))
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('引擎忙（engine_unavailable）'))
    fireEvent.click(screen.getByRole('button', { name: '加载更多正文' }))
    await waitFor(() => expect(screen.getByText('第二段')).toBeInTheDocument())
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
    expect(reads(m)[2]).toMatchObject({ cursor: 'cc01' })
  })

  it('no text on a page read is not "no text": more may follow, or the pages were not parsed', async () => {
    const m = mount({ lists: [page([1, 2])] })
    reading(m, [() => ok(detail(1)), () => ok(detail(2))], [
      () => ok(text([], 'cc01')),
      () => ok(text([], null, [{ reason: 'image', page: 1 }])),
    ])
    render(<DocumentsPage />)
    fireEvent.click(await screen.findByRole('button', { name: '通告 1' }))
    await screen.findByRole('button', { name: '加载更多正文' })
    expect(screen.queryByText('这份文档没有读出文字。')).not.toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: '通告 2' }))
    await waitFor(() => expect(screen.getByRole('note')).toHaveTextContent('涉及 1 页'))
    expect(screen.queryByText('这份文档没有读出文字。')).not.toBeInTheDocument()
  })

  it('a reply for a document no longer open is dropped', async () => {
    const m = mount({ lists: [page([1, 2])] })
    let late: () => void = () => {}
    reading(m, [() => ok(detail(1)), () => ok(detail(2))], [
      () => new Promise((r) => (late = () => r({ ok: true, status: 200, data: text([block('x', '旧文档的字')]) }))),
      () => ok(text([block('y', '新文档的字')])),
    ])
    render(<DocumentsPage />)
    fireEvent.click(await screen.findByRole('button', { name: '通告 1' }))
    await waitFor(() => expect(reads(m)).toHaveLength(1))
    fireEvent.click(screen.getByRole('button', { name: '通告 2' }))
    await waitFor(() => expect(screen.getByText('新文档的字')).toBeInTheDocument())
    await act(async () => late())
    expect(screen.queryByText('旧文档的字')).not.toBeInTheDocument()
  })

  it('says why the text could not be read, and when none was read', async () => {
    const m = mount({ lists: [page([1, 2])] })
    reading(m, [() => ok(detail(1)), () => ok(detail(2))], [
      () => err(422, 'parse_failed', '解析失败'),
      () => ok(text([])),
    ])
    render(<DocumentsPage />)
    fireEvent.click(await screen.findByRole('button', { name: '通告 1' }))
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('解析失败（parse_failed）'))
    expect(screen.queryByText('这份文档没有读出文字。')).not.toBeInTheDocument() // unread is not empty
    fireEvent.click(screen.getByRole('button', { name: '通告 2' }))
    await waitFor(() => expect(screen.getByText('这份文档没有读出文字。')).toBeInTheDocument())
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
  })

  it('sizes read as bytes, KB or MB', () => {
    expect([readableSize(512), readableSize(1536), readableSize(3 * 1024 * 1024)]).toEqual(['512 B', '1.5 KB', '3.0 MB'])
  })
})

describe('DocumentsPage: OS events (hints only)', () => {
  beforeEach(() => { setLocale('zh') })
  it('an event about the job being followed reads it at once; one about another job does not', async () => {
    const m = mount({ lists: [page([]), page([9])], jobs: [() => ok(job('succeeded'))] })
    m.importFile.mockImplementation(() => ok(job('queued')))
    render(<DocumentsPage />)
    fireEvent.click(screen.getByRole('button', IMPORT))
    await waitFor(() => expect(screen.getByText('排队中')).toBeInTheDocument())
    const reads = () => m.request.mock.calls.filter(([r]) => r.op === 'job').length
    act(() => m.event?.({ kind: 'job.finished', payload: { job_id: 'job_01K75A0B1C2D3E4F5G6H7J8K00' } }))
    await tick(10)
    expect(reads()).toBe(0)
    act(() => m.event?.({ kind: 'job.finished', payload: { job_id: job('x').job_id } }))
    await tick(10)
    expect(reads()).toBe(1)
    await waitFor(() => expect(screen.getByText('通告 9')).toBeInTheDocument())
  })

  it('a document imported from anywhere reloads the list; after leaving, events are not heard', async () => {
    const m = mount({ lists: [page([]), page([7])] })
    const { unmount } = render(<DocumentsPage />)
    await waitFor(() => expect(screen.getByText('还没有文档。')).toBeInTheDocument())
    act(() => m.event?.({ kind: 'document.imported', payload: { document_id: doc(7).document_id } }))
    await waitFor(() => expect(screen.getByText('通告 7')).toBeInTheDocument())
    unmount()
    expect(m.event).toBeNull()
  })
})
