// L7-K2 mock activation onboarding.

import { describe, it, expect, beforeEach, afterEach } from 'vitest'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { knowledgeOnboarding, type OnboardDeps } from './knowledge-onboard'
import type { KnowledgeController } from './knowledge-settings'

const ACTIVATION = 'knowledge-activation.json'
const PENDING = 'knowledge-onboard-pending.json'
const ENDPOINT = 'https://kb.example'
const CODE = 'activation-SECRET'
const API_KEY = 'api-key-SECRET'
const KB = 'kb-default'
const GOOD_FILE = { endpoint: ENDPOINT, activation_code: CODE }

function okInit(patch: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    knowledge_base_id: KB,
    outcome: 'created',
    config_version: '1',
    credential: { api_key: API_KEY, expires_at: null },
    ...patch,
  }
}

const IN_PROGRESS = {
  error: {
    code: 'unavailable',
    message: 'another init is running',
    details: { reason: 'request_in_progress', retryable: true },
  },
}

interface Scripted { status: number; body?: unknown; throw?: boolean }

function writeActivation(dir: string, value: unknown, mode = 0o600): void {
  const file = path.join(dir, ACTIVATION)
  fs.writeFileSync(file, typeof value === 'string' ? value : JSON.stringify(value))
  fs.chmodSync(file, mode)
}

function bodyOf(init: RequestInit): { activation_code?: string; idempotency_key?: string } {
  return JSON.parse(String(init.body)) as { activation_code?: string; idempotency_key?: string }
}

