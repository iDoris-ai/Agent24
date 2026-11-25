import { describe, expect, it } from 'vitest'
import { IdorisError, parseEngines, parseOperation, parseStatus, type IdorisOperation, type IdorisStatus } from './api'

const verification = { model_id: 'model', record_id: 'record', locality: 'loopback', verified_at: '2026-10-28T12:00:00Z' } satisfies NonNullable<IdorisStatus['verification']>
const status = {
  state: 'not_configured', version: '0.4.0', data_port: 9011, admin_port: 9012,
  roles: ['daily', 'embed'], last_error: null, migration_pending: false, last_migration_error: null, current_operation: null, verification,
  budget_suggestion: { core_reserved_bytes: 123, idoris_budget_bytes: 456 }, budget_unavailable_reason: null,
} satisfies IdorisStatus
const operation = {
  operation_id: 'op', action: 'install', phase: 'downloading', result: null,
  error_code: null, cancellable: true, verification,
  download_progress: { bytes_done: 1, bytes_total: 2 },
} satisfies IdorisOperation
const invalidResponse = new IdorisError('invalid_response')

describe('iDoris response validation', () => {
  const engine = { id: 'omlx', engine_kind: 'omlx', state: 'ready', managed: true, owned: true }

  it('reads engine states and preserves optional reasons', () => {
    const engines = [engine, { ...engine, id: 'external', state: 'unavailable', reason: 'connection refused', managed: false, owned: false }]
    expect(parseEngines({ engines })).toEqual({ engines })
    expect(parseEngines({ engines: [] })).toEqual({ engines: [] })
  })

  it.each([
    {}, { engines: null }, { engines: [null] },
    { engines: [{ ...engine, id: 1 }] }, { engines: [{ ...engine, engine_kind: null }] },
    { engines: [{ ...engine, state: false }] }, { engines: [{ ...engine, reason: null }] },
    { engines: [{ ...engine, managed: 'true' }] }, { engines: [{ ...engine, owned: 0 }] },
  ])('rejects malformed engine responses: %j', value => {
    expect(() => parseEngines(value)).toThrow(invalidResponse)
  })
  it('accepts degraded recovery snapshots and keeps separate failure counters', () => {
    const recovery = { ...status, state: 'degraded', crashes: 6, hangs: 2, circuit_open: true }
    expect(parseStatus(recovery)).toEqual(recovery)
  })
  it('accepts complete valid snapshots', () => {
    expect(parseStatus(status)).toEqual(status)
    expect(parseOperation(operation)).toEqual(operation)
  })
  it('preserves unknown operation errors for display', () => {
    expect(parseOperation({ ...operation, result: 'failed', error_code: 'future_error' }).error_code).toBe('future_error')
  })
  it('reads optional source metadata without requiring it from older daemons', () => {
    expect(parseStatus({ ...status, source: 'openai_compat' }).source).toBe('openai_compat')
    expect(parseStatus({ ...status, connection: { source: 'omlx' } }).source).toBe('omlx')
    expect(parseStatus(status).source).toBeUndefined()
  })
  it.each([-1, 0.5, Number.MAX_SAFE_INTEGER + 1, NaN, '1'])('rejects invalid bytes %s in every budget and progress field', value => {
    expect(() => parseStatus({ ...status, budget_suggestion: { ...status.budget_suggestion, core_reserved_bytes: value } })).toThrow(invalidResponse)
    expect(() => parseStatus({ ...status, budget_suggestion: { ...status.budget_suggestion, idoris_budget_bytes: value } })).toThrow(invalidResponse)
    expect(() => parseOperation({ ...operation, download_progress: { bytes_done: value } })).toThrow(invalidResponse)
    expect(() => parseOperation({ ...operation, download_progress: { bytes_done: 0, bytes_total: value } })).toThrow(invalidResponse)
  })
  it('rejects a download total smaller than the completed bytes', () => {
    expect(() => parseOperation({ ...operation, download_progress: { bytes_done: 2, bytes_total: 1 } })).toThrow(invalidResponse)
  })
  it.each([
    { ...verification, locality: 'remote' },
    { ...verification, verified_at: 'not-a-date' },
  ])('rejects invalid verification in both snapshots: %j', receipt => {
    expect(() => parseStatus({ ...status, verification: receipt })).toThrow(invalidResponse)
    expect(() => parseOperation({ ...operation, verification: receipt })).toThrow(invalidResponse)
  })
  it('rejects unknown budget unavailable reasons', () => {
    expect(() => parseStatus({ ...status, budget_unavailable_reason: 'unknown' })).toThrow(invalidResponse)
  })
})
