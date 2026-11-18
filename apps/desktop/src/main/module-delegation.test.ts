import { describe, expect, it, vi } from 'vitest'
import type { BackendProxyRequest, BackendProxyResponse } from '../shared/ipc-types'
import type { ProxyEndpoint, ProxyOptions } from './backend-proxy'
import { ELEVATION_HEADER, EnrolError, type ElevateRequest } from './daemon-enroll'
import { sendModuleWrite } from './module-delegation'

const REQUIRED: BackendProxyResponse = {
  ok: false,
  status: 403,
  data: { error: { code: 'module_delegation_required', message: 'Module surface requires delegation' } },
}
const PRESENCE: BackendProxyResponse = {
  ok: false,
  status: 403,
  data: { error: { code: 'presence_required', message: 'Presence confirmation required' } },
}
const OS: BackendProxyResponse = {
  ok: true,
  status: 200,
  data: {
    modules: [
      { name: 'docmod', namespace: '/api/v1/documents' },
      { name: 'sin90', namespace: '/api/v1/sin90' },
    ],
  },
}
const DELEGATED: BackendProxyResponse = { ok: true, status: 200, data: { delegated: true, name: 'sin90' } }
const WROTE: BackendProxyResponse = { ok: true, status: 201, data: { id: 'task_1' } }
const GRANT = 'grant-secret-value'
const FORBIDDEN: BackendProxyResponse = {
  ok: false,
  status: 403,
  data: { error: { code: 'forbidden', message: 'no' } },
}

function setup(replies: BackendProxyResponse[], opts: { fail?: unknown } = {}) {
  const sent: { endpoint: ProxyEndpoint; req: BackendProxyRequest; opts: ProxyOptions }[] = []
  const asked: ElevateRequest[] = []
  let token = 'daily-1'
  const send = vi.fn(async (endpoint: ProxyEndpoint, req: BackendProxyRequest, options: ProxyOptions = {}) => {
    sent.push({ endpoint, req, opts: options })
    return replies.shift() ?? { ok: false, status: 500, data: { error: { code: 'unexpected', message: 'no scripted reply' } } }
  })
  const turn = async <T>(fn: (elevate: (request: ElevateRequest) => Promise<string>, token: string) => Promise<T>): Promise<T> => {
    token = 'daily-2'
    return fn(async (request) => {
      asked.push(request)
      if (opts.fail !== undefined) throw opts.fail
      return GRANT
    }, 'daily-2')
  }
  const deps = {
    connection: () => ({ port: 4100, authHeaders: { Authorization: `Bearer ${token}` } }),
    elevation: () => turn,
    send,
  }
  return { deps, sent, asked }
}

const write: BackendProxyRequest = { method: 'POST', path: '/api/v1/sin90/tasks', body: { title: 'a' } }
const paths = (sent: { req: BackendProxyRequest }[], path: string) => sent.filter((s) => s.req.path === path)

describe('sendModuleWrite', () => {
  it('delegates once through elevate and retries the write', async () => {
    const { deps, sent, asked } = setup([REQUIRED, OS, PRESENCE, PRESENCE, DELEGATED, WROTE])
    await expect(sendModuleWrite(write, { actorKey: 'human-key' }, deps)).resolves.toEqual(WROTE)
    expect(asked.map((a) => ({ method: a.method, path: a.path, body: a.body }))).toEqual([
      { method: 'POST', path: '/api/v1/os/sin90/delegation', body: '' },
    ])
    expect(paths(sent, '/api/v1/sin90/tasks')).toHaveLength(2)
    expect(paths(sent, '/api/v1/os/sin90/delegation')).toHaveLength(3)
    expect(paths(sent, '/api/v1/os')).toHaveLength(1)
    const delegation = paths(sent, '/api/v1/os/sin90/delegation')
    expect(delegation[2]?.endpoint.authHeaders[ELEVATION_HEADER]).toBe(GRANT)
    expect(paths(sent, '/api/v1/sin90/tasks').every((s) => s.opts.actorKey === 'human-key')).toBe(true)
    expect(delegation.every((s) => s.opts.actorKey === undefined)).toBe(true)
    expect(paths(sent, '/api/v1/sin90/tasks').every((s) => s.endpoint.authHeaders[ELEVATION_HEADER] === undefined)).toBe(true)
  })

  it('returns a displayable error when the user refuses and does not retry', async () => {
    const { deps, sent, asked } = setup([REQUIRED, OS, PRESENCE, PRESENCE], { fail: new EnrolError('presence_denied', 'no') })
    const res = await sendModuleWrite(write, {}, deps)
    expect(res.ok).toBe(false)
    expect(res.status).toBe(403)
    expect(res.data).toEqual({ error: { code: 'presence_denied', message: 'The action was not confirmed' } })
    expect(paths(sent, '/api/v1/sin90/tasks')).toHaveLength(1)
    expect(paths(sent, '/api/v1/os/sin90/delegation')).toHaveLength(2)
    expect(asked).toHaveLength(1)
    expect(JSON.stringify(res)).not.toContain(GRANT)
  })

  it('does not delegate on an ordinary 403', async () => {
    const { deps, sent, asked } = setup([FORBIDDEN])
    await expect(sendModuleWrite(write, {}, deps)).resolves.toEqual(FORBIDDEN)
    expect(sent).toHaveLength(1)
    expect(asked).toHaveLength(0)
  })

  it('retries a write at most once', async () => {
    const { deps, sent, asked } = setup([REQUIRED, OS, PRESENCE, PRESENCE, DELEGATED, REQUIRED])
    await expect(sendModuleWrite(write, {}, deps)).resolves.toEqual(REQUIRED)
    expect(paths(sent, '/api/v1/sin90/tasks')).toHaveLength(2)
    expect(paths(sent, '/api/v1/os/sin90/delegation')).toHaveLength(3)
    expect(paths(sent, '/api/v1/os')).toHaveLength(1)
    expect(asked).toHaveLength(1)
  })

  it('does not delegate a read', async () => {
    const { deps, sent, asked } = setup([REQUIRED])
    const read: BackendProxyRequest = { method: 'GET', path: '/api/v1/sin90/tasks' }
    await expect(sendModuleWrite(read, {}, deps)).resolves.toEqual(REQUIRED)
    expect(sent).toHaveLength(1)
    expect(asked).toHaveLength(0)
  })

  it('names the module from the namespace, not from a fixed table', async () => {
    const doc: BackendProxyRequest = { method: 'POST', path: '/api/v1/documents/imports' }
    const { deps, sent, asked } = setup([REQUIRED, OS, PRESENCE, PRESENCE, { ...DELEGATED, data: { delegated: true, name: 'docmod' } }, WROTE])
    await expect(sendModuleWrite(doc, {}, deps)).resolves.toEqual(WROTE)
    expect(asked[0]?.path).toBe('/api/v1/os/docmod/delegation')
    expect(paths(sent, '/api/v1/documents/imports')).toHaveLength(2)
  })
})
