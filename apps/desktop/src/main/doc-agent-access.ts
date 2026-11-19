// D2b: host-side grant of documents.list / get / compose to the Agent.
// Pure of Electron so the grant file and daemon calls can be unit-tested.

import fs from 'node:fs'
import path from 'node:path'

export const DOC_AGENT_TOOLS = ['list', 'get', 'compose'] as const
export const ACCESS_DAYS = 30
export const ACCESS_FILE = 'doc-agent-access.json'
const MAX_RESOURCES = 256
const MODULE = 'documents'

export interface DocAgentAccessDeps {
  dataDir: () => string | null
  /** 发给 daemon 的请求（main 里用 ipc/index.ts 同款 proxyToBackend）。 */
  send: (req: { method: 'GET' | 'POST' | 'DELETE'; path: string; body?: unknown }) => Promise<{ ok: boolean; status: number; data: unknown }>
  /** 当前文档库全部文档 id（最多 256 个，超出截断）。 */
  documentIds: () => Promise<string[]>
  now?: () => number
}

export interface DocAgentAccessState { enabled: boolean; until: string | null }

export type DocAgentAccessResult = DocAgentAccessState & { ok?: boolean; code?: string }

interface AccessFile {
  until: string
  granted: Record<string, string>
}

let queue: Promise<unknown> = Promise.resolve()

function runExclusive<T>(work: () => Promise<T>): Promise<T> {
  const done = queue.then(work, work)
  queue = done.then(() => undefined, () => undefined)
  return done
}

function nowMs(deps: DocAgentAccessDeps): number {
  return deps.now?.() ?? Date.now()
}

function filePath(dir: string): string {
  return path.join(dir, ACCESS_FILE)
}

function readFile(dir: string): AccessFile | null {
  try {
    const raw: unknown = JSON.parse(fs.readFileSync(filePath(dir), 'utf8'))
    if (typeof raw !== 'object' || raw === null) return null
    const until = (raw as { until?: unknown }).until
    if (typeof until !== 'string' || until === '') return null
    const granted: Record<string, string> = {}
    const src = (raw as { granted?: unknown }).granted
    if (typeof src === 'object' && src !== null) {
      for (const [tool, key] of Object.entries(src as Record<string, unknown>)) {
        if (typeof key === 'string') granted[tool] = key
      }
    }
    return { until, granted }
  } catch {
    return null
  }
}

function writeFile(dir: string, data: AccessFile): void {
  fs.writeFileSync(filePath(dir), JSON.stringify({ until: data.until, granted: data.granted }), { encoding: 'utf8', mode: 0o600 })
}

function untilMs(until: string): number | null {
  const t = Date.parse(until)
  return Number.isFinite(t) ? t : null
}

function liveFile(deps: DocAgentAccessDeps): AccessFile | null {
  const dir = deps.dataDir()
  if (dir === null) return null
  const data = readFile(dir)
  if (data === null) return null
  const t = untilMs(data.until)
  if (t === null || t <= nowMs(deps)) return null
  return data
}

function grantKey(fingerprint: string, artifact: string, ids: readonly string[]): string {
  return `${fingerprint}|${artifact}|${[...ids].sort().join(',')}`
}

function responseCode(res: { status: number; data: unknown }): string {
  const err = (res.data as { error?: { code?: unknown } } | null)?.error
  return typeof err?.code === 'string' && err.code !== '' ? err.code : `http_${res.status}`
}

function cardFields(data: unknown): { fingerprint: string; module_version: string; artifact: string } | null {
  if (typeof data !== 'object' || data === null) return null
  const card = data as { fingerprint?: unknown; module_version?: unknown; artifact?: unknown }
  if (typeof card.fingerprint !== 'string' || card.fingerprint === '') return null
  if (typeof card.module_version !== 'string' || card.module_version === '') return null
  if (typeof card.artifact !== 'string' || card.artifact === '') return null
  return { fingerprint: card.fingerprint, module_version: card.module_version, artifact: card.artifact }
}

function toolPath(tool: string, suffix = ''): string {
  return `/api/v1/module-consents/modules/${MODULE}/tools/${encodeURIComponent(tool)}${suffix}`
}

