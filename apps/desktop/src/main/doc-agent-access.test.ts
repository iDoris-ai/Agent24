import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import {
  ACCESS_DAYS,
  ACCESS_FILE,
  DOC_AGENT_TOOLS,
  disable,
  enable,
  state,
  sync,
  type DocAgentAccessDeps,
} from './doc-agent-access'

const NOW = Date.parse('2026-11-18T00:00:00.000Z')
const UNTIL = new Date(NOW + ACCESS_DAYS * 24 * 60 * 60 * 1000).toISOString()
const CARD = { fingerprint: 'fp1', module_version: '0.1.0', artifact: 'art1' }

function cardPath(tool: string): string {
  return `/api/v1/module-consents/modules/documents/tools/${tool}/card`
}
function grantPath(tool: string): string {
  return `/api/v1/module-consents/modules/documents/tools/${tool}`
}

describe('doc-agent-access', () => {
  let dir = ''
  let ids: string[]
  let send: ReturnType<typeof vi.fn>
  let held: Set<string | undefined>
  let cardOk = true

  const deps = (): DocAgentAccessDeps => ({
    dataDir: () => dir,
    send: send as DocAgentAccessDeps['send'],
    documentIds: () => Promise.resolve(ids),
    now: () => NOW,
  })

  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'doc-agent-access-'))
    ids = ['doc_b', 'doc_a']
    cardOk = true
    held = new Set()
    send = vi.fn(async (req: { method: string; path: string; body?: unknown }) => {
      if (req.method === 'GET' && String(req.path).endsWith('/card')) {
        if (!cardOk) return { ok: false, status: 503, data: { error: { code: 'card_unavailable' } } }
        return { ok: true, status: 200, data: CARD }
      }
      if (req.method === 'GET' && req.path === '/api/v1/module-consents') {
        return { ok: true, status: 200, data: { authorizations: [...held].map((tool) => ({ module: 'documents', tool, status: 'granted' })) } }
      }
      if (req.method === 'POST') { held.add(String(req.path).split('/').pop()); return { ok: true, status: 200, data: {} } }
      if (req.method === 'DELETE') return { ok: true, status: 200, data: {} }
      return { ok: false, status: 404, data: { error: { code: 'not_found' } } }
    })
  })

  afterEach(() => {
    fs.rmSync(dir, { recursive: true, force: true })
  })

  it('does not send when access is not enabled', async () => {
    expect(state(deps())).toEqual({ enabled: false, until: null })
    expect(await sync(deps())).toEqual({ enabled: false, until: null })
    expect(send).not.toHaveBeenCalled()
  })

  it('enable GETs a card and POSTs a 30-day grant for each tool', async () => {
    const result = await enable(deps())
    expect(result).toEqual({ enabled: true, until: UNTIL, ok: true })
    expect(state(deps())).toEqual({ enabled: true, until: UNTIL })
    const gets = send.mock.calls.filter(([req]) => req.method === 'GET' && String(req.path).endsWith('/card'))
    const posts = send.mock.calls.filter(([req]) => req.method === 'POST')
    expect(gets.map(([req]) => req.path)).toEqual(DOC_AGENT_TOOLS.map(cardPath))
    expect(posts.map(([req]) => req.path)).toEqual(DOC_AGENT_TOOLS.map(grantPath))
    for (const [req] of posts) {
      expect(req.body).toEqual({
        decision: 'granted',
        fingerprint: CARD.fingerprint,
        module_version: CARD.module_version,
        artifact: CARD.artifact,
        resources: ['doc:doc_b', 'doc:doc_a'],
        expires_at: UNTIL,
      })
    }
    const saved = JSON.parse(fs.readFileSync(path.join(dir, ACCESS_FILE), 'utf8')) as { until: string; granted: Record<string, string> }
    expect(saved.until).toBe(UNTIL)
    const key = `${CARD.fingerprint}|${CARD.artifact}|doc_a,doc_b`
    expect(saved.granted).toEqual({ list: key, get: key, compose: key })
  })

  it('skips POST on a later sync until the document set changes', async () => {
    await enable(deps())
    send.mockClear()
    expect(await sync(deps())).toEqual({ enabled: true, until: UNTIL, ok: true })
    expect(send.mock.calls.filter(([req]) => req.method === 'POST')).toHaveLength(0)
    expect(send.mock.calls.filter(([req]) => req.method === 'GET' && String(req.path).endsWith('/card'))).toHaveLength(DOC_AGENT_TOOLS.length)

    // The daemon no longer holds one (stale after the module was turned off and on): written again.
    held.delete('compose')
    send.mockClear()
    await sync(deps())
    expect(send.mock.calls.filter(([req]) => req.method === 'POST').map(([req]) => req.path)).toEqual([grantPath('compose')])

    ids = ['doc_b', 'doc_a', 'doc_c']
    send.mockClear()
    expect(await sync(deps())).toEqual({ enabled: true, until: UNTIL, ok: true })
    const posts = send.mock.calls.filter(([req]) => req.method === 'POST')
    expect(posts).toHaveLength(DOC_AGENT_TOOLS.length)
    for (const [req] of posts) {
      expect((req.body as { resources: string[] }).resources).toEqual(['doc:doc_b', 'doc:doc_a', 'doc:doc_c'])
    }
  })

  it('returns ok:false and does not write a key when the card fails', async () => {
    cardOk = false
    const result = await enable(deps())
    expect(result).toEqual({ enabled: true, until: UNTIL, ok: false, code: 'card_unavailable' })
    expect(send.mock.calls.filter(([req]) => req.method === 'POST')).toHaveLength(0)
    const after = JSON.parse(fs.readFileSync(path.join(dir, ACCESS_FILE), 'utf8')) as { granted: Record<string, string> }
    expect(after.granted).toEqual({})
  })

  it('treats an expired until as disabled', () => {
    fs.writeFileSync(path.join(dir, ACCESS_FILE), JSON.stringify({
      until: new Date(NOW - 1000).toISOString(),
      granted: { list: 'old' },
    }))
    expect(state(deps())).toEqual({ enabled: false, until: null })
  })

  it('disable deletes the file', async () => {
    await enable(deps())
    expect(fs.existsSync(path.join(dir, ACCESS_FILE))).toBe(true)
    expect(await disable(deps())).toEqual({ enabled: false, until: null, ok: true })
    expect(fs.existsSync(path.join(dir, ACCESS_FILE))).toBe(false)
    expect(send.mock.calls.some(([req]) => req.method === 'DELETE' && req.path === '/api/v1/module-consents/modules/documents')).toBe(true)
    expect(state(deps())).toEqual({ enabled: false, until: null })
  })
})

