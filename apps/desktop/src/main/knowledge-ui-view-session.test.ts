import { describe, expect, it } from 'vitest'
import type { KnowledgeUiClient, UiCallResult } from './knowledge-ui-client'
import { UI_BACKOFF_MS, UI_SELF_CHECK_INTERVAL_MS, UiViewSession, type UiViewSessionDeps } from './knowledge-ui-view-session'

const VIEW = { proof: 'view-proof-secret', vph: 'v'.repeat(43) }
const TAGS = { bindingTag: 'b'.repeat(43), partitionTag: 'p'.repeat(43) }
const SESSION = { sessionToken: 'jwt', expiresInS: 600, ops: ['kb.list' as const], knowledgeBaseIds: ['kb-a'] }
const refused = (code: string, reason: string, retryAfterMs: number | null = null, restarted = false): UiCallResult<never> =>
  ({ kind: 'refused', error: { kind: 'error', code, reason, restarted, retryAfterMs } })

function setup(exchanges: Array<UiCallResult<typeof SESSION>>, opts: { issues?: Array<UiCallResult<never>>; probes?: Array<UiCallResult<{ expiresInS: number }>> } = {}) {
  const log: string[] = []
  const tickets: string[] = []
  let n = 0
  let now = 1000
  const env = { generation: 1, servable: true, credential: 'C' as string | null, onIssue: () => {} }
  const sleeps: number[] = []
  const revokedWith: string[] = []
  let resumes = 0
  const client = {
    issueTicket: async (cred: string, tags: unknown, generation: number) => {
      log.push(`issue:${cred}:${generation}`)
      env.onIssue()
      const queued = opts.issues?.shift()
      if (queued) return queued
      n++
      return { kind: 'ok', value: { ticket: `t${n}`, ticketId: `00000000-0000-4000-8000-00000000000${n}`, expiresInS: 30 } }
    },
    exchange: async (ticket: string) => {
      tickets.push(ticket)
      log.push(`exchange:${ticket}`)
      return exchanges.shift() ?? { kind: 'unknown' }
    },
    probe: async (token: string, view: { proof: string }) => {
      log.push(`probe:${token}:${view.proof}`)
      return opts.probes?.shift() ?? { kind: 'unknown' }
    },
    revokeTicket: async (_c: string, id: string) => { log.push(`revokeTicket:${id.slice(-1)}`); return { kind: 'ok', value: null } },
    revokeFamily: async (c: string, vph: string) => { log.push(`revokeFamily:${vph === VIEW.vph}`); revokedWith.push(c); return { kind: 'ok', value: null } },
  } as unknown as KnowledgeUiClient
  const deps: UiViewSessionDeps = {
    client, tags: TAGS,
    credential: () => env.credential, generation: () => env.generation, servable: () => env.servable,
    now: () => now, sleep: async (ms) => { sleeps.push(ms) },
    onResume: () => { resumes++ },
  }
  return { session: new UiViewSession(deps, { ...VIEW }), log, tickets, env, sleeps, revokedWith, resumes: () => resumes, advance: (ms: number) => { now += ms } }
}