describe('knowledge onboarding (L7-K2)', () => {
  let dir = ''

  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kb-onboard-'))
  })
  afterEach(() => {
    fs.rmSync(dir, { recursive: true, force: true })
  })

  function harness(opts: {
    responses?: Scripted[]
    endpoint?: string
    configureOk?: boolean
    enableOk?: boolean
    controller?: boolean
  } = {}): {
    calls: Array<{ url: string; init: RequestInit }>
    configures: Array<{ endpoint: unknown; knowledgeBaseIds: unknown; apiKey: unknown }>
    enables: boolean[]
    sleeps: number[]
    onboarding: ReturnType<typeof knowledgeOnboarding>
    setEndpoint: (value: string) => void
  } {
    const calls: Array<{ url: string; init: RequestInit }> = []
    const configures: Array<{ endpoint: unknown; knowledgeBaseIds: unknown; apiKey: unknown }> = []
    const enables: boolean[] = []
    const sleeps: number[] = []
    let endpoint = opts.endpoint ?? ''
    let next = 0
    const responses = opts.responses ?? []
    const fetch: OnboardDeps['fetch'] = async (url, init) => {
      calls.push({ url, init })
      const response = responses[next] ?? responses[responses.length - 1]
      next += 1
      if (!response || response.throw) throw new Error('network')
      return new Response(response.body === undefined ? '' : JSON.stringify(response.body), { status: response.status })
    }
    const configureOk = opts.configureOk !== false
    const enableOk = opts.enableOk !== false
    const controller = {
      status: () => ({ enabled: false, endpoint, knowledgeBaseIds: [] as string[], hasKey: endpoint !== '', active: false }),
      configure: async (input: { endpoint: unknown; knowledgeBaseIds: unknown; apiKey?: unknown }) => {
        configures.push({ endpoint: input.endpoint, knowledgeBaseIds: input.knowledgeBaseIds, apiKey: input.apiKey })
        if (!configureOk) return { ok: false as const, reason: 'not_saved' as const }
        endpoint = String(input.endpoint)
        return { ok: true as const }
      },
      setEnabled: async (enabled: boolean) => {
        enables.push(enabled)
        if (!enableOk) return { ok: false as const, reason: 'not_saved' as const }
        return { ok: true as const }
      },
    }
    const onboarding = knowledgeOnboarding({
      dir,
      controller: () => (opts.controller === false ? null : controller as unknown as KnowledgeController),
      fetch,
      sleep: async (ms) => { sleeps.push(ms) },
    })
    return { calls, configures, enables, sleeps, onboarding, setEndpoint: (value) => { endpoint = value } }
  }

  it('local only does not read the activation or call the server', async () => {
    writeActivation(dir, { endpoint: 'not-a-url', activation_code: 'x' }, 0o600)
    const { onboarding, calls } = harness()
    expect(await onboarding.run({ localOnly: true })).toEqual({ ok: false, code: 'local_only_unavailable' })
    expect(calls).toEqual([])
    expect(fs.existsSync(path.join(dir, PENDING))).toBe(false)
  })

  it('an endpoint already saved is left alone', async () => {
    writeActivation(dir, { endpoint: 'http://kb.example', activation_code: CODE })
    const { onboarding, calls, configures } = harness({ endpoint: 'https://already.example' })
    expect(await onboarding.run({ localOnly: false })).toEqual({ ok: false, code: 'already_bound' })
    expect(calls).toEqual([])
    expect(configures).toEqual([])
  })

  it('no controller is unavailable before the file is read', async () => {
    writeActivation(dir, { endpoint: 'not-a-url', activation_code: 'x' })
    const { onboarding, calls } = harness({ controller: false })
    expect(await onboarding.run({ localOnly: false })).toEqual({ ok: false, code: 'unavailable' })
    expect(calls).toEqual([])
  })

  it('a missing activation file is not activated', async () => {
    const { onboarding, calls } = harness()
    expect(await onboarding.run({ localOnly: false })).toEqual({ ok: false, code: 'not_activated' })
    expect(calls).toEqual([])
    expect(fs.existsSync(path.join(dir, PENDING))).toBe(false)
  })

  it.each([
    ['group-readable', (root: string) => writeActivation(root, GOOD_FILE, 0o640)],
    ['symlink', (root: string) => {
      const real = path.join(root, 'real-activation.json')
      fs.writeFileSync(real, JSON.stringify(GOOD_FILE))
      fs.chmodSync(real, 0o600)
      fs.symlinkSync(real, path.join(root, ACTIVATION))
    }],
    ['unknown field', (root: string) => writeActivation(root, { ...GOOD_FILE, note: 'x' })],
    ['bad endpoint', (root: string) => writeActivation(root, { endpoint: 'http://kb.example', activation_code: CODE })],
    ['bad json', (root: string) => writeActivation(root, '{')],
    ['short code', (root: string) => writeActivation(root, { endpoint: ENDPOINT, activation_code: 'short' })],
  ])('%s activation is invalid and does not call the server', async (_name, setup) => {
    setup(dir)
    const { onboarding, calls } = harness()
    expect(await onboarding.run({ localOnly: false })).toEqual({ ok: false, code: 'activation_invalid' })
    expect(calls).toEqual([])
    expect(fs.existsSync(path.join(dir, PENDING))).toBe(false)
  })

  it('a created base is saved, enabled, and the pending key and secrets are dropped', async () => {
    writeActivation(dir, GOOD_FILE)
    const { onboarding, calls, configures, enables } = harness({
      responses: [{ status: 200, body: okInit({ details: { config_mismatch: true } }) }],
    })
    const result = await onboarding.run({ localOnly: false })
    expect(result).toEqual({ ok: true, outcome: 'created', knowledgeBaseId: KB, configMismatch: true })
    const text = JSON.stringify(result)
    const sent = bodyOf(calls[0]!.init)
    expect(text).not.toContain(API_KEY)
    expect(text).not.toContain(CODE)
    expect(text).not.toContain(sent.idempotency_key)
    expect(calls).toHaveLength(1)
    expect(calls[0]!.url).toBe(`${ENDPOINT}/api/v1/agent24/knowledge/init`)
    expect(calls[0]!.init.method).toBe('POST')
    expect(calls[0]!.init.redirect).toBe('error')
    expect(calls[0]!.init.cache).toBe('no-store')
    expect(calls[0]!.init.signal).toBeInstanceOf(AbortSignal)
    const headers = new Headers(calls[0]!.init.headers)
    expect(headers.get('content-type')).toBe('application/json')
    expect(headers.get('authorization')).toBeNull()
    expect(headers.get('x-api-key')).toBeNull()
    expect(sent.activation_code).toBe(CODE)
    expect(sent.idempotency_key).toMatch(/^[A-Za-z0-9_-]{43}$/)
    expect(configures).toEqual([{ endpoint: ENDPOINT, knowledgeBaseIds: [KB], apiKey: API_KEY }])
    expect(enables).toEqual([true])
    expect(fs.existsSync(path.join(dir, PENDING))).toBe(false)
  })

  it('two concurrent runs share one fetch', async () => {
    writeActivation(dir, GOOD_FILE)
    const { onboarding, calls } = harness({ responses: [{ status: 200, body: okInit() }] })
    const first = onboarding.run({ localOnly: false })
    const second = onboarding.run({ localOnly: false })
    expect(second).toBe(first)
    await expect(first).resolves.toMatchObject({ ok: true, outcome: 'created' })
    expect(calls).toHaveLength(1)
  })

  it('retries request_in_progress twice and then saves the existing base', async () => {
    writeActivation(dir, GOOD_FILE)
    const { onboarding, calls, sleeps } = harness({
      responses: [
        { status: 503, body: IN_PROGRESS },
        { status: 503, body: IN_PROGRESS },
        { status: 200, body: okInit({ outcome: 'existing' }) },
      ],
    })
    expect(await onboarding.run({ localOnly: false })).toEqual({
      ok: true, outcome: 'existing', knowledgeBaseId: KB, configMismatch: false,
    })
    expect(calls).toHaveLength(3)
    expect(sleeps).toEqual([2_000, 2_000])
    const keys = calls.map((call) => bodyOf(call.init).idempotency_key)
    expect(keys[0]).toBe(keys[1])
    expect(keys[1]).toBe(keys[2])
  })

  it('gives up after five request_in_progress answers and keeps the pending key', async () => {
    writeActivation(dir, GOOD_FILE)
    const { onboarding, calls, sleeps } = harness({ responses: [{ status: 503, body: IN_PROGRESS }] })
    expect(await onboarding.run({ localOnly: false })).toEqual({ ok: false, code: 'in_progress' })
    expect(calls).toHaveLength(5)
    expect(sleeps).toEqual([2_000, 2_000, 2_000, 2_000])
    expect(fs.existsSync(path.join(dir, PENDING))).toBe(true)
  })

  it('a 503 that is not request_in_progress is a server error', async () => {
    writeActivation(dir, GOOD_FILE)
    const { onboarding, calls, sleeps } = harness({
      responses: [{
        status: 503,
        body: { error: { code: 'unavailable', message: 'busy', details: { reason: 'overloaded', retryable: true } } },
      }],
    })
    expect(await onboarding.run({ localOnly: false })).toEqual({ ok: false, code: 'server_error' })
    expect(calls).toHaveLength(1)
    expect(sleeps).toEqual([])
  })

  it('a network error keeps the key, and the next success then a fresh run uses a new one', async () => {
    writeActivation(dir, GOOD_FILE)
    const { onboarding, calls, setEndpoint } = harness({
      responses: [
        { status: 0, throw: true },
        { status: 200, body: okInit() },
        { status: 200, body: okInit({ outcome: 'existing' }) },
      ],
    })
    expect(await onboarding.run({ localOnly: false })).toEqual({ ok: false, code: 'unreachable' })
    const pending = path.join(dir, PENDING)
    const saved = JSON.parse(fs.readFileSync(pending, 'utf8')) as { idempotency_key: string }
    expect(fs.statSync(pending).mode & 0o777).toBe(0o600)
    expect(saved.idempotency_key).toBe(bodyOf(calls[0]!.init).idempotency_key)
    expect(fs.readFileSync(pending, 'utf8')).not.toContain(CODE)
    expect(fs.readFileSync(pending, 'utf8')).not.toContain(API_KEY)

    const second = await onboarding.run({ localOnly: false })
    expect(second).toEqual({ ok: true, outcome: 'created', knowledgeBaseId: KB, configMismatch: false })
    expect(bodyOf(calls[1]!.init).idempotency_key).toBe(saved.idempotency_key)
    expect(fs.existsSync(pending)).toBe(false)
    expect(JSON.stringify(second)).not.toContain(API_KEY)
    expect(JSON.stringify(second)).not.toContain(CODE)

    setEndpoint('')
    expect(await onboarding.run({ localOnly: false })).toMatchObject({ ok: true, outcome: 'existing' })
    expect(bodyOf(calls[2]!.init).idempotency_key).not.toBe(saved.idempotency_key)
  })

  it('401 is an invalid activation and 404 means the route is not offered', async () => {
    writeActivation(dir, GOOD_FILE)
    const denied = harness({
      responses: [{
        status: 401,
        body: { error: { code: 'unauthorized', message: 'no', details: { reason: 'credential_invalid', retryable: false } } },
      }],
    })
    expect(await denied.onboarding.run({ localOnly: false })).toEqual({ ok: false, code: 'activation_invalid' })
    expect(denied.calls).toHaveLength(1)
    expect(denied.configures).toEqual([])

    const missing = harness({
      responses: [{
        status: 404,
        body: { error: { code: 'not_found', message: 'no route', details: { retryable: false } } },
      }],
    })
    expect(await missing.onboarding.run({ localOnly: false })).toEqual({ ok: false, code: 'not_offered' })
    expect(missing.configures).toEqual([])
  })

  it('a malformed 200 is a server error and does not configure', async () => {
    writeActivation(dir, GOOD_FILE)
    const { onboarding, configures } = harness({
      responses: [{ status: 200, body: okInit({ credential: { api_key: 'bad key', expires_at: null } }) }],
    })
    expect(await onboarding.run({ localOnly: false })).toEqual({ ok: false, code: 'server_error' })
    expect(configures).toEqual([])
    expect(fs.existsSync(path.join(dir, PENDING))).toBe(true)
  })

  it('configure or setEnabled failing is not_saved and keeps the pending key', async () => {
    writeActivation(dir, GOOD_FILE)
    const failedSave = harness({ responses: [{ status: 200, body: okInit() }], configureOk: false })
    expect(await failedSave.onboarding.run({ localOnly: false })).toEqual({ ok: false, code: 'not_saved' })
    expect(failedSave.configures).toHaveLength(1)
    expect(failedSave.enables).toEqual([])
    expect(fs.existsSync(path.join(dir, PENDING))).toBe(true)

    const failedEnable = harness({ responses: [{ status: 200, body: okInit() }], enableOk: false })
    expect(await failedEnable.onboarding.run({ localOnly: false })).toEqual({ ok: false, code: 'not_saved' })
    expect(failedEnable.enables).toEqual([true])
    expect(fs.existsSync(path.join(dir, PENDING))).toBe(true)
  })

  it('preflight reads /health and reports an existing endpoint', async () => {
    writeActivation(dir, GOOD_FILE)
    const seen: Array<{ url: string; init: RequestInit }> = []
    let status = 200
    const onboarding = knowledgeOnboarding({
      dir,
      controller: () => ({ status: () => ({ enabled: false, endpoint: '', knowledgeBaseIds: [], hasKey: false, active: false }) }) as unknown as KnowledgeController,
      fetch: async (url, init) => {
        seen.push({ url, init })
        if (status === 0) throw new Error('down')
        return new Response('ok', { status })
      },
    })
    expect(await onboarding.preflight()).toEqual({
      activated: true,
      invalidActivation: false,
      alreadyBound: false,
      reachable: true,
      formats: ['md', 'markdown', 'txt'],
      maxBytes: 10_485_760,
    })
    expect(seen[0]!.url).toBe(`${ENDPOINT}/health`)
    expect(seen[0]!.init.method).toBe('GET')
    expect(seen[0]!.init.redirect).toBe('error')
    expect(seen[0]!.init.cache).toBe('no-store')
    expect(JSON.stringify(seen[0]!.init.headers ?? {})).not.toContain(CODE)
    status = 503
    expect((await onboarding.preflight()).reachable).toBe(false)
    status = 0
    expect((await onboarding.preflight()).reachable).toBe(false)

    const bound = knowledgeOnboarding({
      dir,
      controller: () => ({
        status: () => ({ enabled: true, endpoint: 'https://already.example/api/v1', knowledgeBaseIds: ['kb'], hasKey: true, active: true }),
      }) as unknown as KnowledgeController,
      fetch: async () => new Response('ok', { status: 200 }),
    })
    expect(await bound.preflight()).toMatchObject({ activated: true, alreadyBound: true, reachable: true })

    fs.rmSync(path.join(dir, ACTIVATION))
    const absent = harness()
    expect(await absent.onboarding.preflight()).toMatchObject({
      activated: false, invalidActivation: false, alreadyBound: false, reachable: null,
    })
    writeActivation(dir, { ...GOOD_FILE, note: 'x' })
    expect(await absent.onboarding.preflight()).toMatchObject({
      activated: false, invalidActivation: true, reachable: null,
    })
    expect(absent.calls).toEqual([])
  })
})
