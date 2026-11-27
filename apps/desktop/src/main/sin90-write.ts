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
const PLAN_KINDS = new Set(['year', 'half', 'quarter', 'month'])
const PLAN_METRICS = new Set(['none', 'count', 'minutes'])
const PLAN_STATUSES = new Set(['draft', 'active', 'done', 'missed', 'dropped'])
const BACKUP_NAME = /^[A-Za-z0-9._-]+\.json$/
const TEMPLATE_LANGS = new Set(['zh', 'en', 'th'])

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
    case 'evaluatePriority': {
      const advice = req['advice']
      if (advice !== undefined && typeof advice !== 'string') return bad()
      return caller.request(route('POST', '/api/v1/sin90/ai/priority/evaluate', advice === undefined ? {} : { advice }), human)
    }
    case 'overridePriority': {
      const task = id(req['id'])
      const quadrant = req['quadrant']
      const rationale = req['rationale']
      if (!task || typeof quadrant !== 'string' || !['do_first', 'schedule', 'delegate', 'eliminate'].includes(quadrant)
        || typeof rationale !== 'string' || rationale.length > 2000) return bad()
      return caller.request(route('PUT', `/api/v1/sin90/tasks/${task}/priority/override`, { quadrant, rationale }), human)
    }
    case 'clearPriorityOverride': {
      const task = id(req['id'])
      return task ? caller.request(route('DELETE', `/api/v1/sin90/tasks/${task}/priority/override`), human) : bad()
    }
    case 'completionNote': {
      const task = id(req['id'])
      const note = req['note']
      if (!task || typeof note !== 'string' || note.length > 4000) return bad()
      return caller.request(route('PUT', `/api/v1/sin90/tasks/${task}/completion-note`, { note }), human)
    }
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
    case 'updateArea': {
      const area = id(req['id'])
      const body: Record<string, unknown> = {}
      for (const field of ['to', 'current_state', 'ideal_state'] as const) {
        if (!(field in req)) continue
        const value = req[field]
        if (field === 'to' ? (value !== 'active' && value !== 'archived') : value !== null && !text(value)) return bad()
        body[field] = value === null ? null : text(value)
      }
      if (!area || Object.keys(body).length === 0) return bad()
      return caller.request(route('PATCH', `/api/v1/sin90/areas/${area}`, body), human)
    }
    case 'updateDirection': {
      const direction = id(req['id'])
      if (!direction) return bad()
      const body: Record<string, unknown> = {}
      for (const [input, output, max] of [['title','title',500],['status','status',32],['targetWindow','target_window',32],['goal3y','goal_3y',2000]] as const) {
        if (!(input in req)) continue
        const value = req[input]
        const nullable = input === 'goal3y'
        if (value === null && !nullable) return bad()
        if (value !== null && !text(value, max)) return bad()
        body[output] = value === null ? null : text(value, max)
      }
      for (const [input, output] of [['areaId','area_id'],['goalFromYear','goal_from_year']] as const) {
        if (!(input in req)) continue
        const value = req[input]
        if (value !== null && !(input === 'goalFromYear' && typeof value === 'number' && Number.isInteger(value) && value >= 1 && value <= 9997) && !(input === 'areaId' && id(value))) return bad()
        body[output] = value
      }
      if (!Object.keys(body).length || (body['status'] !== undefined && !new Set(['draft','active','paused','achieved','abandoned']).has(String(body['status'])))) return bad()
      return caller.request(route('PATCH', `/api/v1/sin90/directions/${direction}`, body), human)
    }
    case 'createPeriodPlan': {
      const direction = id(req['directionId'])
      const kind = req['kind']; const period = text(req['period'], 16); const target = text(req['target'])
      const parent = req['parentId'] === undefined || req['parentId'] === null ? null : id(req['parentId'])
      const metric = req['metric'] ?? 'none'; const metricTarget = req['metricTarget'] ?? null
      const routine = req['routineId'] === undefined || req['routineId'] === null ? null : id(req['routineId'])
      const status = req['status'] ?? 'draft'
      const validPeriod = kind === 'year' ? /^\d{4}$/.test(period ?? '') : kind === 'half' ? /^\d{4}-H[12]$/.test(period ?? '') : kind === 'quarter' ? /^\d{4}-Q[1-4]$/.test(period ?? '') : kind === 'month' ? /^\d{4}-(0[1-9]|1[0-2])$/.test(period ?? '') : false
      if (!direction || typeof kind !== 'string' || !PLAN_KINDS.has(kind) || !period || !validPeriod || !target || (kind === 'year' && parent !== null) || (kind !== 'year' && parent === null) || (req['parentId'] != null && !parent) || typeof metric !== 'string' || !PLAN_METRICS.has(metric) || (metricTarget !== null && (!Number.isInteger(metricTarget) || Number(metricTarget) < 0)) || (req['routineId'] != null && !routine) || typeof status !== 'string' || !PLAN_STATUSES.has(status)) return bad()
      return caller.request(route('POST', '/api/v1/sin90/period-plans', { direction_id: direction, kind, period, parent_id: parent, target, metric, metric_target: metricTarget, routine_id: routine, status }), human)
    }
    case 'updatePeriodPlan': {
      const plan = id(req['id']); if (!plan) return bad()
      const body: Record<string, unknown> = {}
      if ('target' in req) { const value = text(req['target']); if (!value) return bad(); body['target'] = value }
      if ('metric' in req) { if (typeof req['metric'] !== 'string' || !PLAN_METRICS.has(req['metric'])) return bad(); body['metric'] = req['metric'] }
      if ('metricTarget' in req) { const value = req['metricTarget']; if (value !== null && (!Number.isInteger(value) || Number(value) < 0)) return bad(); body['metric_target'] = value }
      if ('routineId' in req) { const value = req['routineId']; if (value !== null && !id(value)) return bad(); body['routine_id'] = value }
      if ('status' in req) { if (typeof req['status'] !== 'string' || !PLAN_STATUSES.has(req['status'])) return bad(); body['status'] = req['status'] }
      if (!Object.keys(body).length) return bad()
      return caller.request(route('PATCH', `/api/v1/sin90/period-plans/${plan}`, body), human)
    }
    case 'putReviewSettings': {
      const { auto, cadence, weekday, hour } = req
      if (typeof auto !== 'boolean' || (cadence !== 'weekly' && cadence !== 'monthly') || !Number.isInteger(weekday) || Number(weekday) < 1 || Number(weekday) > 7 || !Number.isInteger(hour) || Number(hour) < 0 || Number(hour) > 23) return bad()
      return caller.request(route('PUT', '/api/v1/sin90/settings/review', { auto, cadence, weekday, hour }), human)
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
    case 'createWeeklyReview': {
      const period = req['isoWeek']
      return typeof period === 'string' && ISO_WEEK.test(period)
        ? caller.request(route('POST', '/api/v1/sin90/reviews', { kind: 'weekly', period }), human) : bad()
    }
    case 'aiSummarize': {
      const review = id(req['reviewId'])
      return review ? caller.request(route('POST', '/api/v1/sin90/ai/summarize', { review_id: review }), human) : bad()
    }
    case 'dataReset': {
      const hasTemplate = 'template' in req
      const hasLang = 'lang' in req
      if (!hasTemplate && !hasLang) {
        return caller.request(route('POST', '/api/v1/sin90/data/reset', {}), human)
      }
      if (req['template'] !== 'graduate') return bad()
      if (hasLang && (typeof req['lang'] !== 'string' || !TEMPLATE_LANGS.has(req['lang']))) return bad()
      const body: Record<string, unknown> = { template: 'graduate' }
      if (hasLang) body['lang'] = req['lang']
      return caller.request(route('POST', '/api/v1/sin90/data/reset', body), human)
    }
    case 'dataRestoreBackup': {
      const name = req['name']
      if (typeof name !== 'string' || !BACKUP_NAME.test(name)) return bad()
      return caller.request(route('POST', '/api/v1/sin90/data/restore-backup', { name }), human)
    }
    default:
      return bad()
  }
}

