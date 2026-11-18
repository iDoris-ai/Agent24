import { describe, expect, it, vi } from 'vitest'
import type { BackendProxyRequest, BackendProxyResponse } from '../shared/ipc-types'
import { performSin90Write, type Sin90Caller } from './sin90-write'

const HUMAN = 'h'.repeat(48)

function caller(keyOk: boolean) {
  const request = vi.fn(async (): Promise<BackendProxyResponse> => ({ ok: true, status: 201, data: { id: 't1' } }))
  const api: Sin90Caller = {
    key: () => (keyOk ? { ok: true, human: HUMAN } : { ok: false }),
    request,
  }
  return { api, request }
}

describe('performSin90Write', () => {
  it('does not send when the human key is missing', async () => {
    const { api, request } = caller(false)
    const out = await performSin90Write({ op: 'capture', text: 'a note' }, api)
    expect(out.ok).toBe(false)
    expect(request).not.toHaveBeenCalled()
    expect(JSON.stringify(out)).not.toContain(HUMAN)
  })

  it('posts capture and a task transition with the human key', async () => {
    const { api, request } = caller(true)
    await performSin90Write({ op: 'capture', text: 'a note' }, api)
    await performSin90Write({ op: 'transitionTask', id: 'task_1', to: 'planned' }, api)
    expect(request).toHaveBeenNthCalledWith(1, {
      method: 'POST', path: '/api/v1/sin90/capture', body: { text: 'a note' },
    }, HUMAN)
    expect(request).toHaveBeenNthCalledWith(2, {
      method: 'PATCH', path: '/api/v1/sin90/tasks/task_1', body: { to: 'planned' },
    }, HUMAN)
  })

  it('installs the default life systems pack through the human-key write path', async () => {
    const { api, request } = caller(true)
    expect((await performSin90Write({ op: 'installPack' }, api)).ok).toBe(true)
    expect(request).toHaveBeenCalledWith({ method: 'POST', path: '/api/v1/sin90/packs/install', body: {} }, HUMAN)
  })

  it('refuses an unknown operation and a bad transition without sending', async () => {
    const { api, request } = caller(true)
    expect((await performSin90Write({ op: 'transitionTask', id: 'task_1', to: 'carried_over' }, api)).ok).toBe(false)
    expect((await performSin90Write({ op: 'transitionTask', id: '../x', to: 'done' }, api)).ok).toBe(false)
    expect(request).not.toHaveBeenCalled()
  })

  it('carries a task over by creating the week when it is missing, then a proposal', async () => {
    const calls: BackendProxyRequest[] = []
    const keys: Array<string | null> = []
    const request = vi.fn(async (req: BackendProxyRequest, actorKey: string | null): Promise<BackendProxyResponse> => {
      calls.push(req)
      keys.push(actorKey)
      if (req.path === '/api/v1/sin90/weeks' && req.method === 'GET') return { ok: true, status: 200, data: { weeks: [] } }
      if (req.method === 'POST' && req.path === '/api/v1/sin90/weeks') return { ok: true, status: 201, data: { id: 'week_1', iso_week: '2026-W47' } }
      return { ok: true, status: 202, data: { id: 'p1', status: 'pending' } }
    })
    const out = await performSin90Write({ op: 'carryOver', taskId: 'task_1', isoWeek: '2026-W47' }, {
      key: () => ({ ok: true, human: HUMAN }),
      request,
    })
    expect(out.ok).toBe(true)
    expect(keys).toEqual([null, HUMAN, HUMAN])
    expect(calls[2]?.path).toBe('/api/v1/sin90/proposals')
    const body = calls[2]?.body as { ops: { op: string; to_week: string }[]; source: string; status: string }
    expect(body.ops).toEqual([{ op: 'carry_over_task', task_id: 'task_1', to_week: 'week_1' }])
    expect(body.source).toBe('rule')
    expect(body.status).toBe('pending')
  })
})