describe('UiViewSession (U2.1, U2.9–U2.12, U3.6–U3.8)', () => {
  it('mints with C and the generation, exchanges once, keeps the JWT', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }])
    expect(await t.session.establish()).toEqual({ kind: 'ready' })
    expect(t.log).toEqual(['issue:C:1', 'exchange:t1'])
    expect(t.session.token()).toBe('jwt')
    expect(t.session.exchangedAt()).toBe(1000)
  })

  it('never sends a ticket twice: an invalid or unknown exchange mints a new one', async () => {
    const t = setup([refused('unauthorized', 'credential_invalid'), { kind: 'unknown' }, { kind: 'ok', value: SESSION }])
    expect(await t.session.establish()).toEqual({ kind: 'ready' })
    expect(t.tickets).toEqual(['t1', 't2', 't3'])
    expect(new Set(t.tickets).size).toBe(t.tickets.length)
  })

  it('makes the second consecutive credential_invalid a view-level handling', async () => {
    const t = setup([refused('unauthorized', 'credential_invalid'), refused('unauthorized', 'credential_invalid')])
    expect(await t.session.establish()).toEqual({ kind: 'view_level' })
    expect(t.session.token()).toBeNull()
  })

  it('closes with probe_failed after three unknown outcomes in a row, backing off between them', async () => {
    const t = setup([{ kind: 'unknown' }, { kind: 'unknown' }, { kind: 'unknown' }])
    expect(await t.session.establish()).toEqual({ kind: 'closed', code: 'probe_failed' })
    expect(t.sleeps).toEqual([UI_BACKOFF_MS, UI_BACKOFF_MS])
  })

  it('counts unknown issuance answers too, and honours a longer retry_after_ms', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }], { issues: [refused('unavailable', 'overloaded', 2500), { kind: 'unknown' }] })
    expect(await t.session.establish()).toEqual({ kind: 'ready' })
    expect(t.sleeps).toEqual([2500, UI_BACKOFF_MS])
  })

  it('a known answer in between starts the unknown count again', async () => {
    const t = setup([{ kind: 'unknown' }, { kind: 'unknown' }, refused('unauthorized', 'credential_invalid'), { kind: 'unknown' }, { kind: 'unknown' }, { kind: 'ok', value: SESSION }])
    expect(await t.session.establish()).toEqual({ kind: 'ready' })
  })

  it('revokes ticket and family and does not exchange when the generation moved (U2.12)', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }])
    t.env.onIssue = () => { t.env.generation = 2 }
    expect(await t.session.establish()).toEqual({ kind: 'generation_changed' })
    expect(t.log).toEqual(['issue:C:1', 'revokeTicket:1', 'revokeFamily:true'])
  })

  it('revokes only the ticket and keeps the view when the binding stops being servable (U2.12)', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }])
    t.env.onIssue = () => { t.env.servable = false }
    expect(await t.session.establish()).toEqual({ kind: 'not_servable' })
    expect(t.log).toEqual(['issue:C:1', 'revokeTicket:1'])
  })

  it('does not mint without C or while not servable', async () => {
    const t = setup([])
    t.env.credential = null
    expect(await t.session.establish()).toEqual({ kind: 'not_servable' })
    t.env.credential = 'C'
    t.env.servable = false
    expect(await t.session.establish()).toEqual({ kind: 'not_servable' })
    expect(t.log).toEqual([])
  })

  it('a destroy during issuance revokes the ticket, and destroy revokes the family once', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }])
    t.env.onIssue = () => t.session.destroy()
    expect(await t.session.establish()).toEqual({ kind: 'destroyed' })
    t.session.destroy()
    expect(t.log).toEqual(['issue:C:1', 'revokeFamily:true', 'revokeTicket:1'])
    expect(t.session.token()).toBeNull()
  })

  it('forgets the JWT on destroy, not only hides it (U2.5)', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }])
    await t.session.establish()
    t.session.destroy()
    expect(t.session.token()).toBeNull()
    expect(t.session.exchangedAt()).toBeNull()
    expect(JSON.stringify(t.session)).not.toContain('jwt')
  })

  it('keeps one exchange in flight per instance', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }])
    const [a, b] = [t.session.establish(), t.session.establish()]
    expect(a).toBe(b)
    await a
    expect(t.log.filter((l) => l.startsWith('issue'))).toHaveLength(1)
  })

  it('hands C problems to the Provider and closes on an empty scope', async () => {
    for (const reason of ['credential_expired', 'credential_revoked']) {
      expect(await setup([refused('unauthorized', reason)]).session.establish()).toEqual({ kind: 'credential', reason })
    }
    expect(await setup([], { issues: [refused('unavailable', 'credential_near_expiry')] }).session.establish())
      .toEqual({ kind: 'credential', reason: 'credential_near_expiry' })
    expect(await setup([], { issues: [refused('forbidden', 'empty_scope')] }).session.establish()).toEqual({ kind: 'closed', code: 'empty_scope' })
  })

  it('asks for renewal once about 2/3 of the JWT life is used (U3.6)', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }])
    expect(t.session.needsRenewal()).toBe(true)
    await t.session.establish()
    t.advance(399_000)
    expect(t.session.needsRenewal()).toBe(false)
    t.advance(1_000)
    expect(t.session.needsRenewal()).toBe(true)
  })

  it('says how long until renewal is due', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }])
    expect(t.session.renewalDelay()).toBe(0)
    await t.session.establish()
    expect(t.session.renewalDelay()).toBe(400_000)
    t.advance(399_000)
    expect(t.session.renewalDelay()).toBe(1_000)
    t.advance(5_000)
    expect(t.session.renewalDelay()).toBe(0)
  })
})