export function sin90KeyAvailable(): { available: boolean } {
  return { available: readInstalledHumanKey().ok }
}

export function defaultCaller(): Sin90Caller {
  return {
    key: () => readInstalledHumanKey(),
    request: (sent, actorKey) => sendModuleWrite(
      sent,
      actorKey ? { actorKey } : {},
      { connection: daemonConnection, elevation: daemonElevation },
    ),
  }
}

export function defaultBackupFileName(now: Date = new Date()): string {
  const y = now.getFullYear()
  const m = String(now.getMonth() + 1).padStart(2, '0')
  const d = String(now.getDate()).padStart(2, '0')
  return `sin90-backup-${y}${m}${d}.json`
}

export interface Sin90DialogBridge {
  showSaveDialog: (options: { defaultPath?: string; filters?: Array<{ name: string; extensions: string[] }> }) => Promise<{ canceled: boolean; filePath?: string }>
  showOpenDialog: (options: { properties?: string[]; filters?: Array<{ name: string; extensions: string[] }> }) => Promise<{ canceled: boolean; filePaths?: string[] }>
}

export interface Sin90FsBridge {
  readFile: (path: string, encoding: 'utf-8') => Promise<string>
  writeFile: (path: string, content: string, encoding: 'utf-8') => Promise<void>
}

