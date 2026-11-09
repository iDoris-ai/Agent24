import { describe, it, expect, vi } from 'vitest'
import type { BackendProxyRequest, BackendProxyResponse } from '../shared/ipc-types'
import type { ProxyEndpoint } from './backend-proxy'
import { EnrolError, type ElevateRequest } from './daemon-enroll'
import { approvalConfirmShown, elevationSummary, presenceRequired, SKIP_APPROVAL_CONFIRM_IN_SERVICE_MODE, sendElevating } from './elevated-request'

// M5 B3: the two-step send, against a scripted daemon.

const PRESENCE_REQUIRED: BackendProxyResponse = { ok: false, status: 403, data: { error: { code: 'presence_required', message: 'Presence confirmation required' } } }
const OK: BackendProxyResponse = { ok: true, status: 200, data: { id: 'sch_1' } }
const GRANT = 'grant-secret-value'

function setup(replies: BackendProxyResponse[], opts: { elevation?: boolean; fail?: unknown } = {}) {
  const sent: { endpoint: ProxyEndpoint; req: BackendProxyRequest }[] = []
  const asked: ElevateRequest[] = []
  let token = 'daily-1'
  const send = vi.fn(async (endpoint: ProxyEndpoint, req: BackendProxyRequest) => {
    sent.push({ endpoint, req })
    return replies.shift() ?? OK
  })
  const elevate = async <T>(request: ElevateRequest, use: (grant: string) => Promise<T>): Promise<T> => {
    asked.push(request)
    if (opts.fail !== undefined) throw opts.fail
    // A renew right before the turn: the retry must carry the token of now.
    token = 'daily-2'
    return use(GRANT)
  }
  const deps = {
    connection: () => ({ port: 4100, authHeaders: { Authorization: `Bearer ${token}` } }),
    elevation: () => (opts.elevation === false ? null : elevate),
    send,
  }
  return { deps, sent, asked }
}

const schedule: BackendProxyRequest = { method: 'POST', path: '/api/v1/schedules', body: { name: 'daily', cron: '0 9 * * *' }, idempotencyKey: 'k1' }

describe('sendElevating (M5 B3)', () => {
  it('on 403 presence_required, elevates once with the exact body and retries once with the grant and the current token', async () => {
    const { deps, sent, asked } = setup([PRESENCE_REQUIRED, OK])
    await expect(sendElevating(schedule, { host: '127.0.0.1' }, deps)).resolves.toEqual(OK)
    expect(asked).toEqual([{ method: 'POST', path: '/api/v1/schedules', body: JSON.stringify(schedule.body), summary: elevationSummary(schedule) }])
    expect(sent).toHaveLength(2)
    expect(sent[0]?.endpoint.authHeaders).toEqual({ Authorization: 'Bearer daily-1' })
    expect(sent[1]?.endpoint.authHeaders).toEqual({ Authorization: 'Bearer daily-2', 'X-Agent24-Elevation': GRANT })
    expect(sent[1]?.req).toBe(schedule)
  })

  it('a retry that is refused again is returned as it is: one elevation, never a second', async () => {
    const { deps, sent, asked } = setup([PRESENCE_REQUIRED, PRESENCE_REQUIRED])
    await expect(sendElevating(schedule, {}, deps)).resolves.toEqual(PRESENCE_REQUIRED)
    expect(asked).toHaveLength(1)
    expect(sent).toHaveLength(2)
  })

  it('hashes no body as the empty string', async () => {
    const { deps, asked } = setup([PRESENCE_REQUIRED, OK])
    await sendElevating({ method: 'POST', path: '/api/v1/shutdown' }, {}, deps)
    expect(asked[0]?.body).toBe('')
  })

  it.each([
    ['an answer that is not 403 presence_required', [OK], true, schedule],
    ['another 403', [{ ok: false, status: 403, data: { error: { code: 'forbidden', message: 'x' } } }], true, schedule],
    ['presence_required with another status', [{ ok: false, status: 401, data: { error: { code: 'presence_required' } } }], true, schedule],
    ['our own child (no enrolment)', [PRESENCE_REQUIRED], false, schedule],
    ['a path with a query (the daemon refuses its grant)', [PRESENCE_REQUIRED], true, { method: 'POST', path: '/api/v1/schedules?x=1', body: {} }],
  ] as const)('%s: one plain request, nothing elevated', async (_name, replies, elevation, req) => {
    const first = replies[0]
    const { deps, sent, asked } = setup([...replies], { elevation })
    await expect(sendElevating(req as BackendProxyRequest, {}, deps)).resolves.toEqual(first)
    expect(sent).toHaveLength(1)
    expect(asked).toEqual([])
  })

  it.each([
    [new EnrolError('presence_denied', 'Denied'), 'presence_denied'],
    [new EnrolError('presence_timeout', 'timed out'), 'presence_timeout'],
    [new Error('boom'), 'internal'],
  ])('a confirmation that does not come answers 403 with its code, and nothing is retried (%s)', async (fail, code) => {
    const { deps, sent } = setup([PRESENCE_REQUIRED], { fail })
    const res = await sendElevating(schedule, {}, deps)
    expect(res).toEqual({ ok: false, status: 403, data: { error: { code, message: 'The action was not confirmed' } } })
    expect(sent).toHaveLength(1)
  })

  it('sends nothing without a connection', async () => {
    const { deps, sent } = setup([])
    const res = await sendElevating(schedule, {}, { ...deps, connection: () => null })
    expect(res.data).toMatchObject({ error: { code: 'backend_not_ready' } })
    expect(sent).toEqual([])
  })

  it('the grant is never in the answer the caller sees', async () => {
    const { deps } = setup([PRESENCE_REQUIRED, { ok: false, status: 403, data: { error: { code: 'elevation_invalid', message: 'Elevation grant is invalid' } } }])
    expect(JSON.stringify(await sendElevating(schedule, {}, deps))).not.toContain(GRANT)
  })
})

describe('approvalConfirmShown (PM 11-09: one switch, off by default)', () => {
  it('is off: every approval keeps its own box, also in service mode', () => {
    expect(SKIP_APPROVAL_CONFIRM_IN_SERVICE_MODE).toBe(false)
    expect(approvalConfirmShown(true)).toBe(true)
    expect(approvalConfirmShown(false)).toBe(true)
  })
  it('switched on, only service mode drops it; our own child keeps it', () => {
    expect(approvalConfirmShown(true, true)).toBe(false)
    expect(approvalConfirmShown(false, true)).toBe(true)
  })
})

describe('presenceRequired', () => {
  it('is only a 403 with the presence_required code', () => {
    expect(presenceRequired(PRESENCE_REQUIRED)).toBe(true)
    expect(presenceRequired({ ok: false, status: 403, data: null })).toBe(false)
    expect(presenceRequired({ ok: false, status: 403, data: { error: 'presence_required' } })).toBe(false)
  })
})
