// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act, renderHook } from '@testing-library/react'
import type { RunEvent } from '../../../shared/ipc-types'
import { MAX_ASKING, RETRY_FIRST_MS, resetRunStatusesForTests, useRunStatuses } from './run-status'

// M4 A3: the history list's run statuses, against a daemon the test answers
// for, with its /events.

type Answer = { ok: boolean; status: number; data: unknown }
let answer: (runId: string) => Promise<Answer>
let proxy: ReturnType<typeof vi.fn>
let emit: (event: RunEvent) => void
const ok = (status: string): Answer => ({ ok: true, status: 200, data: { status } })
const away: Answer = { ok: false, status: 503, data: { error: { code: 'backend_not_ready' } } }

beforeEach(() => {
  vi.useFakeTimers()
  proxy = vi.fn((req: { path: string }) => answer(decodeURIComponent(req.path.replace('/api/v1/runs/', ''))))
  const listeners = new Set<(e: RunEvent) => void>()
  emit = event => { for (const l of listeners) l(event) }
  Object.defineProperty(window, 'agent24', {
    value: { backendProxy: proxy, onRunEvent: (cb: (e: RunEvent) => void) => { listeners.add(cb); return () => listeners.delete(cb) } },
    writable: true, configurable: true,
  })
  resetRunStatusesForTests()
})
afterEach(() => { resetRunStatusesForTests(); vi.useRealTimers() })

const flush = () => act(async () => { await vi.advanceTimersByTimeAsync(0) })
const wait = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms) })

