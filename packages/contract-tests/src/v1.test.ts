// v1 protocol contract skeletons — activated milestone by milestone.
// Source of truth: protocol/openapi.yaml + protocol/events.schema.json.
// A5 turns the M-A group live against the node mock daemon; the M-C group
// goes live with the Rust daemon (tasks C1..C5). Keep names in sync with
// docs/specs/SPEC-002-protocol.md §2/§3.

import { beforeAll, describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import Ajv2020 from 'ajv/dist/2020.js'
import addFormats from 'ajv-formats'
import WsClient from 'ws'
import { BASE_URL, TOKEN, get, post, resolveLlmExpectation } from './helpers.js'

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..')
const WS_URL = BASE_URL.replace(/^http/, 'ws') + '/api/v1/events'

let llmUp = false
let validateEvent: ((doc: unknown) => boolean) & { errors?: unknown } = Object.assign(() => false, {})
beforeAll(async () => {
  llmUp = await resolveLlmExpectation()
  const schema = JSON.parse(readFileSync(join(repoRoot, 'protocol', 'events.schema.json'), 'utf8')) as Record<string, unknown>
  const ajv = new Ajv2020.default({ strict: false })
  addFormats.default(ajv)
  validateEvent = ajv.compile(schema) as typeof validateEvent
})

// Collect WS events while `action` runs, until a terminal run event or timeout.
// Uses the `ws` client (not the global WebSocket): agent24d requires the
// bearer token on the upgrade request, which browser-style clients cannot set.
async function collectEventsDuring(action: () => Promise<void>): Promise<Array<Record<string, unknown>>> {
  const events: Array<Record<string, unknown>> = []
  const ws = new WsClient(WS_URL, {
    headers: TOKEN ? { Authorization: `Bearer ${TOKEN}` } : {},
  })
  await new Promise<void>((resolve, reject) => {
    ws.on('open', () => resolve())
    ws.on('error', (err) => reject(new Error(`WS connect failed: ${WS_URL}: ${String(err)}`)))
  })
  const done = new Promise<void>((resolve) => {
    const timer = setTimeout(() => resolve(), 10_000)
    ws.on('message', (data) => {
      const doc = JSON.parse(String(data)) as Record<string, unknown>
      events.push(doc)
      const t = doc['type']
      if (t === 'run.completed' || t === 'run.failed' || t === 'run.cancelled') {
        clearTimeout(timer)
        resolve()
      }
    })
  })
  await action()
  await done
  ws.close()
  return events
}

describe('v1 M-A (live since A5)', () => {
  it('GET /api/v1/health → 200 {status:"ok", version, backend}', async () => {
    const res = await get('/api/v1/health')
    expect(res.status).toBe(200)
    const body = res.body as { status: string; version: string; backend: string }
    expect(body.status).toBe('ok')
    expect(typeof body.version).toBe('string')
    expect(body.version.length).toBeGreaterThan(0)
    expect(typeof body.backend).toBe('string')
  })

  it('POST /api/v1/chat without messages → 400 invalid_request envelope', async () => {
    const res = await post('/api/v1/chat', { model: 'whatever' })
    expect(res.status).toBe(400)
    const body = res.body as { error: { code: string; message: string } }
    expect(body.error.code).toBe('invalid_request')
    expect(typeof body.error.message).toBe('string')
  })

  it('POST /api/v1/chat with a literal null body → 400 (never 500/internal leak)', async () => {
    const res = await post('/api/v1/chat', null)
    expect(res.status).toBe(400)
    expect((res.body as { error: { code: string } }).error.code).toBe('invalid_request')
  })

  it('unknown v1 route → 404 not_found envelope', async () => {
    const res = await get('/api/v1/definitely-not-a-route')
    expect(res.status).toBe(404)
    expect((res.body as { error: { code: string } }).error.code).toBe('not_found')
  })

  it('bare /api/v1 (no trailing slash) → 404 v1 envelope, never legacy shape', async () => {
    const res = await get('/api/v1')
    expect(res.status).toBe(404)
    const body = res.body as { error: { code: string; message: string } }
    expect(body.error.code).toBe('not_found')
    expect(typeof body.error.message).toBe('string')
  })

  it('oversized chat body → 413 payload_too_large envelope', async () => {
    const big = 'x'.repeat(1024 * 1024 + 100)
    const res = await post('/api/v1/chat', { messages: [{ role: 'user', content: big }] })
    expect(res.status).toBe(413)
    expect((res.body as { error: { code: string } }).error.code).toBe('payload_too_large')
  })

  it('WS upgrade carrying a browser Origin header is rejected', async () => {
    const outcome = await new Promise<string>((resolve) => {
      const ws = new WsClient(WS_URL, {
        headers: {
          Origin: 'http://evil.example',
          ...(TOKEN ? { Authorization: `Bearer ${TOKEN}` } : {}),
        },
      })
      ws.on('open', () => { ws.close(); resolve('open') })
      ws.on('error', () => resolve('rejected'))
      setTimeout(() => resolve('timeout'), 5000)
    })
    expect(outcome).toBe('rejected')
  })

  it('GET /api/v1/models → 200 {models:[{id, provider, tier, loaded}]}', async () => {
    const res = await get('/api/v1/models')
    expect(res.status).toBe(200)
    const body = res.body as { models: Array<Record<string, unknown>> }
    expect(Array.isArray(body.models)).toBe(true)
    for (const m of body.models) {
      expect(typeof m['id']).toBe('string')
      expect(typeof m['provider']).toBe('string')
      expect(typeof m['tier']).toBe('string')
      expect(typeof m['loaded']).toBe('boolean')
    }
  })

  it('GET /api/v1/usage → 200 Usage aggregate', async () => {
    const res = await get('/api/v1/usage')
    expect(res.status).toBe(200)
    const body = res.body as Record<string, unknown>
    for (const k of ['prompt_tokens', 'completion_tokens', 'total_tokens']) {
      expect(typeof body[k], k).toBe('number')
    }
  })

  it('chat success emits run.started → model.delta → run.completed; envelope valid, seq monotonic', async (ctx) => {
    if (!llmUp) return ctx.skip()
    let chatRes: { status: number; body: unknown } | null = null
    const events = await collectEventsDuring(async () => {
      chatRes = await post('/api/v1/chat', {
        messages: [{ role: 'user', content: 'Reply with the single word: pong' }],
      })
    })
    expect(chatRes!.status).toBe(200)
    const resBody = chatRes!.body as { message: { role: string; content: string }; usage: Record<string, unknown> }
    expect(resBody.message.role).toBe('assistant')
    expect(typeof resBody.usage['total_tokens']).toBe('number')

    // Correlate by the first run.started's run_id — shields against bystander
    // events if the daemon under test serves concurrent traffic.
    const started = events.find((e) => e['type'] === 'run.started')
    expect(started).toBeDefined()
    const runId = (started!['payload'] as { run_id: string }).run_id
    expect(runId).toMatch(/^run_[0-9A-HJKMNP-TV-Z]{26}$/) // ULID (Crockford base32)
    const mine = events.filter((e) => (e['payload'] as { run_id?: string }).run_id === runId)
    const types = mine.map((e) => e['type'])
    expect(types[0]).toBe('run.started')
    expect(types).toContain('model.delta')
    expect(types[types.length - 1]).toBe('run.completed')
    let prevSeq = -1
    for (const ev of mine) {
      expect(validateEvent(ev), JSON.stringify(validateEvent.errors)).toBe(true)
      expect(ev['v']).toBe(1)
      expect(ev['seq']).toBeGreaterThan(prevSeq)
      prevSeq = ev['seq'] as number
    }
  })

  it('chat failure emits run.started → run.failed with provider_unavailable envelope', async (ctx) => {
    if (llmUp) return ctx.skip()
    let chatRes: { status: number; body: unknown } | null = null
    const events = await collectEventsDuring(async () => {
      chatRes = await post('/api/v1/chat', { messages: [{ role: 'user', content: 'hi' }] })
    })
    expect(chatRes!.status).toBe(503)
    expect((chatRes!.body as { error: { code: string } }).error.code).toBe('provider_unavailable')
    const started = events.find((e) => e['type'] === 'run.started')
    expect(started).toBeDefined()
    const runId = (started!['payload'] as { run_id: string }).run_id
    const mine = events.filter((e) => (e['payload'] as { run_id?: string }).run_id === runId)
    const types = mine.map((e) => e['type'])
    expect(types[0]).toBe('run.started')
    expect(types[types.length - 1]).toBe('run.failed')
    for (const ev of mine) {
      expect(validateEvent(ev), JSON.stringify(validateEvent.errors)).toBe(true)
    }
  })

  describe('agent24d only (B2+)', () => {
    const IS_RUST = (process.env['A24_TARGET'] || 'node') !== 'node'
    it('requests without bearer token → 401 unauthorized envelope', async (ctx) => {
      if (!IS_RUST) return ctx.skip() // mock daemon is exempt from auth
      const res = await fetch(`${BASE_URL}/api/v1/models`) // deliberately no Authorization
      expect(res.status).toBe(401)
      const body = (await res.json()) as { error: { code: string } }
      expect(body.error.code).toBe('unauthorized')
    })
  })
})

describe('v1 M-C runs (activate in C2)', () => {
  it.todo('POST /api/v1/runs → 202 Run(status=queued), then run.started event')
  it.todo('GET /api/v1/runs/{id} → 200 Run; unknown id → 404')
  it.todo('GET /api/v1/runs?status=running filters correctly')
  it.todo('POST /api/v1/runs/{id}/cancel → 202; streaming run emits run.cancelled within 1s')
  it.todo('cancel is idempotent: cancelling a terminal run returns 202 with unchanged run')
})

describe('v1 M-C approvals (activate in C4)', () => {
  it.todo('approval-required tool emits approval.required (request class) with available_decisions')
  it.todo('GET /api/v1/approvals?status=pending lists the pending approval')
  it.todo('POST /api/v1/approvals/{id} {type:approve} → 200, run resumes')
  it.todo('POST /api/v1/approvals/{id} {type:deny, reason} → 200, model receives reason, run continues')
  it.todo('POST /api/v1/approvals/{id} {type:abort} → run cancelled')
  it.todo('decision type not in available_decisions → 400 invalid_request')
  it.todo('second decision on same approval → 409 approval_already_resolved')
  it.todo('expired approval resolves to timed_out (fail-closed) and emits approval.resolved')
  it.todo('daemon restart marks lingering pending approvals aborted (fail-closed)')
})

describe('v1 M-C schedules (activate in C5)', () => {
  it.todo('POST /api/v1/schedules (cron spec) → 201 with computed next_run_at')
  it.todo('GET /api/v1/schedules lists it; GET /api/v1/schedules/{id} returns it')
  it.todo('POST /api/v1/schedules with every.secs < 60 → 400')
  it.todo('PATCH /api/v1/schedules/{id} spec change recomputes next_run_at')
  it.todo('DELETE /api/v1/schedules/{id} → 204; then GET → 404')
  it.todo('POST /api/v1/schedules/{id}/run_now → 202 {run_id}, next_run_at unchanged')
  it.todo('schedule firing emits schedule.fired {schedule_id, run_id}')
})

describe('v1 M-C sessions & tools (activate in C1/C3)', () => {
  it.todo('POST /api/v1/sessions → 201 Session; GET list contains it; GET /api/v1/sessions/{id} returns it')
  it.todo('GET /api/v1/tools → 200 {tools:[{name, source, description, requires_approval}]}')
})
