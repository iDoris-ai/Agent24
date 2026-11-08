import { describe, expect, it } from 'vitest'
import type { KnowledgeUiClient, UiCallResult } from './knowledge-ui-client'
import { UI_BACKOFF_MS, UiViewSession, type UiViewSessionDeps } from './knowledge-ui-view-session'

const VIEW = { proof: 'proof', vph: 'v'.repeat(43) }
const TAGS = { bindingTag: 'b'.repeat(43), partitionTag: 'p'.repeat(43) }
const SESSION = { sessionToken: 'jwt', expiresInS: 600, ops: ['kb.list' as const], knowledgeBaseIds: ['kb-a'] }
const refused = (code: string, reason: string, retryAfterMs: number | null = null): UiCallResult<never> =>
  ({ kind: 'refused', error: { kind: 'error', code, reason, restarted: false, retryAfterMs } })

function setup(exchanges: Array<UiCallResult<typeof SESSION>>, opts: { issues?: Array<UiCallResult<never>> } = {}) {
  const log: string[] = []
  const tickets: string[] = []
  let n = 0
  let now = 1000
  const env = { generation: 1, servable: true, credential: 'C' as string | null, onIssue: () => {} }
  const sleeps: number[] = []
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
    revokeTicket: async (_c: string, id: string) => { log.push(`revokeTicket:${id.slice(-1)}`); return { kind: 'ok', value: null } },
    revokeFamily: async (_c: string, vph: string) => { log.push(`revokeFamily:${vph === VIEW.vph}`); return { kind: 'ok', value: null } },
  } as unknown as KnowledgeUiClient
  const deps: UiViewSessionDeps = {
    client, view: VIEW, tags: TAGS,
    credential: () => env.credential, generation: () => env.generation, servable: () => env.servable,
    boundKnowledgeBaseIds: () => ['kb-a'], now: () => now, sleep: async (ms) => { sleeps.push(ms) },
  }
  return { session: new UiViewSession(deps), log, tickets, env, sleeps, advance: (ms: number) => { now += ms } }
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
})
