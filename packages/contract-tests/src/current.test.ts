// Contract tests for the CURRENT (pre-v1) node daemon surface.
// SCOPE: node daemon ONLY — these routes are non-versioned and the LLM probe
// assumes same-host providers; the agent24d dual-run suite is v1.test.ts,
// which must never import this file's assumptions.
// Requires a running daemon at A24_BASE_URL (default 127.0.0.1:8765) — see README.

import { beforeAll, describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import Ajv2020 from 'ajv/dist/2020.js'
import addFormats from 'ajv-formats'
import { get, post, resolveLlmExpectation } from './helpers.js'

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..')

// This whole file targets the node daemon's pre-v1 surface. Pointing
// A24_BASE_URL at agent24d must NOT run these suites: gate on A24_TARGET.
const TARGET = process.env['A24_TARGET'] || 'node'
const describeNode = TARGET === 'node' ? describe : describe.skip

let llmUp = false
beforeAll(async () => {
  if (TARGET !== 'node') return
  llmUp = await resolveLlmExpectation()
})

describeNode('GET /health', () => {
  it('returns 200 with status ok and a timestamp', async () => {
    const res = await get('/health')
    expect(res.status).toBe(200)
    const body = res.body as { status: string; ts: number }
    expect(body.status).toBe('ok')
    expect(typeof body.ts).toBe('number')
  })
})

describeNode('unknown route', () => {
  it('returns 404 with an error body', async () => {
    const res = await get('/definitely/not/a/route')
    expect(res.status).toBe(404)
    const body = res.body as { error: string; path: string }
    expect(body.error).toBe('Not found')
    expect(body.path).toBe('/definitely/not/a/route')
  })
})

describeNode('GET /api/llm/models', () => {
  it('returns 200 with an array; entries carry a model id (+ status fields)', async () => {
    const res = await get('/api/llm/models')
    expect(res.status).toBe(200)
    expect(Array.isArray(res.body)).toBe(true)
    for (const entry of res.body as Array<Record<string, unknown>>) {
      expect(typeof entry['id']).toBe('string')
      expect((entry['id'] as string).length).toBeGreaterThan(0)
      if (entry['engine'] !== undefined) expect(typeof entry['engine']).toBe('string')
      if (entry['status'] !== undefined) expect(typeof entry['status']).toBe('string')
    }
  })
})

describeNode('GET /api/llm/usage', () => {
  it('returns 200 with an array of usage entries', async () => {
    const res = await get('/api/llm/usage')
    expect(res.status).toBe(200)
    expect(Array.isArray(res.body)).toBe(true)
    for (const entry of res.body as Array<Record<string, unknown>>) {
      expect(typeof entry['tokens']).toBe('number')
      expect(typeof entry['model']).toBe('string')
      expect(typeof entry['provider']).toBe('string')
    }
  })
})

describeNode('POST /api/llm/chat', () => {
  it('rejects a body without messages with 400', async () => {
    const res = await post('/api/llm/chat', { model: 'whatever' })
    expect(res.status).toBe(400)
    expect(res.body).toEqual({ error: 'messages array required' })
  })

  it('answers a valid chat when a provider is up (200 + message), then usage records the full entry', async (ctx) => {
    if (!llmUp) return ctx.skip()
    const res = await post('/api/llm/chat', {
      messages: [{ role: 'user', content: 'Reply with the single word: pong' }],
    })
    expect(res.status).toBe(200)
    const body = res.body as { message: { role: string; content: string } }
    expect(body.message.role).toBe('assistant')
    expect(typeof body.message.content).toBe('string')
    expect(body.message.content.length).toBeGreaterThan(0)

    // usage must now contain at least one fully-shaped entry (llm-gateway.ts push)
    const usage = await get('/api/llm/usage')
    expect(usage.status).toBe(200)
    const entries = usage.body as Array<Record<string, unknown>>
    expect(entries.length).toBeGreaterThan(0)
    const last = entries[entries.length - 1]!
    expect(typeof last['tokens']).toBe('number')
    expect(typeof last['model']).toBe('string')
    expect(typeof last['provider']).toBe('string')
    expect(typeof last['moduleId']).toBe('string')
    expect(typeof last['timestamp']).toBe('number')
  })

  it('fails with 503 and a helpful error when no provider is up', async (ctx) => {
    if (llmUp) return ctx.skip()
    const res = await post('/api/llm/chat', {
      messages: [{ role: 'user', content: 'hi' }],
    })
    expect(res.status).toBe(503)
    expect((res.body as { error: string }).error).toContain('unavailable')
  })
})

describeNode('GET /api/modules', () => {
  it('returns manifests that validate against protocol/module.schema.json', async () => {
    const res = await get('/api/modules')
    expect(res.status).toBe(200)
    const modules = res.body as Array<Record<string, unknown>>
    expect(Array.isArray(modules)).toBe(true)
    expect(modules.length).toBeGreaterThan(0)

    const schema = JSON.parse(
      readFileSync(join(repoRoot, 'protocol', 'module.schema.json'), 'utf8'),
    ) as Record<string, unknown>
    const ajv = new Ajv2020.default({ strict: false })
    addFormats.default(ajv)
    const validate = ajv.compile(schema)

    for (const mod of modules) {
      expect(typeof mod['enabled']).toBe('boolean')
      const ok = validate(mod)
      expect(ok, `manifest ${String(mod['id'])} invalid: ${JSON.stringify(validate.errors)}`).toBe(true)
    }
  })
})

describeNode('POST /api/modules/install', () => {
  it('rejects a missing packageName with 400', async () => {
    const res = await post('/api/modules/install', {})
    expect(res.status).toBe(400)
    const body = res.body as { ok: boolean; error: string }
    expect(body.ok).toBe(false)
    expect(body.error).toBe('packageName required')
  })
})
