import { describe, expect, it, vi } from 'vitest'
import type { BackendProxyRequest, BackendProxyResponse } from '../shared/ipc-types'
import {
  exportSin90ToFile, importSin90FromFile, performSin90Write,
  type Sin90Caller, type Sin90DialogBridge, type Sin90FsBridge,
} from './sin90-write'

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

  it('validates every new Sin90 write before sending and maps accepted payloads', async () => {
    const { api, request } = caller(true)
    for (const value of [
      { op: 'updateArea', id: 'a1' }, { op: 'updateArea', id: 'a1', ideal_state: '' },
      { op: 'updateDirection', id: '../d', goal3y: 'goal' }, { op: 'updateDirection', id: 'd1' },
      { op: 'createPeriodPlan', directionId: 'd1', kind: 'century', period: 'x', target: 'goal' },
      { op: 'createPeriodPlan', directionId: 'd1', kind: 'year', period: '2026', target: 'goal', parentId: 'p1' },
      { op: 'updatePeriodPlan', id: 'p1' }, { op: 'updatePeriodPlan', id: 'p1', status: 'active-ish' },
      { op: 'putReviewSettings', auto: true, cadence: 'daily', weekday: 8, hour: 25 },
    ]) expect((await performSin90Write(value, api)).ok).toBe(false)
    expect(request).not.toHaveBeenCalled()
    await performSin90Write({ op: 'updateDirection', id: 'd1', goal3y: '旅行', goalFromYear: 2026 }, api)
    await performSin90Write({ op: 'createPeriodPlan', directionId: 'd1', kind: 'year', period: '2026', target: '健康', metric: 'count', metricTarget: 12 }, api)
    await performSin90Write({ op: 'putReviewSettings', auto: false, cadence: 'monthly', weekday: 1, hour: 9 }, api)
    expect(request).toHaveBeenNthCalledWith(1, { method: 'PATCH', path: '/api/v1/sin90/directions/d1', body: { goal_3y: '旅行', goal_from_year: 2026 } }, HUMAN)
    expect(request).toHaveBeenNthCalledWith(2, { method: 'POST', path: '/api/v1/sin90/period-plans', body: { direction_id: 'd1', kind: 'year', period: '2026', parent_id: null, target: '健康', metric: 'count', metric_target: 12, routine_id: null, status: 'draft' } }, HUMAN)
    expect(request).toHaveBeenNthCalledWith(3, { method: 'PUT', path: '/api/v1/sin90/settings/review', body: { auto: false, cadence: 'monthly', weekday: 1, hour: 9 } }, HUMAN)
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

  it('validates and performs dataReset, dataRestoreBackup, dataExport, dataImport', async () => {
    const { api, request } = caller(true)

    // Illegal requests
    for (const badReq of [
      { op: 'dataReset', template: 'freshman' },
      { op: 'dataReset', template: 'graduate', lang: 'fr' },
      { op: 'dataReset', lang: 'zh' }, // missing template
      { op: 'dataRestoreBackup', name: '../escape.json' },
      { op: 'dataRestoreBackup', name: 'notjson.txt' },
      { op: 'dataRestoreBackup', name: '' },
      { op: 'dataRestoreBackup' },
      { op: 'dataImport' },
      { op: 'dataImport', backup: null },
      { op: 'dataImport', backup: { format: 'other-format' } },
    ]) {
      expect((await performSin90Write(badReq, api)).ok).toBe(false)
    }
    expect(request).not.toHaveBeenCalled()

    // Valid dataReset (empty)
    await performSin90Write({ op: 'dataReset' }, api)
    expect(request).toHaveBeenNthCalledWith(1, { method: 'POST', path: '/api/v1/sin90/data/reset', body: {} }, HUMAN)

    // Valid dataReset (template + lang)
    await performSin90Write({ op: 'dataReset', template: 'graduate', lang: 'zh' }, api)
    expect(request).toHaveBeenNthCalledWith(2, { method: 'POST', path: '/api/v1/sin90/data/reset', body: { template: 'graduate', lang: 'zh' } }, HUMAN)

    // Valid dataRestoreBackup
    await performSin90Write({ op: 'dataRestoreBackup', name: 'auto-20261119-1234.json' }, api)
    expect(request).toHaveBeenNthCalledWith(3, { method: 'POST', path: '/api/v1/sin90/data/restore-backup', body: { name: 'auto-20261119-1234.json' } }, HUMAN)

    // Valid dataExport
    await performSin90Write({ op: 'dataExport' }, api)
    expect(request).toHaveBeenNthCalledWith(4, { method: 'POST', path: '/api/v1/sin90/data/export' }, HUMAN)

    // Valid dataImport
    const backupObj = { format: 'sin90-backup', schema: 18, tables: {} }
    await performSin90Write({ op: 'dataImport', backup: backupObj }, api)
    expect(request).toHaveBeenNthCalledWith(5, { method: 'POST', path: '/api/v1/sin90/data/import', body: { backup: backupObj } }, HUMAN)
  })

  it('exportSin90ToFile handles success, cancel, and failures', async () => {
    const { api, request } = caller(true)
    const written: Record<string, string> = {}
    const dialogBridge: Sin90DialogBridge = {
      showSaveDialog: vi.fn(async () => ({ canceled: false, filePath: '/tmp/test-backup.json' })),
      showOpenDialog: vi.fn(),
    }
    const fsBridge: Sin90FsBridge = {
      readFile: vi.fn(),
      writeFile: vi.fn(async (p, c) => { written[p] = c }),
    }

    // Success
    request.mockResolvedValueOnce({ ok: true, status: 200, data: { format: 'sin90-backup', exported_at: '2026-11-19' } })
    const res = await exportSin90ToFile(api, dialogBridge, fsBridge)
    expect(res.ok).toBe(true)
    expect(written['/tmp/test-backup.json']).toContain('sin90-backup')

    // User canceled
    dialogBridge.showSaveDialog = vi.fn(async () => ({ canceled: true }))
    request.mockResolvedValueOnce({ ok: true, status: 200, data: { format: 'sin90-backup' } })
    const cancelRes = await exportSin90ToFile(api, dialogBridge, fsBridge)
    expect(cancelRes.ok).toBe(false)
    expect(cancelRes.status).toBe(499)

    // Write file failed
    dialogBridge.showSaveDialog = vi.fn(async () => ({ canceled: false, filePath: '/tmp/bad.json' }))
    fsBridge.writeFile = vi.fn(async () => { throw new Error('disk full') })
    request.mockResolvedValueOnce({ ok: true, status: 200, data: { format: 'sin90-backup' } })
    const writeErrRes = await exportSin90ToFile(api, dialogBridge, fsBridge)
    expect(writeErrRes.ok).toBe(false)
    expect(writeErrRes.status).toBe(500)
  })

  it('importSin90FromFile handles success, cancel, invalid JSON, and bad format', async () => {
    const { api, request } = caller(true)
    request.mockResolvedValue({ ok: true, status: 200, data: { ok: true } })

    const dialogBridge: Sin90DialogBridge = {
      showSaveDialog: vi.fn(),
      showOpenDialog: vi.fn(async () => ({ canceled: false, filePaths: ['/tmp/import.json'] })),
    }
    let fileContent = JSON.stringify({ format: 'sin90-backup', schema: 18 })
    const fsBridge: Sin90FsBridge = {
      readFile: vi.fn(async () => fileContent),
      writeFile: vi.fn(),
    }

    // Success
    const res = await importSin90FromFile(api, dialogBridge, fsBridge)
    expect(res.ok).toBe(true)
    expect(request).toHaveBeenCalledWith({
      method: 'POST',
      path: '/api/v1/sin90/data/import',
      body: { backup: { format: 'sin90-backup', schema: 18 } },
    }, HUMAN)

    // Cancel
    dialogBridge.showOpenDialog = vi.fn(async () => ({ canceled: true, filePaths: [] }))
    const cancelRes = await importSin90FromFile(api, dialogBridge, fsBridge)
    expect(cancelRes.ok).toBe(false)
    expect(cancelRes.status).toBe(499)

    // Invalid JSON
    dialogBridge.showOpenDialog = vi.fn(async () => ({ canceled: false, filePaths: ['/tmp/corrupt.json'] }))
    fileContent = '{ bad json'
    const jsonErrRes = await importSin90FromFile(api, dialogBridge, fsBridge)
    expect(jsonErrRes.ok).toBe(false)
    expect(jsonErrRes.status).toBe(400)

    // Bad format
    fileContent = JSON.stringify({ format: 'random-format' })
    const formatErrRes = await importSin90FromFile(api, dialogBridge, fsBridge)
    expect(formatErrRes.ok).toBe(false)
    expect(formatErrRes.status).toBe(400)
  })
})
