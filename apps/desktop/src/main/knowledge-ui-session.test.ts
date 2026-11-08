import { describe, expect, it } from 'vitest'
import { readUiError, readUiExchange, readUiProbe, UI_SESSION_WINDOW_MS, UiSessionLedger } from './knowledge-ui-session'

const BOUND = ['kb-a', 'kb-b']
const EXCHANGE = { session_token: 'jwt', expires_in: 600, ops: ['kb.list', 'chunk.list'], effective_scope: { knowledge_base_ids: ['kb-a'] } }
const MIN = 60 * 1000

describe('readUiExchange (Weknora C2-01b2)', () => {
  it('reads the session and ignores unknown fields', () => {
    expect(readUiExchange({ ...EXCHANGE, extra: 1 }, BOUND)).toEqual({ sessionToken: 'jwt', expiresInS: 600, ops: ['kb.list', 'chunk.list'], knowledgeBaseIds: ['kb-a'] })
  })

  it.each([
    ['no token', { ...EXCHANGE, session_token: '' }],
    ['lifetime not a positive integer', { ...EXCHANGE, expires_in: 0 }],
    ['lifetime fractional', { ...EXCHANGE, expires_in: 1.5 }],
    ['op outside the closed set', { ...EXCHANGE, ops: ['kb.list', 'search'] }],
    ['duplicate op', { ...EXCHANGE, ops: ['kb.list', 'kb.list'] }],
    ['empty scope', { ...EXCHANGE, effective_scope: { knowledge_base_ids: [] } }],
    ['scope the settings do not bind', { ...EXCHANGE, effective_scope: { knowledge_base_ids: ['kb-a', 'kb-z'] } }],
    ['no scope', { ...EXCHANGE, effective_scope: null }],
    ['not an object', 'jwt'],
  ])('refuses %s', (_name, body) => {
    expect(readUiExchange(body, BOUND)).toBeNull()
  })
})

describe('readUiProbe', () => {
  it('reads expires_in and nothing else', () => {
    expect(readUiProbe({ expires_in: 0 })).toEqual({ expiresInS: 0 })
    expect(readUiProbe({ expires_in: 42, x: 1 })).toEqual({ expiresInS: 42 })
    for (const bad of [{}, { expires_in: -1 }, { expires_in: '5' }, { expires_in: 1.2 }, null]) expect(readUiProbe(bad)).toBeNull()
  })
})

describe('readUiError (U3.8 decoding, decision #36)', () => {
  const invalid = (details: Record<string, unknown>) => ({ error: { code: 'unauthorized', message: 'x', details: { retryable: false, reason: 'credential_invalid', ...details } } })

  it('reads restarted from the probe next to credential_invalid', () => {
    expect(readUiError(invalid({ restarted: true }), 'probe')).toEqual({ kind: 'error', code: 'unauthorized', reason: 'credential_invalid', restarted: true, retryAfterMs: null })
    expect(readUiError(invalid({ restarted: false }), 'probe')).toMatchObject({ restarted: false })
    expect(readUiError(invalid({}), 'probe')).toMatchObject({ restarted: false })
  })

  it('makes a non-boolean restarted on the probe an unknown answer, with no truthiness', () => {
    for (const v of [null, 'true', 1, {}]) expect(readUiError(invalid({ restarted: v }), 'probe')).toEqual({ kind: 'unknown' })
  })

  it('ignores restarted anywhere else', () => {
    expect(readUiError(invalid({ restarted: true }), 'exchange')).toMatchObject({ restarted: false })
    expect(readUiError(invalid({ restarted: 'x' }), 'resource')).toMatchObject({ kind: 'error', restarted: false })
    const expired = { error: { code: 'unauthorized', message: 'x', details: { reason: 'credential_expired', restarted: true } } }
    expect(readUiError(expired, 'probe')).toEqual({ kind: 'error', code: 'unauthorized', reason: 'credential_expired', restarted: false, retryAfterMs: null })
  })

  it('takes unknown fields and gives unknown for a body it cannot read', () => {
    expect(readUiError({ error: { code: 'unavailable', message: 'm', details: { reason: 'overloaded', retry_after_ms: 5, x: 1 } } }, 'exchange'))
      .toEqual({ kind: 'error', code: 'unavailable', reason: 'overloaded', restarted: false, retryAfterMs: 5 })
    for (const bad of [null, {}, { error: {} }, { error: { code: 7 } }, 'x']) expect(readUiError(bad, 'probe')).toEqual({ kind: 'unknown' })
  })
})