export const defaultDialogBridge: Sin90DialogBridge = {
  showSaveDialog: async (options) => {
    const { dialog } = await import('electron')
    return dialog.showSaveDialog(options)
  },
  showOpenDialog: async (options) => {
    const { dialog } = await import('electron')
    return dialog.showOpenDialog(options as unknown as Electron.OpenDialogOptions)
  },
}

export const defaultFsBridge: Sin90FsBridge = {
  readFile: async (p, enc) => {
    const { readFile } = await import('node:fs/promises')
    return readFile(p, enc)
  },
  writeFile: async (p, c, enc) => {
    const { writeFile } = await import('node:fs/promises')
    return writeFile(p, c, enc)
  },
}

function sin90Error(code: string, message: string, status = 400): BackendProxyResponse {
  return { ok: false, status, data: { error: { code, message } } }
}

/** Whole-database export/import. Reachable only from the file-dialog IPCs below,
 *  never from the renderer's generic `sin90:write`, so the renderer never holds a
 *  backup and cannot replace the database without the native dialog. */
function dataTransfer(caller: Sin90Caller, path: string, body?: Record<string, unknown>): Promise<BackendProxyResponse> {
  const key = caller.key()
  if (!key.ok) return Promise.resolve(proxyError('forbidden', 'human key unavailable'))
  return caller.request(route('POST', path, body), key.human)
}

export async function exportSin90ToFile(
  caller: Sin90Caller = defaultCaller(),
  dialogBridge: Sin90DialogBridge = defaultDialogBridge,
  fsBridge: Sin90FsBridge = defaultFsBridge,
): Promise<BackendProxyResponse> {
  const exported = await dataTransfer(caller, '/api/v1/sin90/data/export')
  if (!exported.ok) return exported

  let picked: { canceled: boolean; filePath?: string }
  try {
    picked = await dialogBridge.showSaveDialog({
      defaultPath: defaultBackupFileName(),
      filters: [{ name: 'JSON', extensions: ['json'] }],
    })
  } catch (err) {
    return sin90Error('dialog_failed', err instanceof Error ? err.message : 'dialog failed', 500)
  }

  if (picked.canceled || !picked.filePath) {
    return sin90Error('cancelled', 'export cancelled', 499)
  }

  try {
    const jsonText = JSON.stringify(exported.data, null, 2)
    await fsBridge.writeFile(picked.filePath, jsonText, 'utf-8')
    return { ok: true, status: 200, data: { ok: true, path: picked.filePath } }
  } catch (err) {
    return sin90Error('write_failed', err instanceof Error ? err.message : 'failed to write file', 500)
  }
}

export async function importSin90FromFile(
  caller: Sin90Caller = defaultCaller(),
  dialogBridge: Sin90DialogBridge = defaultDialogBridge,
  fsBridge: Sin90FsBridge = defaultFsBridge,
): Promise<BackendProxyResponse> {
  let picked: { canceled: boolean; filePaths?: string[] }
  try {
    picked = await dialogBridge.showOpenDialog({
      properties: ['openFile'],
      filters: [{ name: 'JSON', extensions: ['json'] }],
    })
  } catch (err) {
    return sin90Error('dialog_failed', err instanceof Error ? err.message : 'dialog failed', 500)
  }

  const filePath = picked.filePaths?.[0]
  if (picked.canceled || !filePath) {
    return sin90Error('cancelled', 'import cancelled', 499)
  }

  let text: string
  try {
    text = await fsBridge.readFile(filePath, 'utf-8')
  } catch (err) {
    return sin90Error('read_failed', err instanceof Error ? err.message : 'failed to read file', 500)
  }

  let parsed: unknown
  try {
    parsed = JSON.parse(text)
  } catch {
    return sin90Error('invalid_backup', 'backup file is not valid JSON', 400)
  }

  if (!isObj(parsed) || parsed['format'] !== 'sin90-backup') {
    return sin90Error('invalid_backup', 'backup file must have format "sin90-backup"', 400)
  }

  return dataTransfer(caller, '/api/v1/sin90/data/import', { backup: parsed })
}

/** IPC entry. The human key stays in this process. */
export function desktopSin90Write(req: unknown): Promise<BackendProxyResponse> {
  return performSin90Write(req, defaultCaller())
}

export function desktopSin90ExportToFile(): Promise<BackendProxyResponse> {
  return exportSin90ToFile(defaultCaller())
}

export function desktopSin90ImportFromFile(): Promise<BackendProxyResponse> {
  return importSin90FromFile(defaultCaller())
}
