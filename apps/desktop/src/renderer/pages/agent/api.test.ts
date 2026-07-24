// @vitest-environment jsdom
import { describe, it, expect, vi } from 'vitest'
import {
  errorMessage,
  listRuns,
  getRun,
  cancelRun,
  listSchedules,
  createSchedule,
  updateSchedule,
  deleteSchedule,
  runScheduleNow,
  listPendingApprovals,
  decideApproval,
} from './api'

function setProxy(fn: (req: { method: string; path: string; body?: unknown }) => unknown) {
  window.agent24 = { backendProxy: vi.fn(fn as never) } as never
}

describe('agent api', () => {
  it('errorMessage prefers the envelope message, falls back to status', () => {
    expect(errorMessage({ status: 500, data: { error: { message: 'boom' } } })).toBe('boom')
    expect(errorMessage({ status: 503, data: null })).toBe('HTTP 503')
    expect(errorMessage({ status: 404, data: {} })).toBe('HTTP 404')
  })

  it('list/get/cancel runs unwrap and error correctly', async () => {
    setProxy((req) => {
      if (req.path === '/api/v1/runs') return { ok: true, status: 200, data: { runs: [{ id: 'run_1' }] } }
      if (req.path === '/api/v1/runs/run_1') return { ok: true, status: 200, data: { id: 'run_1' } }
      return { ok: true, status: 202, data: {} }
    })
    expect(await listRuns()).toEqual([{ id: 'run_1' }])
    expect((await getRun('run_1')).id).toBe('run_1')
    await expect(cancelRun('run_1')).resolves.toBeUndefined()
  })

  it('a non-ok response throws the envelope message', async () => {
    setProxy(() => ({ ok: false, status: 500, data: { error: { message: 'db down' } } }))
    await expect(listRuns()).rejects.toThrow('db down')
    await expect(getRun('x')).rejects.toThrow('db down')
    await expect(cancelRun('x')).rejects.toThrow('db down')
  })

  it('schedule CRUD hits the right endpoints', async () => {
    const seen: string[] = []
    setProxy((req) => {
      seen.push(`${req.method} ${req.path}`)
      if (req.method === 'GET') return { ok: true, status: 200, data: { schedules: [] } }
      if (req.path.endsWith('/run_now')) return { ok: true, status: 202, data: { run_id: 'run_5' } }
      return { ok: true, status: 200, data: { id: 'sch_1' } }
    })
    await listSchedules()
    await createSchedule({ name: 'x', spec: { type: 'every', secs: 60 }, action: { type: 'agent_run', prompt: 'p' } })
    await updateSchedule('sch_1', { enabled: false })
    await deleteSchedule('sch_1')
    expect(await runScheduleNow('sch_1')).toBe('run_5')
    expect(seen).toEqual([
      'GET /api/v1/schedules',
      'POST /api/v1/schedules',
      'PATCH /api/v1/schedules/sch_1',
      'DELETE /api/v1/schedules/sch_1',
      'POST /api/v1/schedules/sch_1/run_now',
    ])
  })

  it('schedule mutations surface errors', async () => {
    setProxy(() => ({ ok: false, status: 400, data: { error: { message: 'bad spec' } } }))
    await expect(
      createSchedule({ name: 'x', spec: { type: 'every', secs: 1 }, action: { type: 'agent_run', prompt: 'p' } }),
    ).rejects.toThrow('bad spec')
    await expect(updateSchedule('s', { name: 'y' })).rejects.toThrow('bad spec')
    await expect(deleteSchedule('s')).rejects.toThrow('bad spec')
    await expect(runScheduleNow('s')).rejects.toThrow('bad spec')
    await expect(listSchedules()).rejects.toThrow('bad spec')
  })

  it('decideApproval treats 409 as non-fatal but other errors throw', async () => {
    setProxy(() => ({ ok: false, status: 409, data: {} }))
    await expect(decideApproval('apr_1', { type: 'approve' })).resolves.toBeUndefined()
    setProxy(() => ({ ok: false, status: 400, data: { error: { message: 'nope' } } }))
    await expect(decideApproval('apr_1', { type: 'abort' })).rejects.toThrow('nope')
    setProxy(() => ({ ok: true, status: 200, data: { approvals: [{ id: 'apr_1' }] } }))
    expect(await listPendingApprovals()).toEqual([{ id: 'apr_1' }])
  })

  it('decideApproval rejects an empty deny reason before any proxy call', async () => {
    const proxy = vi.fn()
    window.agent24 = { backendProxy: proxy } as never
    await expect(decideApproval('apr_1', { type: 'deny', reason: '  ' })).rejects.toThrow('原因')
    await expect(decideApproval('apr_1', { type: 'deny' })).rejects.toThrow('原因')
    expect(proxy).not.toHaveBeenCalled()
  })

  it('errorMessage handles a string-shaped error (IPC fallback)', () => {
    expect(errorMessage({ status: 502, data: { error: 'proxy refused' } })).toBe('proxy refused')
  })
})