describe('UiSessionLedger (U3.8, decisions #3 and #36)', () => {
  it('rebuilds once for credential_invalid and closes the second time in 10 minutes', () => {
    const l = new UiSessionLedger()
    expect(l.handle({ kind: 'invalid' }, 0).action).toBe('rebuild')
    expect(l.handle({ kind: 'invalid' }, 9 * MIN).action).toBe('closed')
  })

  it('uses a sliding window, not a fixed one', () => {
    const l = new UiSessionLedger()
    l.handle({ kind: 'invalid' }, 0)
    expect(l.handle({ kind: 'invalid' }, UI_SESSION_WINDOW_MS).action).toBe('rebuild')
    expect(l.handle({ kind: 'invalid' }, UI_SESSION_WINDOW_MS + 5 * MIN).action).toBe('closed')
  })

  it('leaves server restarts out of the count, three events per window', () => {
    const l = new UiSessionLedger()
    for (const [i, t] of [1, 2, 3].entries()) {
      expect(l.handle({ kind: 'restarted', jwtExchangedAt: t * MIN - 1 }, t * MIN), `event ${i + 1}`).toEqual({ action: 'rebuild', merged: false, exempt: true })
    }
    // The fourth event counts once like a plain credential_invalid; the fifth closes.
    expect(l.handle({ kind: 'restarted', jwtExchangedAt: 4 * MIN - 1 }, 4 * MIN)).toEqual({ action: 'rebuild', merged: false, exempt: false })
    expect(l.handle({ kind: 'restarted', jwtExchangedAt: 5 * MIN - 1 }, 5 * MIN)).toEqual({ action: 'closed', merged: false, exempt: false })
  })

  it('counts a restart once however many views it hits', () => {
    const l = new UiSessionLedger()
    // Two views exchanged at 1 and 2 min; the server restarts; both probes come back restarted.
    expect(l.handle({ kind: 'restarted', jwtExchangedAt: 1 * MIN }, 5 * MIN)).toMatchObject({ exempt: true, merged: false })
    expect(l.handle({ kind: 'restarted', jwtExchangedAt: 2 * MIN }, 5 * MIN + 10)).toEqual({ action: 'rebuild', merged: true, exempt: false })
    // A view rebuilt after that event, hit by a second restart: a new event.
    expect(l.handle({ kind: 'restarted', jwtExchangedAt: 6 * MIN }, 8 * MIN)).toMatchObject({ exempt: true, merged: false })
  })

  it('moves the merge baseline on an over-limit event too', () => {
    const l = new UiSessionLedger()
    for (const t of [1, 2, 3]) l.handle({ kind: 'restarted', jwtExchangedAt: t * MIN - 1 }, t * MIN)
    expect(l.handle({ kind: 'restarted', jwtExchangedAt: 4 * MIN - 1 }, 4 * MIN)).toMatchObject({ exempt: false, merged: false })
    // Another view of that same fourth restart: merged, so it does not close the binding.
    expect(l.handle({ kind: 'restarted', jwtExchangedAt: 3 * MIN + 30 * 1000 }, 4 * MIN + 10)).toEqual({ action: 'rebuild', merged: true, exempt: false })
  })

  it('frees exemptions as they leave the window, and starts afresh after the way out of closed', () => {
    const l = new UiSessionLedger()
    for (const t of [1, 2, 3]) l.handle({ kind: 'restarted', jwtExchangedAt: t * MIN - 1 }, t * MIN)
    expect(l.handle({ kind: 'restarted', jwtExchangedAt: 11 * MIN }, 11 * MIN + 1)).toMatchObject({ exempt: true })
    l.handle({ kind: 'invalid' }, 12 * MIN)
    expect(l.handle({ kind: 'invalid' }, 13 * MIN).action).toBe('closed')
    l.reset()
    expect(l.handle({ kind: 'invalid' }, 14 * MIN).action).toBe('rebuild')
  })
})
