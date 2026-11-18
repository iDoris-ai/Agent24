// Sin90 writes. The renderer names an operation. Main checks it against this
// table, attaches the human key, and sends it. The key is not returned.

import { randomBytes } from 'node:crypto'
import type { BackendProxyRequest, BackendProxyResponse, HttpMethod } from '../shared/ipc-types'
import { daemonConnection, daemonElevation } from './backend-manager'
import { proxyError } from './backend-proxy'
import { sendModuleWrite } from './module-delegation'
import { readInstalledHumanKey, type HumanKey } from './sin90-key'

const ID = /^[A-Za-z0-9_-]{1,80}$/
const ISO_WEEK = /^\d{4}-W(0[1-9]|[1-4]\d|5[0-3])$/
const TASK_TO = new Set(['planned', 'in_progress', 'done', 'dropped'])
const WEEK_TO = new Set(['active', 'reviewing', 'closed'])
const BLOCK_TO = new Set(['started', 'completed', 'skipped'])
const ROUTINE_TO = new Set(['active', 'paused'])

export interface Sin90Caller {
  key: () => HumanKey
  /** `actorKey` is null for reads. Writes always pass the human key. */
  request: (req: BackendProxyRequest, actorKey: string | null) => Promise<BackendProxyResponse>
}

function isObj(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}

function text(v: unknown, max = 500): string | null {
  if (typeof v !== 'string') return null
  const trimmed = v.trim()
  if (!trimmed || trimmed.length > max) return null
  return trimmed
}

function id(v: unknown): string | null {
  return typeof v === 'string' && ID.test(v) ? v : null
}

function route(method: HttpMethod, path: string, body?: unknown): BackendProxyRequest {
  return body === undefined ? { method, path } : { method, path, body }
}

function bad(): BackendProxyResponse {
  return proxyError('invalid_request', 'not a Sin90 write this page can send')
}

function weekIdOf(data: unknown, isoWeek: string): string | null {
  if (!isObj(data) || !Array.isArray(data['weeks'])) return null
  for (const row of data['weeks']) {
    if (isObj(row) && row['iso_week'] === isoWeek && typeof row['id'] === 'string' && ID.test(row['id'])) return row['id']
  }
  return null
}

async function carryOver(taskId: string, isoWeek: string, human: string, caller: Sin90Caller): Promise<BackendProxyResponse> {
  const listed = await caller.request(route('GET', '/api/v1/sin90/weeks'), null)
  if (!listed.ok) return listed
  let weekId = weekIdOf(listed.data, isoWeek)
  if (!weekId) {
    const created = await caller.request(route('POST', '/api/v1/sin90/weeks', { iso_week: isoWeek }), human)
    if (!created.ok) return created
    weekId = isObj(created.data) && typeof created.data['id'] === 'string' ? id(created.data['id']) : null
    if (!weekId) return bad()
  }
  return caller.request(route('POST', '/api/v1/sin90/proposals', {
    id: randomBytes(16).toString('hex'),
    status: 'pending',
    source: 'rule',
    ops: [{ op: 'carry_over_task', task_id: taskId, to_week: weekId }],
  }), human)
}

