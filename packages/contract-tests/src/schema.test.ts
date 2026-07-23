// Pure-schema contract tests (no daemon needed): the generated
// protocol/events.schema.json must accept every fixture and reject the
// canonical negative shapes. Guards the FORCE_REQUIRED table in
// rust/crates/agent24-protocol/src/bin/export-schema.rs against drift.

import { beforeAll, describe, expect, it } from 'vitest'
import { readFileSync, readdirSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import Ajv2020 from 'ajv/dist/2020.js'
import addFormats from 'ajv-formats'

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..')
const eventsDir = join(repoRoot, 'protocol', 'fixtures', 'events')

let validate: ((doc: unknown) => boolean) & { errors?: unknown } = Object.assign(() => false, {})
beforeAll(() => {
  const schema = JSON.parse(
    readFileSync(join(repoRoot, 'protocol', 'events.schema.json'), 'utf8'),
  ) as Record<string, unknown>
  const ajv = new Ajv2020.default({ strict: false })
  addFormats.default(ajv)
  validate = ajv.compile(schema) as typeof validate
})

const ENVELOPE = { v: 1, seq: 0, ts: '2026-07-23T12:00:00Z' }

describe('events schema — fixtures', () => {
  it('accepts every fixture', () => {
    const files = readdirSync(eventsDir).filter((f) => f.endsWith('.json'))
    expect(files.length).toBeGreaterThanOrEqual(12)
    for (const f of files) {
      const doc = JSON.parse(readFileSync(join(eventsDir, f), 'utf8')) as unknown
      expect(validate(doc), `${f}: ${JSON.stringify(validate.errors)}`).toBe(true)
    }
  })
})

describe('events schema — canonical negatives (fail-closed guards)', () => {
  it('rejects a missing always-present nullable field', () => {
    expect(
      validate({ ...ENVELOPE, type: 'run.started', payload: { run_id: 'r' } }),
    ).toBe(false)
  })

  it('rejects tool.completed missing its always-present nullable output_summary', () => {
    expect(
      validate({
        ...ENVELOPE,
        type: 'tool.completed',
        payload: { run_id: 'r', tool_call_id: 't', status: 'completed' },
      }),
    ).toBe(false)
  })

  it('rejects approval.required missing its always-present nullable decision/decided_at', () => {
    expect(
      validate({
        ...ENVELOPE,
        type: 'approval.required',
        payload: {
          id: 'apr_x', run_id: 'r', tool_call_id: 't', kind: 'exec', summary: 's',
          payload: {}, available_decisions: ['approve'], status: 'pending',
          expires_at: '2026-07-23T12:05:00Z', created_at: '2026-07-23T12:00:00Z',
          // decision + decided_at deliberately omitted
        },
      }),
    ).toBe(false)
  })

  it('rejects a bad closed-enum value', () => {
    expect(
      validate({
        ...ENVELOPE,
        type: 'tool.completed',
        payload: { run_id: 'r', tool_call_id: 't', status: 'sprinting', output_summary: null },
      }),
    ).toBe(false)
  })

  it('rejects an unknown event type', () => {
    expect(validate({ ...ENVELOPE, type: 'weird.event', payload: {} })).toBe(false)
  })

  it('rejects a wrong protocol version (v must be const 1)', () => {
    expect(
      validate({ v: 2, seq: 0, ts: ENVELOPE.ts, type: 'run.cancelled', payload: { run_id: 'r' } }),
    ).toBe(false)
  })
})
