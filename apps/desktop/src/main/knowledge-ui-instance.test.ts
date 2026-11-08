import { readdirSync, readFileSync } from 'node:fs'
import path from 'node:path'
import { describe, expect, it } from 'vitest'
import type { KnowledgeUiClient, UiCallResult } from './knowledge-ui-client'
import type { InjectRequest } from './knowledge-ui-inject'
import { KnowledgeUiInstance, type UiInstanceEvent } from './knowledge-ui-instance'
import { parseUiRoutes } from './knowledge-ui-routes'
import { UiSessionLedger } from './knowledge-ui-session'

const ORIGIN = 'http://127.0.0.1:18282'
const ROUTES = parseUiRoutes({ routes: [{ method: 'GET', path: '/api/v1/agent24/knowledge/ui/knowledge-bases', op: 'kb.list' }] })
const VIEW = { proof: 'proof', vph: 'v'.repeat(43) }
const WC = 3
const exchanged = (token: string): UiCallResult<never> =>
  ({ kind: 'ok', value: { sessionToken: token, expiresInS: 600, ops: ['kb.list'], knowledgeBaseIds: ['kb-a'] } } as never)
const refused = (code: string, reason: string, restarted = false): UiCallResult<never> =>
  ({ kind: 'refused', error: { kind: 'error', code, reason, restarted, retryAfterMs: null } })

function setup(script: { exchanges?: UiCallResult<never>[]; probes?: UiCallResult<never>[]; issues?: UiCallResult<never>[] } = {}, ledger = new UiSessionLedger()) {
  let now = 1000
  let tokens = 0
  const log: string[] = []
  const events: UiInstanceEvent[] = []
  const timers: Array<{ fn: () => void; ms: number; cleared: boolean }> = []
  const client = {
    issueTicket: async () => {
      log.push('issue')
      return script.issues?.shift() ?? { kind: 'ok', value: { ticket: `t${++tokens}`, ticketId: `00000000-0000-4000-8000-00000000000${tokens}`, expiresInS: 30 } }
    },
    exchange: async () => { log.push('exchange'); return script.exchanges?.shift() ?? exchanged(`jwt${tokens}`) },
    probe: async () => { log.push('probe'); return script.probes?.shift() ?? { kind: 'ok', value: { expiresInS: 100 } } },
    revokeTicket: async () => { log.push('revokeTicket'); return { kind: 'ok', value: null } },
    revokeFamily: async () => { log.push('revokeFamily'); return { kind: 'ok', value: null } },
  } as unknown as KnowledgeUiClient
  const make = () => new KnowledgeUiInstance({
    client, bindingTag: 'b'.repeat(43), partition: 'knowledge-view-x', origin: ORIGIN, routes: ROUTES, ledger,
    credential: () => 'C', generation: () => 1, servable: () => true, knowledgeBaseIds: () => ['kb-a'],
    now: () => now, sleep: async () => {},
    setTimer: (fn, ms) => { const t = { fn, ms, cleared: false }; timers.push(t); return t },
    clearTimer: (h) => { (h as { cleared: boolean }).cleared = true },
    onEvent: (e) => events.push(e), view: VIEW,
  })
  return { ui: make(), make, log, events, timers, advance: (ms: number) => { now += ms } }
}

let id = 0
const fetchReq = (over: Partial<InjectRequest> = {}): InjectRequest => ({
  id: ++id, webContentsId: WC, mainFrame: true, resourceType: 'xhr', method: 'GET',
  url: `${ORIGIN}/api/v1/agent24/knowledge/ui/knowledge-bases`, requestHeaders: {}, ...over,
})
const flush = () => new Promise((r) => setTimeout(r, 0))