/** One registered write. Refuses before any request when the human key is missing. */
export async function performSin90Write(req: unknown, caller: Sin90Caller): Promise<BackendProxyResponse> {
  if (!isObj(req) || typeof req['op'] !== 'string') return bad()
  const key = caller.key()
  if (!key.ok) return proxyError('forbidden', 'human key unavailable')
  const human = key.human
  switch (req['op']) {
    case 'capture': {
      const value = text(req['text'])
      return value ? caller.request(route('POST', '/api/v1/sin90/capture', { text: value }), human) : bad()
    }
    case 'installPack':
      return caller.request(route('POST', '/api/v1/sin90/packs/install', {}), human)
    case 'createDirection': {
      const title = text(req['title'])
      const target = text(req['targetWindow'], 32)
      if (!title || !target) return bad()
      const area = req['areaId'] === undefined ? undefined : id(req['areaId'])
      if (req['areaId'] !== undefined && !area) return bad()
      return caller.request(route('POST', '/api/v1/sin90/directions', {
        title, target_window: target, ...(area ? { area_id: area } : {}),
      }), human)
    }
    case 'createTask': {
      const title = text(req['title'])
      if (!title) return bad()
      const direction = req['directionId'] === undefined ? undefined : id(req['directionId'])
      if (req['directionId'] !== undefined && !direction) return bad()
      return caller.request(route('POST', '/api/v1/sin90/tasks', {
        title, ...(direction ? { direction_id: direction } : {}),
      }), human)
    }
    case 'transitionTask': {
      const task = id(req['id'])
      const to = req['to']
      if (!task || typeof to !== 'string' || !TASK_TO.has(to)) return bad()
      return caller.request(route('PATCH', `/api/v1/sin90/tasks/${task}`, { to }), human)
    }
    case 'createWeek': {
      const iso = req['isoWeek']
      if (typeof iso !== 'string' || !ISO_WEEK.test(iso)) return bad()
      return caller.request(route('POST', '/api/v1/sin90/weeks', { iso_week: iso }), human)
    }
    case 'transitionWeek': {
      const week = id(req['id'])
      const to = req['to']
      if (!week || typeof to !== 'string' || !WEEK_TO.has(to)) return bad()
      return caller.request(route('PATCH', `/api/v1/sin90/weeks/${week}`, { to }), human)
    }
    case 'createBlock': {
      const minutes = req['plannedMinutes']
      if (typeof minutes !== 'number' || !Number.isInteger(minutes) || minutes < 1 || minutes > 10080) return bad()
      const direction = req['directionId'] === undefined ? undefined : id(req['directionId'])
      const task = req['taskId'] === undefined ? undefined : id(req['taskId'])
      if ((req['directionId'] !== undefined && !direction) || (req['taskId'] !== undefined && !task)) return bad()
      return caller.request(route('POST', '/api/v1/sin90/schedule-blocks', {
        planned_minutes: minutes,
        ...(direction ? { direction_id: direction } : {}),
        ...(task ? { task_id: task } : {}),
      }), human)
    }
    case 'transitionBlock': {
      const block = id(req['id'])
      const to = req['to']
      if (!block || typeof to !== 'string' || !BLOCK_TO.has(to)) return bad()
      return caller.request(route('PATCH', `/api/v1/sin90/schedule-blocks/${block}`, { to }), human)
    }
    case 'transitionRoutine': {
      const routine = id(req['id'])
      const to = req['to']
      if (!routine || typeof to !== 'string' || !ROUTINE_TO.has(to)) return bad()
      return caller.request(route('POST', `/api/v1/sin90/routines/${routine}/transition`, { to }), human)
    }
    case 'carryOver': {
      const task = id(req['taskId'])
      const iso = req['isoWeek']
      if (!task || typeof iso !== 'string' || !ISO_WEEK.test(iso)) return bad()
      return carryOver(task, iso, human, caller)
    }
    case 'acceptProposal': {
      const proposal = id(req['id'])
      return proposal ? caller.request(route('POST', `/api/v1/sin90/proposals/${proposal}/accept`), human) : bad()
    }
    case 'rejectProposal': {
      const proposal = id(req['id'])
      if (!proposal) return bad()
      if (req['reason'] === undefined) return caller.request(route('POST', `/api/v1/sin90/proposals/${proposal}/reject`), human)
      const reason = text(req['reason'], 1000)
      return reason ? caller.request(route('POST', `/api/v1/sin90/proposals/${proposal}/reject`, { reason }), human) : bad()
    }
    case 'aiClassify':
      return caller.request(route('POST', '/api/v1/sin90/ai/classify', {}), human)
    case 'aiPropose': {
      const week = id(req['weekId'])
      return week ? caller.request(route('POST', '/api/v1/sin90/ai/propose', { week_id: week }), human) : bad()
    }
    case 'aiSummarize': {
      const review = id(req['reviewId'])
      return review ? caller.request(route('POST', '/api/v1/sin90/ai/summarize', { review_id: review }), human) : bad()
    }
    default:
      return bad()
  }
}

export function sin90KeyAvailable(): { available: boolean } {
  return { available: readInstalledHumanKey().ok }
}

/** IPC entry. The human key stays in this process. */
export function desktopSin90Write(req: unknown): Promise<BackendProxyResponse> {
  return performSin90Write(req, {
    key: () => readInstalledHumanKey(),
    request: (sent, actorKey) => sendModuleWrite(
      sent,
      actorKey ? { actorKey } : {},
      { connection: daemonConnection, elevation: daemonElevation },
    ),
  })
}
