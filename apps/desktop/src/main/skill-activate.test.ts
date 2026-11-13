import { describe, expect, it, vi, beforeEach } from 'vitest'
import { handleSkillActivate, parseSkillActivateInput, type SkillActivateDeps } from './skill-activate'
import type { BackendProxyRequest, BackendProxyResponse } from '../shared/ipc-types'
import { resetMainLocaleForTest } from './main-i18n'

beforeEach(() => resetMainLocaleForTest())

const DIGEST = 'a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90'
const rev = (id: string, kind: string, origin = 'imported', semver = '1.0.0', digest = DIGEST) =>
  ({ revision_id: id, origin, semver, kind, content_digest: digest, created_at: 't' })

function detail(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    skill_id: 'demo',
    pointer: { origin: 'imported', body_revision_id: 'b1', evals_revision_id: 'e1', generation: 4 },
    generation: 4,
    revisions: [
      rev('b1', 'body'),
      rev('e1', 'evals'),
      rev('b2', 'body', 'imported', '2.0.0', 'ffeeddccbbaa00998877665544332211ffeeddccbbaa00998877665544332211'),
      rev('e2', 'evals', 'imported', '2.0.0'),
      rev('ub', 'body', 'user', '3.0.0', '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'),
      rev('ue', 'evals', 'user', '3.0.0'),
    ],
    activations: [{ activation_id: 'act-1', body_revision_id: 'b1', evals_revision_id: 'e1' }],
    text_consistency: null,
    ...over,
  }
}

function harness(opts: { record?: BackendProxyResponse; confirm?: boolean } = {}) {
  const calls: BackendProxyRequest[] = []
  const confirm = vi.fn(async (_m: string, _d: string) => opts.confirm ?? true)
  const deps: SkillActivateDeps = {
    call: async (req) => {
      calls.push(req)
      if (req.method === 'GET') return opts.record ?? { ok: true, status: 200, data: detail() }
      return { ok: true, status: 200, data: { result: 'activated' } }
    },
    confirm,
  }
  return { deps, calls, confirm }
}

const posts = (calls: BackendProxyRequest[]) => calls.filter((c) => c.method === 'POST')