describe('KnowledgeUiInstance', () => {
  it('gets the first JWT, injects it, and schedules the renewal at 2/3 of its life', async () => {
    const t = setup()
    expect(t.ui.inject(fetchReq(), WC)).toEqual({ cancel: true })
    expect(await t.ui.start()).toBe(true)
    expect(t.ui.inject(fetchReq(), WC)).toMatchObject({ injected: true, requestHeaders: { Authorization: 'Bearer jwt1', 'X-Agent24-View-Proof': 'proof' } })
    expect(t.timers.map((x) => x.ms)).toEqual([400_000])
    t.advance(400_000)
    t.timers[0].fn()
    await flush()
    expect(t.ui.inject(fetchReq(), WC)).toMatchObject({ requestHeaders: { Authorization: 'Bearer jwt2' } })
    expect(t.timers).toHaveLength(2)
  })

  it('self-checks only on a 401 to a request it injected', async () => {
    const t = setup()
    await t.ui.start()
    const plain = fetchReq({ url: `${ORIGIN}/other` })
    t.ui.inject(plain, WC)
    t.ui.responded(plain.id, 401)
    const ok = fetchReq()
    t.ui.inject(ok, WC)
    t.ui.responded(ok.id, 200)
    t.ui.responded(ok.id, 401) // already forgotten
    await flush()
    expect(t.log).not.toContain('probe')
    const bad = fetchReq()
    t.ui.inject(bad, WC)
    t.ui.responded(bad.id, 401)
    await flush()
    expect(t.log.filter((l) => l === 'probe')).toHaveLength(1)
    expect(t.events).toEqual([])
  })

  it('rebuilds on a probe credential_invalid, and the binding closes on the second within 10 minutes', async () => {
    const ledger = new UiSessionLedger()
    const t = setup({ probes: [refused('unauthorized', 'credential_invalid'), refused('unauthorized', 'credential_invalid')] }, ledger)
    await t.ui.start()
    const r = fetchReq()
    t.ui.inject(r, WC)
    t.ui.responded(r.id, 401)
    await flush()
    expect(t.events).toEqual([{ kind: 'rebuild' }])
    // Stopped: the dead JWT goes out no more.
    expect(t.ui.inject(fetchReq(), WC)).toEqual({ cancel: true })
    const next = t.make()
    await next.start()
    const r2 = fetchReq()
    next.inject(r2, WC)
    next.responded(r2.id, 401)
    await flush()
    expect(t.events).toEqual([{ kind: 'rebuild' }, { kind: 'closed', code: 'session_rejected' }])
  })

  it('handles one self-check once, however many 401s share it', async () => {
    const t = setup({ probes: [refused('unauthorized', 'credential_invalid')] })
    await t.ui.start()
    const a = fetchReq()
    const b = fetchReq()
    t.ui.inject(a, WC)
    t.ui.inject(b, WC)
    t.ui.responded(a.id, 401)
    t.ui.responded(b.id, 401)
    await flush()
    expect(t.log.filter((l) => l === 'probe')).toHaveLength(1)
    expect(t.events).toEqual([{ kind: 'rebuild' }])
  })

  it('leaves a restart out of the count (decision #36)', async () => {
    const restarted = refused('unauthorized', 'credential_invalid', true)
    const t = setup({ probes: [restarted, restarted] })
    await t.ui.start()
    const r = fetchReq()
    t.ui.inject(r, WC)
    t.ui.responded(r.id, 401)
    await flush()
    t.advance(1000)
    const next = t.make()
    await next.start()
    const r2 = fetchReq()
    next.inject(r2, WC)
    next.responded(r2.id, 401)
    await flush()
    expect(t.events).toEqual([{ kind: 'rebuild' }, { kind: 'rebuild' }])
  })

  it('closes with revocation_mismatch on the third revoked-yet-issued in the window', async () => {
    const revoked = refused('unauthorized', 'credential_revoked')
    const t = setup({ probes: [revoked, revoked, revoked] })
    await t.ui.start()
    for (let i = 0; i < 3; i++) {
      t.advance(11_000)
      const r = fetchReq()
      t.ui.inject(r, WC)
      t.ui.responded(r.id, 401)
      await flush()
    }
    expect(t.events).toEqual([{ kind: 'closed', code: 'revocation_mismatch' }])
    // The renewal that closed it left a JWT behind; it is not injected.
    expect(t.ui.inject(fetchReq(), WC)).toEqual({ cancel: true })
    expect(t.log.filter((l) => l === 'issue')).toHaveLength(4) // the first, then one per revoked probe; the third such issue closes
  })

  it('reports closed codes and C problems from the first exchange, and does not load then', async () => {
    const empty = setup({ exchanges: [refused('forbidden', 'empty_scope')] })
    expect(await empty.ui.start()).toBe(false)
    expect(empty.events).toEqual([{ kind: 'closed', code: 'empty_scope' }])
    const cred = setup({ issues: [refused('unauthorized', 'credential_revoked')] })
    expect(await cred.ui.start()).toBe(false)
    expect(cred.events).toEqual([{ kind: 'unavailable' }])
    expect(cred.timers).toEqual([])
  })

  it('destroy stops injection and renewal and revokes the family', async () => {
    const t = setup()
    await t.ui.start()
    t.ui.destroy()
    t.ui.destroy()
    expect(t.ui.inject(fetchReq(), WC)).toEqual({ cancel: true })
    expect(t.timers[0].cleared).toBe(true)
    expect(t.log.filter((l) => l === 'revokeFamily')).toHaveLength(1)
    const r = fetchReq()
    t.ui.responded(r.id, 401)
    await flush()
    expect(t.events).toEqual([])
  })

  it('never puts C, a ticket, a JWT or the view proof into an event (U2.5)', async () => {
    const secrets = ['C', 'proof', 'jwt1', 'jwt2', 't1', 't2']
    const runs = [
      setup({ probes: [refused('unauthorized', 'credential_invalid')] }),
      setup({ exchanges: [refused('forbidden', 'empty_scope')] }),
      setup({ issues: [refused('unauthorized', 'credential_expired')] }),
    ]
    for (const t of runs) {
      if (await t.ui.start()) {
        const r = fetchReq()
        t.ui.inject(r, WC)
        t.ui.responded(r.id, 401)
        await flush()
      }
      expect(t.events).toHaveLength(1)
      const text = JSON.stringify(t.events)
      for (const secret of secrets) expect(text).not.toContain(`"${secret}"`)
      expect(Object.keys(t.events[0]).sort()).toEqual(t.events[0].kind === 'closed' ? ['code', 'kind'] : ['kind'])
    }
  })

  it('the UI session modules log nothing at all (U2.5)', () => {
    const files = readdirSync(__dirname).filter((f) => /^knowledge-ui-[a-z-]+\.ts$/.test(f) && !f.endsWith('.test.ts'))
    expect(files.length).toBeGreaterThanOrEqual(6)
    for (const file of files) {
      const source = readFileSync(path.join(__dirname, file), 'utf8')
      expect(source, file).not.toMatch(/\bconsole\.|\blog\(|\.log\(|\bwarn\(|\bdebug\(/)
    }
  })

  it('a renewal timer that fires after an event does nothing', async () => {
    const t = setup({ probes: [refused('unauthorized', 'credential_invalid')] })
    await t.ui.start()
    const fire = t.timers[0].fn
    const r = fetchReq()
    t.ui.inject(r, WC)
    t.ui.responded(r.id, 401)
    await flush()
    expect(t.timers[0].cleared).toBe(true)
    fire()
    await flush()
    expect(t.log.filter((l) => l === 'issue')).toHaveLength(1)
  })
})