describe('UiViewSession self-check and injection (U3.8, U4.5 ⑤)', () => {
  const OK = { kind: 'ok' as const, value: { expiresInS: 300 } }
  const second = { kind: 'ok' as const, value: { ...SESSION, sessionToken: 'jwt2' } }

  it('hands out JWT and proof only while ready, not paused, not expired and not destroyed', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }])
    expect(t.session.injection()).toBeNull()
    await t.session.establish()
    expect(t.session.injection()).toEqual({ token: 'jwt', proof: VIEW.proof })
    t.advance(599_999)
    expect(t.session.injection()).not.toBeNull()
    t.advance(1)
    expect(t.session.injection()).toBeNull()
    const u = setup([{ kind: 'ok', value: SESSION }])
    await u.session.establish()
    u.session.destroy()
    expect(u.session.injection()).toBeNull()
  })

  it('renews instead of probing when the JWT is at or near its end', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }, second])
    await t.session.establish()
    t.advance(400_000)
    expect(await t.session.selfCheck()).toEqual({ kind: 'ready' })
    expect(t.log.some((l) => l.startsWith('probe:'))).toBe(false)
    expect(t.session.token()).toBe('jwt2')
  })

  it('probes with the JWT and the proof; a good answer does nothing', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }], { probes: [OK] })
    await t.session.establish()
    expect(await t.session.selfCheck()).toEqual({ kind: 'ok' })
    expect(t.log).toEqual(['issue:C:1', 'exchange:t1', `probe:jwt:${VIEW.proof}`])
  })

  it('runs one self-check at a time and at most every 10 seconds', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }], { probes: [OK, OK] })
    await t.session.establish()
    const a = t.session.selfCheck()
    expect(t.session.selfCheck()).toBe(a)
    await a
    expect(await t.session.selfCheck()).toEqual({ kind: 'skipped' })
    t.advance(UI_SELF_CHECK_INTERVAL_MS - 1)
    expect(await t.session.selfCheck()).toEqual({ kind: 'skipped' })
    t.advance(1)
    expect(await t.session.selfCheck()).toEqual({ kind: 'ok' })
    expect(t.log.filter((l) => l.startsWith('probe:'))).toHaveLength(2)
  })

  it('renews on credential_expired', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }, second], { probes: [refused('unauthorized', 'credential_expired')] })
    await t.session.establish()
    expect(await t.session.selfCheck()).toEqual({ kind: 'ready' })
    expect(t.session.token()).toBe('jwt2')
  })

  it('issues once with C on credential_revoked and reports a renewal for the ledger', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }, second], { probes: [refused('unauthorized', 'credential_revoked')] })
    await t.session.establish()
    expect(await t.session.selfCheck()).toEqual({ kind: 'revoked_renewed' })
    expect(t.log.filter((l) => l.startsWith('issue:'))).toHaveLength(2)
    // If C itself is revoked, that goes to the Provider instead.
    const issues: Array<UiCallResult<never>> = []
    const u = setup([{ kind: 'ok', value: SESSION }], { probes: [refused('unauthorized', 'credential_revoked')], issues })
    await u.session.establish()
    issues.push(refused('unauthorized', 'credential_revoked'))
    expect(await u.session.selfCheck()).toEqual({ kind: 'credential', reason: 'credential_revoked' })
  })

  it('makes credential_invalid view-level, carrying restarted and the JWT time, and stops injecting it', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }], { probes: [refused('unauthorized', 'credential_invalid', null, true)] })
    await t.session.establish()
    t.advance(5_000)
    expect(await t.session.selfCheck()).toEqual({ kind: 'session_invalid', restarted: true, jwtExchangedAt: 1000, at: 6000 })
    expect(t.session.injection()).toBeNull()
    const u = setup([{ kind: 'ok', value: SESSION }], { probes: [refused('unauthorized', 'credential_invalid')] })
    await u.session.establish()
    expect(await u.session.selfCheck()).toMatchObject({ kind: 'session_invalid', restarted: false })
  })

  it('pauses injection on an unknown probe, lifts it on the next answer, and closes after three in a row', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }], { probes: [{ kind: 'unknown' }, refused('unavailable', 'overloaded', 3000), OK] })
    await t.session.establish()
    let pausedSeen = false
    t.session['deps'].sleep = async () => { pausedSeen ||= t.session.injection() === null }
    expect(await t.session.selfCheck()).toEqual({ kind: 'ok' })
    expect(pausedSeen).toBe(true)
    expect(t.session.injection()).not.toBeNull()
    const u = setup([{ kind: 'ok', value: SESSION }], { probes: [{ kind: 'unknown' }, refused('unauthorized', 'some_new_reason'), { kind: 'unknown' }] })
    await u.session.establish()
    expect(await u.session.selfCheck()).toEqual({ kind: 'closed', code: 'probe_failed' })
    expect(u.sleeps).toEqual([UI_BACKOFF_MS, UI_BACKOFF_MS])
    expect(u.session.injection()).toBeNull()
  })

  it('lifts the pause when a later answer leads to a renewal', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }, second], { probes: [{ kind: 'unknown' }, refused('unauthorized', 'credential_expired')] })
    await t.session.establish()
    expect(await t.session.selfCheck()).toEqual({ kind: 'ready' })
    expect(t.session.injection()).toEqual({ token: 'jwt2', proof: VIEW.proof })
  })

  it('stops a self-check when the instance goes', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }], { probes: [{ kind: 'unknown' }, OK] })
    await t.session.establish()
    t.session['deps'].sleep = async () => { t.session.destroy() }
    expect(await t.session.selfCheck()).toEqual({ kind: 'destroyed' })
    expect(t.log.filter((l) => l.startsWith('probe:'))).toHaveLength(1)
  })

  it('stops injecting the moment the binding is not servable or its generation moved (Codex 04a-1 R1 #1)', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }])
    await t.session.establish()
    expect(t.session.injection()).not.toBeNull()
    t.env.servable = false
    expect(t.session.injection()).toBeNull()
    t.env.servable = true
    t.env.generation = 2
    expect(t.session.injection()).toBeNull()
    t.env.generation = 1
    expect(t.session.injection()).not.toBeNull()
  })

  it('revokes the family with the C it was issued with, even once the binding has none (Codex 04a-1 R1 #3, U4.7 ③)', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }])
    await t.session.establish()
    t.env.credential = null
    t.session.destroy()
    expect(t.revokedWith).toEqual(['C'])
    // A replaced key: still the family's own C.
    const u = setup([{ kind: 'ok', value: SESSION }])
    await u.session.establish()
    u.env.credential = 'C2'
    u.session.destroy()
    expect(u.revokedWith).toEqual(['C'])
  })

  it('drops the view proof on destroy, not only the JWT (Codex 04a-1 R1 #7, U4.7 ②)', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }])
    await t.session.establish()
    expect(JSON.stringify(t.session)).toContain(VIEW.proof)
    t.session.destroy()
    expect(JSON.stringify(t.session)).not.toContain(VIEW.proof)
    expect(JSON.stringify(t.session)).not.toContain('jwt')
    expect(JSON.stringify(t.session)).not.toContain('"C"')
  })

  it('pauses injection while a renewal exchange ends unknown, and resumes with a reload once renewed (Codex 04a-1 R1 #5, #6)', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }, { kind: 'unknown' }, { kind: 'ok', value: { ...SESSION, sessionToken: 'jwt2' } }])
    await t.session.establish()
    expect(t.resumes()).toBe(0)
    let pausedWhileWaiting: boolean | null = null
    t.session['deps'].sleep = async () => { pausedWhileWaiting = t.session.injection() === null }
    t.advance(400_000)
    expect(await t.session.establish()).toEqual({ kind: 'ready' })
    expect(pausedWhileWaiting).toBe(true)
    expect(t.session.injection()).toEqual({ token: 'jwt2', proof: VIEW.proof })
    expect(t.resumes()).toBe(1)
  })

  it('a probe that recovers lifts the pause with one reload; a renewal with no pause before it reloads nothing', async () => {
    const t = setup([{ kind: 'ok', value: SESSION }], { probes: [{ kind: 'unknown' }, { kind: 'ok', value: { expiresInS: 100 } }] })
    await t.session.establish()
    expect(await t.session.selfCheck()).toEqual({ kind: 'ok' })
    expect(t.resumes()).toBe(1)
    const u = setup([{ kind: 'ok', value: SESSION }, { kind: 'ok', value: SESSION }])
    await u.session.establish()
    u.advance(400_000)
    await u.session.establish()
    expect(u.resumes()).toBe(0)
  })
})