describe('skills:activate main-process IPC', () => {
  it('builds the four-item summary from the daemon record and posts once after a yes', async () => {
    const h = harness()
    const res = await handleSkillActivate(h.deps, 'activate', {
      skillId: 'demo', bodyRevisionId: 'b2', evalsRevisionId: 'e1',
    })
    expect(res.ok).toBe(true)
    expect(h.confirm).toHaveBeenCalledTimes(1)
    const [message, text] = h.confirm.mock.calls[0]
    expect(message).toContain('"demo"')
    expect(text).toContain('2.0.0')
    expect(text).toContain('ffeeddcc')
    expect(text).not.toContain('ffeeddccb')
    expect(text).toContain('unchanged')
    const [post] = posts(h.calls)
    expect(post.path).toBe('/api/v1/skills/demo/activate')
    expect(post.body).toEqual({ expected_generation: 4, body_revision_id: 'b2', evals_revision_id: 'e1', accepted: [] })
  })

  it('shows that the evals side changed, and a source switch with from and to', async () => {
    const h = harness()
    await handleSkillActivate(h.deps, 'activate', { skillId: 'demo', bodyRevisionId: 'ub', evalsRevisionId: 'ue' })
    const text = h.confirm.mock.calls[0][1]
    expect(text).toContain('changed')
    expect(text).toContain('Imported -> User')
    expect(text).toContain('01234567')
    const changed = harness()
    await handleSkillActivate(changed.deps, 'activate', { skillId: 'demo', bodyRevisionId: 'b1', evalsRevisionId: 'e2' })
    expect(changed.confirm.mock.calls[0][1]).toMatch(/current pointer: changed/)
    expect(changed.confirm.mock.calls[0][1]).not.toContain('Source switch')
  })

  it('ignores a summary, a generation and a verdict sent by the page', async () => {
    const h = harness()
    await handleSkillActivate(h.deps, 'activate', {
      skillId: 'demo', bodyRevisionId: 'b2', evalsRevisionId: 'e1',
      summary: 'FORGED harmless text', message: 'FORGED', expectedGeneration: 99, expected_generation: 99,
      semver: '9.9.9', digest: 'deadbeef', passed: true, score: 1, body: 'x',
    })
    const [message, text] = h.confirm.mock.calls[0]
    expect(`${message}\n${text}`).not.toMatch(/FORGED|9\.9\.9|deadbeef/)
    expect(posts(h.calls)[0].body).toEqual({
      expected_generation: 4, body_revision_id: 'b2', evals_revision_id: 'e1', accepted: [],
    })
    expect(JSON.stringify(posts(h.calls)[0].body)).not.toMatch(/passed|score|FORGED/)
  })

  it('sends no request when the user refuses or closes the dialog', async () => {
    const h = harness({ confirm: false })
    const res = await handleSkillActivate(h.deps, 'activate', { skillId: 'demo', bodyRevisionId: 'b2', evalsRevisionId: 'e1' })
    expect(res.ok).toBe(false)
    expect(res.status).toBe(499)
    expect(posts(h.calls)).toHaveLength(0)
  })

  it('answers 404 or 400, with no dialog and no request, when a revision is not usable in the daemon record', async () => {
    for (const [input, status, code] of [
      [{ skillId: 'demo', bodyRevisionId: 'nope', evalsRevisionId: 'e1' }, 404, 'skill_revision_not_found'],
      [{ skillId: 'demo', bodyRevisionId: 'e1', evalsRevisionId: 'e1' }, 400, 'skill_revision_mismatch'], // an evals id in the body slot
      [{ skillId: 'demo', bodyRevisionId: 'b2', evalsRevisionId: 'ue' }, 400, 'skill_origin_mismatch'], // origins differ
    ] as const) {
      const h = harness()
      const res = await handleSkillActivate(h.deps, 'activate', input)
      expect(res.ok, JSON.stringify(input)).toBe(false)
      expect([res.status, (res.data as { error: { code: string } }).error.code]).toEqual([status, code])
      expect(h.confirm).not.toHaveBeenCalled()
      expect(posts(h.calls)).toHaveLength(0)
    }
    const unreadable = harness({ record: { ok: false, status: 500, data: null } })
    const res = await handleSkillActivate(unreadable.deps, 'activate', { skillId: 'demo', bodyRevisionId: 'b2', evalsRevisionId: 'e1' })
    expect([res.status, (res.data as { error: { code: string } }).error.code]).toEqual([502, 'skill_unavailable'])
    expect(unreadable.confirm).not.toHaveBeenCalled()
    const malformed = harness({ record: { ok: true, status: 200, data: { skill_id: 'demo' } } })
    await handleSkillActivate(malformed.deps, 'activate', { skillId: 'demo', bodyRevisionId: 'b2', evalsRevisionId: 'e1' })
    expect(malformed.confirm).not.toHaveBeenCalled()
    expect(posts(malformed.calls)).toHaveLength(0)
  })

  it('rolls back by history id and names the side that comes back', async () => {
    const h = harness({ record: { ok: true, status: 200, data: detail({
      pointer: { origin: 'imported', body_revision_id: 'b2', evals_revision_id: 'e1', generation: 4 },
    }) } })
    await handleSkillActivate(h.deps, 'rollback', { skillId: 'demo', rollbackActivationId: 'act-1' })
    const [message, text] = h.confirm.mock.calls[0]
    expect(message).toContain('Roll back')
    expect(text).toContain('body comes back')
    expect(posts(h.calls)[0].body).toEqual({ expected_generation: 4, rollback_activation_id: 'act-1' })
    const missing = harness()
    const res = await handleSkillActivate(missing.deps, 'rollback', { skillId: 'demo', rollbackActivationId: 'act-404' })
    expect(res.ok).toBe(false)
    expect(missing.confirm).not.toHaveBeenCalled()
  })

  it('keeps the two channels apart and refuses malformed input', async () => {
    const h = harness()
    const pair = { skillId: 'demo', bodyRevisionId: 'b2', evalsRevisionId: 'e1' }
    expect((await handleSkillActivate(h.deps, 'rollback', pair)).status).toBe(400)
    expect((await handleSkillActivate(h.deps, 'activate', { skillId: 'demo', rollbackActivationId: 'act-1' })).status).toBe(400)
    for (const bad of [
      null, 'demo', {}, { skillId: '../x', bodyRevisionId: 'b2', evalsRevisionId: 'e1' },
      { skillId: 'demo', bodyRevisionId: 'b2', evalsRevisionId: 'e1', rollbackActivationId: 'act-1' },
      { skillId: 'demo', bodyRevisionId: 'b2', evalsRevisionId: 'e1', accepted: 'x' },
      { skillId: 'demo', bodyRevisionId: 'b 2', evalsRevisionId: 'e1' },
    ]) {
      expect((await handleSkillActivate(h.deps, 'activate', bad)).status, JSON.stringify(bad)).toBe(400)
    }
    expect(h.calls).toHaveLength(0)
    expect(parseSkillActivateInput({ skillId: 'demo', bodyRevisionId: 'b2', evalsRevisionId: 'e1', accepted: ['c1'] }))
      .toEqual({ skillId: 'demo', bodyRevisionId: 'b2', evalsRevisionId: 'e1', accepted: ['c1'] })
  })

  it('passes accepted failure ids through and says how many in the dialog', async () => {
    const h = harness()
    await handleSkillActivate(h.deps, 'activate', { skillId: 'demo', bodyRevisionId: 'b2', evalsRevisionId: 'e1', accepted: ['c1', 'c2'] })
    expect(h.confirm.mock.calls[0][1]).toContain('accept: 2')
    expect(posts(h.calls)[0].body).toMatchObject({ accepted: ['c1', 'c2'] })
  })

  it('passes transport and credential failures through instead of folding them into 502', async () => {
    const input = { skillId: 'demo', bodyRevisionId: 'b2', evalsRevisionId: 'e1' }
    for (const record of [
      { ok: false, status: 502, data: { error: { code: 'backend_not_ready', message: 'x' } } },
      { ok: false, status: 504, data: { error: { code: 'timeout', message: 'x' } } },
      { ok: false, status: 401, data: { error: { code: 'unauthorized', message: 'x' } } },
      { ok: false, status: 403, data: { error: { code: 'forbidden', message: 'x' } } },
    ] as BackendProxyResponse[]) {
      const h = harness({ record })
      const res = await handleSkillActivate(h.deps, 'activate', input)
      expect(res).toEqual(record)
      expect(h.confirm).not.toHaveBeenCalled()
      expect(posts(h.calls)).toHaveLength(0)
    }
    const missing = harness({ record: { ok: false, status: 404, data: null } })
    expect((await handleSkillActivate(missing.deps, 'activate', input)).status).toBe(404)
  })

  it('names the source on a first activation, when there is no pointer', async () => {
    const h = harness({ record: { ok: true, status: 200, data: detail({ pointer: null, generation: 0 }) } })
    await handleSkillActivate(h.deps, 'activate', { skillId: 'demo', bodyRevisionId: 'ub', evalsRevisionId: 'ue' })
    const text = h.confirm.mock.calls[0][1]
    expect(text).toContain('Source: User')
    expect(text).toContain('no current pointer')
    expect(text).not.toContain('Source switch')
  })

  it('localizes the source name in the first-activation text, and keeps an unknown one plain', async () => {
    const { setMainLocale } = await import('./main-i18n')
    const run = async () => {
      const h = harness({ record: { ok: true, status: 200, data: detail({ pointer: null, generation: 0 }) } })
      await handleSkillActivate(h.deps, 'activate', { skillId: 'demo', bodyRevisionId: 'ub', evalsRevisionId: 'ue' })
      return h.confirm.mock.calls[0][1] as string
    }
    setMainLocale('zh')
    expect(await run()).toContain('来源：用户')
    setMainLocale('th')
    expect(await run()).toContain('แหล่งที่มา: ผู้ใช้')
  })

  it('says how many failing cases the restored pair accepted, and stays quiet when the detail has no count', async () => {
    const withCount = harness({ record: { ok: true, status: 200, data: detail({
      pointer: { origin: 'imported', body_revision_id: 'b2', evals_revision_id: 'e1', generation: 4 },
      activations: [{ activation_id: 'act-1', body_revision_id: 'b1', evals_revision_id: 'e1', accepted_count: 3 }],
    }) } })
    await handleSkillActivate(withCount.deps, 'rollback', { skillId: 'demo', rollbackActivationId: 'act-1' })
    expect(withCount.confirm.mock.calls[0][1]).toContain('This pair was accepted with 3 failing case(s)')

    const none = harness({ record: { ok: true, status: 200, data: detail({
      pointer: { origin: 'imported', body_revision_id: 'b2', evals_revision_id: 'e1', generation: 4 },
      activations: [{ activation_id: 'act-1', body_revision_id: 'b1', evals_revision_id: 'e1', accepted_count: 0 }],
    }) } })
    await handleSkillActivate(none.deps, 'rollback', { skillId: 'demo', rollbackActivationId: 'act-1' })
    expect(none.confirm.mock.calls[0][1]).not.toContain('failing case')

    const { setMainLocale } = await import('./main-i18n')
    setMainLocale('zh')
    const zh = harness({ record: { ok: true, status: 200, data: detail({
      pointer: { origin: 'imported', body_revision_id: 'b2', evals_revision_id: 'e1', generation: 4 },
      activations: [{ activation_id: 'act-1', body_revision_id: 'b1', evals_revision_id: 'e1', accepted_json: '["a","b"]' }],
    }) } })
    await handleSkillActivate(zh.deps, 'rollback', { skillId: 'demo', rollbackActivationId: 'act-1' })
    expect(zh.confirm.mock.calls[0][1]).toContain('这一对当初接受了 2 条失败用例')
  })

  it('shows no dialog when the pair is already active, and posts so the daemon answers unchanged', async () => {
    const h = harness()
    const res = await handleSkillActivate(h.deps, 'activate', { skillId: 'demo', bodyRevisionId: 'b1', evalsRevisionId: 'e1' })
    expect(res.ok).toBe(true)
    expect(h.confirm).not.toHaveBeenCalled()
    expect(posts(h.calls)[0].body).toEqual({ expected_generation: 4, body_revision_id: 'b1', evals_revision_id: 'e1', accepted: [] })
    // A rollback to the current pair is the same no-op.
    const rb = harness()
    await handleSkillActivate(rb.deps, 'rollback', { skillId: 'demo', rollbackActivationId: 'act-1' })
    expect(rb.confirm).not.toHaveBeenCalled()
    expect(posts(rb.calls)[0].body).toEqual({ expected_generation: 4, rollback_activation_id: 'act-1' })
  })
})