// PR-Daemon #1915 B2: turning off is not reported done while the grants live.
describe('disable when the daemon does not end the grants', () => {
  it('keeps access on and says so when DELETE answers 503; 404 counts as done', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'doc-agent-access-'))
    let status = 503
    const send = vi.fn(async (req: { method: string; path: string }) => {
      if (req.method === 'DELETE') return { ok: status < 300, status, data: { error: { code: status === 503 ? 'storage_unavailable' : 'not_found' } } }
      if (req.path.endsWith('/card')) return { ok: true, status: 200, data: { fingerprint: 'f', module_version: '1', artifact: 'a' } }
      if (req.path === '/api/v1/module-consents') return { ok: true, status: 200, data: { authorizations: [] } }
      return { ok: true, status: 200, data: {} }
    })
    const d: DocAgentAccessDeps = { dataDir: () => dir, send: send as DocAgentAccessDeps['send'], documentIds: async () => ['doc_a'], now: () => Date.parse('2026-11-19T00:00:00Z') }
    await enable(d)
    const off = await disable(d)
    expect(off).toMatchObject({ enabled: true, ok: false, code: 'storage_unavailable' })
    expect(state(d).enabled).toBe(true)
    status = 404
    expect(await disable(d)).toEqual({ enabled: false, until: null, ok: true })
    expect(state(d).enabled).toBe(false)
    fs.rmSync(dir, { recursive: true, force: true })
  })
})