describe('run statuses for the history list (M4 A3)', () => {
  it('asks again on a timer a run whose answer did not come, e.g. while the daemon starts (Codex A3 C2)', async () => {
    answer = async () => away
    const { result } = renderHook(() => useRunStatuses(['run_1']))
    await flush()
    expect(result.current.get('run_1')).toBeUndefined()
    answer = async () => ok('completed')
    await wait(RETRY_FIRST_MS)
    expect(result.current.get('run_1')).toBe('completed')
    // Known now: no more asking.
    const asked = proxy.mock.calls.length
    await wait(60_000)
    expect(proxy).toHaveBeenCalledTimes(asked)
  })

  it('keeps what was known when the answer after an event does not come, and asks again (Codex A3 C2)', async () => {
    answer = async () => ok('running')
    const { result } = renderHook(() => useRunStatuses(['run_1']))
    await flush()
    answer = async () => away
    act(() => emit({ type: 'run.completed', run_id: 'run_1' }))
    await flush()
    expect(result.current.get('run_1')).toBe('running')
    answer = async () => ok('completed')
    await wait(RETRY_FIRST_MS)
    expect(result.current.get('run_1')).toBe('completed')
  })

  it('asks a run whose answer did not come at once when any event shows the daemon is there', async () => {
    answer = async () => away
    const { result } = renderHook(() => useRunStatuses(['run_1']))
    await flush()
    answer = async () => ok('completed')
    act(() => emit({ type: 'run.started', run_id: 'run_other' }))
    await flush()
    expect(result.current.get('run_1')).toBe('completed')
  })

  it('asks again a run still waiting on an approval the daemon said was decided, until it moves on (Codex A3 C1)', async () => {
    answer = async () => ok('awaiting_approval')
    const { result } = renderHook(() => useRunStatuses(['run_1']))
    await flush()
    // The approval is decided while the run still waits: it goes back to
    // running only after its tool, without an event of its own.
    act(() => emit({ type: 'approval.resolved', run_id: 'run_1' }))
    await flush()
    expect(result.current.get('run_1')).toBe('awaiting_approval')
    answer = async () => ok('running')
    await wait(RETRY_FIRST_MS)
    expect(result.current.get('run_1')).toBe('running')
    const asked = proxy.mock.calls.length
    await wait(60_000)
    expect(proxy).toHaveBeenCalledTimes(asked)
  })

  it('does not keep asking a run that waits on an approval nobody decided yet', async () => {
    answer = async () => ok('awaiting_approval')
    renderHook(() => useRunStatuses(['run_1']))
    await flush()
    act(() => emit({ type: 'approval.required', run_id: 'run_1' }))
    await flush()
    const asked = proxy.mock.calls.length
    await wait(60_000)
    expect(proxy).toHaveBeenCalledTimes(asked)
  })

  it('stops asking once the run moved on, also when it later waits on another approval', async () => {
    answer = async () => ok('awaiting_approval')
    const { result } = renderHook(() => useRunStatuses(['run_1']))
    await flush()
    act(() => emit({ type: 'approval.resolved', run_id: 'run_1' }))
    await flush()
    answer = async () => ok('running')
    await wait(RETRY_FIRST_MS)
    expect(result.current.get('run_1')).toBe('running')
    // Its next tool needs an approval too: nobody has decided that one.
    answer = async () => ok('awaiting_approval')
    act(() => emit({ type: 'approval.required', run_id: 'run_1' }))
    await flush()
    const asked = proxy.mock.calls.length
    await wait(60_000)
    expect(proxy).toHaveBeenCalledTimes(asked)
  })

  it('a late answer does not undo approvals decided while it was on its way (Codex A3 r2 C1)', async () => {
    let late!: (a: Answer) => void
    answer = () => new Promise(resolve => { late = resolve })
    const { result } = renderHook(() => useRunStatuses(['run_1']))
    await flush()
    // While the daemon's first answer ("running") is on its way, the run
    // waits on an approval and the user decides it.
    act(() => emit({ type: 'approval.required', run_id: 'run_1' }))
    act(() => emit({ type: 'approval.resolved', run_id: 'run_1' }))
    answer = async () => ok('awaiting_approval')
    await act(async () => { late(ok('running')) })
    await flush()
    expect(result.current.get('run_1')).toBe('awaiting_approval')
    answer = async () => ok('running')
    await wait(RETRY_FIRST_MS)
    expect(result.current.get('run_1')).toBe('running')
  })

  it('does not keep asking about a second approval nobody decided, even unseen running between (Codex A3 r2 C3)', async () => {
    answer = async () => ok('awaiting_approval')
    renderHook(() => useRunStatuses(['run_1']))
    await flush()
    act(() => emit({ type: 'approval.resolved', run_id: 'run_1' }))
    act(() => emit({ type: 'approval.required', run_id: 'run_1' }))
    await flush()
    const asked = proxy.mock.calls.length
    await wait(60_000)
    expect(proxy).toHaveBeenCalledTimes(asked)
  })

  it('stops asking about runs no list shows any more (Codex A3 r2 C2)', async () => {
    answer = async () => away
    const { rerender, unmount } = renderHook(({ ids }) => useRunStatuses(ids), { initialProps: { ids: ['run_1', 'run_2'] } })
    await flush()
    rerender({ ids: ['run_2'] })
    await wait(RETRY_FIRST_MS)
    const later = proxy.mock.calls.slice(2).map(c => c[0].path)
    expect(later).toContain('/api/v1/runs/run_2')
    expect(later).not.toContain('/api/v1/runs/run_1')
    unmount()
    const asked = proxy.mock.calls.length
    await wait(120_000)
    expect(proxy).toHaveBeenCalledTimes(asked)
  })

  it('while the daemon answers no run, events ask the waiting runs at once only once (Codex A3 r2 C2)', async () => {
    answer = async () => away
    renderHook(() => useRunStatuses(['run_1']))
    await flush()
    act(() => emit({ type: 'run.started', run_id: 'run_other' }))
    await flush()
    const asked = proxy.mock.calls.length
    expect(asked).toBe(2)
    for (let i = 0; i < 5; i++) act(() => emit({ type: 'run.started', run_id: 'run_other' }))
    await flush()
    expect(proxy).toHaveBeenCalledTimes(asked)
  })

  it('keeps a run\'s wait when the list\'s ids change and it is still shown (Codex A3 r3 C1)', async () => {
    answer = async () => ok('awaiting_approval')
    const { result, rerender } = renderHook(({ ids }) => useRunStatuses(ids), { initialProps: { ids: ['run_1'] } })
    await flush()
    act(() => emit({ type: 'approval.resolved', run_id: 'run_1' }))
    await flush()
    // Another conversation gets a task meanwhile.
    rerender({ ids: ['run_1', 'run_2'] })
    await flush()
    answer = async () => ok('running')
    await wait(RETRY_FIRST_MS)
    expect(result.current.get('run_1')).toBe('running')
  })

  it('asks again a run shown anew, whose events went unheard meanwhile (Codex A3 r3 C2)', async () => {
    answer = async () => ok('running')
    const first = renderHook(() => useRunStatuses(['run_1']))
    await flush()
    first.unmount()
    await flush()
    // It ends while no list shows it.
    answer = async () => ok('completed')
    act(() => emit({ type: 'run.completed', run_id: 'run_1' }))
    await flush()
    const { result } = renderHook(() => useRunStatuses(['run_1']))
    await flush()
    expect(result.current.get('run_1')).toBe('completed')
  })

  it('does not ask again, after its answer, a run no list shows any more (Codex A3 r3 C3)', async () => {
    let late!: (a: Answer) => void
    answer = () => new Promise(resolve => { late = resolve })
    const { unmount } = renderHook(() => useRunStatuses(['run_1']))
    await flush()
    act(() => emit({ type: 'run.completed', run_id: 'run_1' }))
    unmount()
    await flush()
    await act(async () => { late(ok('completed')) })
    await flush()
    expect(proxy).toHaveBeenCalledTimes(1)
  })

  it(`asks at most ${MAX_ASKING} runs at once (Codex A3 C3)`, async () => {
    const pending: ((a: Answer) => void)[] = []
    answer = () => new Promise(resolve => { pending.push(resolve) })
    const ids = Array.from({ length: 10 }, (_, i) => `run_${i}`)
    const { result } = renderHook(() => useRunStatuses(ids))
    await flush()
    expect(proxy).toHaveBeenCalledTimes(MAX_ASKING)
    for (let i = 0; i < 10; i++) {
      await act(async () => { pending[i](ok('completed')) })
      await flush()
      expect(proxy.mock.calls.length).toBeLessThanOrEqual(Math.min(10, MAX_ASKING + i + 1))
    }
    expect([...result.current.values()]).toEqual(Array(10).fill('completed'))
  })
})