export function state(deps: DocAgentAccessDeps): DocAgentAccessState {
  const data = liveFile(deps)
  return data ? { enabled: true, until: data.until } : { enabled: false, until: null }
}

async function syncUnlocked(deps: DocAgentAccessDeps): Promise<DocAgentAccessResult> {
  const dir = deps.dataDir()
  const data = liveFile(deps)
  if (dir === null || data === null) return { enabled: false, until: null }
  let ids: string[]
  try {
    ids = (await deps.documentIds()).slice(0, MAX_RESOURCES)
  } catch {
    return { enabled: true, until: data.until, ok: false, code: 'list_failed' }
  }
  // A grant the daemon no longer holds as current (stale after the module was
  // turned off and on, revoked elsewhere) is written again even if the card
  // and the documents are the same.
  const listed = await deps.send({ method: 'GET', path: '/api/v1/module-consents' }).catch(() => null)
  const current = new Set(
    ((listed?.data as { authorizations?: Array<{ module?: unknown; tool?: unknown; status?: unknown }> } | null)?.authorizations ?? [])
      .filter((a) => a.module === MODULE && a.status === 'granted')
      .map((a) => String(a.tool)),
  )
  for (const tool of DOC_AGENT_TOOLS) {
    const cardRes = await deps.send({ method: 'GET', path: toolPath(tool, '/card') })
    if (!cardRes.ok) return { enabled: true, until: data.until, ok: false, code: responseCode(cardRes) }
    const card = cardFields(cardRes.data)
    if (card === null) return { enabled: true, until: data.until, ok: false, code: 'invalid_card' }
    const key = grantKey(card.fingerprint, card.artifact, ids)
    if (data.granted[tool] === key && current.has(tool)) continue
    const post = await deps.send({
      method: 'POST',
      path: toolPath(tool),
      body: {
        decision: 'granted',
        fingerprint: card.fingerprint,
        module_version: card.module_version,
        artifact: card.artifact,
        resources: ids.map((id) => `doc:${id}`),
        expires_at: data.until,
      },
    })
    if (!post.ok) return { enabled: true, until: data.until, ok: false, code: responseCode(post) }
    data.granted[tool] = key
    writeFile(dir, data)
  }
  return { enabled: true, until: data.until, ok: true }
}

async function enableUnlocked(deps: DocAgentAccessDeps): Promise<DocAgentAccessResult> {
  const dir = deps.dataDir()
  if (dir === null) return { enabled: false, until: null, ok: false, code: 'no_data_dir' }
  const until = new Date(nowMs(deps) + ACCESS_DAYS * 24 * 60 * 60 * 1000).toISOString()
  writeFile(dir, { until, granted: {} })
  return syncUnlocked(deps)
}

/** Off only once the daemon has ended the grants (2xx), or holds none (404).
 * Otherwise the file stays, so the page still says on, and says the turning
 * off did not happen (PR-Daemon #1915 B2: it said off while the grants lived). */
async function disableUnlocked(deps: DocAgentAccessDeps): Promise<DocAgentAccessResult> {
  const res = await deps.send({ method: 'DELETE', path: `/api/v1/module-consents/modules/${MODULE}` }).catch(() => null)
  if (res === null || !(res.ok || res.status === 404)) {
    return { ...state(deps), ok: false, code: res === null ? 'daemon_unreachable' : responseCode(res) }
  }
  const dir = deps.dataDir()
  if (dir !== null) {
    try { fs.unlinkSync(filePath(dir)) } catch { /* already gone */ }
  }
  return { enabled: false, until: null, ok: true }
}

export function enable(deps: DocAgentAccessDeps): Promise<DocAgentAccessResult> {
  return runExclusive(() => enableUnlocked(deps))
}

export function disable(deps: DocAgentAccessDeps): Promise<DocAgentAccessResult> {
  return runExclusive(() => disableUnlocked(deps))
}

export function sync(deps: DocAgentAccessDeps): Promise<DocAgentAccessResult> {
  return runExclusive(() => syncUnlocked(deps))
}
