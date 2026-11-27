// L7-K2 mock activation onboarding.

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { knowledgeOnboarding, type OnboardDeps, type OnboardLite } from './knowledge-onboard'
import { KnowledgeHost, type HostTransport } from './knowledge-host'
import { KnowledgeLoginStore } from './knowledge-logins'
import { KnowledgeReachability } from './knowledge-reachability'
import { KNOWLEDGE_BINDING, KnowledgeController, KnowledgeSettingsStore } from './knowledge-settings'
import { knowledgeUploads } from './knowledge-upload'
import type { LiteKnowledgeStatus } from '../shared/ipc-types'
import { settingsKnowledgeViewTarget } from './knowledge-view'

vi.mock('./backend-manager', () => ({ daemonConnection: () => null, daemonElevation: () => null }))

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
    adoptAs?: 'ok' | 'inactive' | 'bound' | 'overtaken' | 'not_saved'
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
    const adoptAs = opts.adoptAs ?? 'ok'
    const controller = {
      status: () => ({ enabled: false, endpoint, knowledgeBaseIds: [] as string[], hasKey: endpoint !== '', active: false }),
      adopt: async (input: { endpoint: string; knowledgeBaseIds: string[]; apiKey: string }) => {
        configures.push({ endpoint: input.endpoint, knowledgeBaseIds: input.knowledgeBaseIds, apiKey: input.apiKey })
        if (adoptAs === 'bound' || adoptAs === 'overtaken' || adoptAs === 'not_saved') return { saved: false as const, reason: adoptAs }
        endpoint = input.endpoint
        enables.push(true)
        return adoptAs === 'inactive'
          ? { saved: true as const, result: { ok: false as const, reason: 'unavailable' as const } }
          : { saved: true as const, result: { ok: true as const } }
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

  it('local only without a Lite does not read the activation or call the server', async () => {
    writeActivation(dir, { endpoint: 'not-a-url', activation_code: 'x' }, 0o600)
    const { onboarding, calls } = harness()
    expect(await onboarding.run({ localOnly: true })).toEqual({ ok: false, code: 'local_unavailable' })
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
    ['endpoint with user info', (root: string) => writeActivation(root, { ...GOOD_FILE, endpoint: 'https://user@kb.example/api/v1' })],
    ['endpoint with query', (root: string) => writeActivation(root, { ...GOOD_FILE, endpoint: `${ENDPOINT}/api/v1?x=1` })],
    ['endpoint with fragment', (root: string) => writeActivation(root, { ...GOOD_FILE, endpoint: `${ENDPOINT}/api/v1#x` })],
    ['endpoint with arbitrary path', (root: string) => writeActivation(root, { ...GOOD_FILE, endpoint: `${ENDPOINT}/prefix/api/v1` })],
    ['endpoint with dot segments', (root: string) => writeActivation(root, { ...GOOD_FILE, endpoint: `${ENDPOINT}/prefix/../api/v1` })],
    ['bad json', (root: string) => writeActivation(root, '{')],
    ['short code', (root: string) => writeActivation(root, { endpoint: ENDPOINT, activation_code: 'short' })],
  ])('%s activation is invalid and does not call the server', async (_name, setup) => {
    setup(dir)
    const { onboarding, calls } = harness()
    expect(await onboarding.preflight()).toMatchObject({ invalidActivation: true, reachable: null })
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
    expect(result).toEqual({ ok: true, outcome: 'created', knowledgeBaseId: KB, configMismatch: true, activeNow: true })
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
    expect(configures).toEqual([{ endpoint: `${ENDPOINT}/api/v1`, knowledgeBaseIds: [KB], apiKey: API_KEY }])
    expect(enables).toEqual([true])
    expect(fs.existsSync(path.join(dir, PENDING))).toBe(false)
  })

  it.each(['/', '/api/v1/'])('hands activation path %s off to one binding for daemon retrieval, readiness and uploads', async (activationPath) => {
    const root = 'http://127.0.0.1:18283'
    const apiBase = `${root}/api/v1`
    const providerBase = `${apiBase}/agent24/knowledge`
    writeActivation(dir, { ...GOOD_FILE, endpoint: `${root}${activationPath}` })
    const logins = new KnowledgeLoginStore(dir, {
      isEncryptionAvailable: () => true,
      encryptString: (plain) => Buffer.from(plain),
      decryptString: (cipher) => cipher.toString(),
    }, 'darwin')
    const daemon = vi.fn<HostTransport>(async () => ({ status: 204 }))
    const host = new KnowledgeHost(logins, {
      connection: () => ({ port: 1, authHeaders: { Authorization: 'Bearer fixture' } }),
      transport: daemon,
    })
    host.start()
    const store = new KnowledgeSettingsStore(dir)
    const controller = new KnowledgeController(host, logins, store)
    const fetch = vi.fn(async (url: string, init: RequestInit) => {
      if (url === `${root}/health` && init.method === 'GET') return new Response(null, { status: 204 })
      if (url === `${providerBase}/init` && init.method === 'POST') return Response.json(okInit())
      if (new Headers(init.headers).get('x-api-key') !== API_KEY) return new Response(null, { status: 401 })
      if (url === `${providerBase}/ready`) return Response.json({ live: true })
      const statuses = new Map([
        [`${providerBase}/ingest`, 'saved'],
        [`${providerBase}/jobs/get`, 'processing'],
        [`${providerBase}/jobs/cancel`, 'cancelled'],
      ])
      const status = statuses.get(url)
      if (!status) return new Response(null, { status: 404 })
      const body = JSON.parse(String(init.body)) as { request_id: string }
      return Response.json({
        request_id: body.request_id,
        job: { job_id: 'job-1', status, cost: { final: status === 'cancelled', items: [] } },
        cancel_effect: 'cancelled',
      })
    })
    const reachability = new KnowledgeReachability({ inputs: () => controller.probeInputs(), fetch })
    const onboarding = knowledgeOnboarding({ dir, controller: () => controller, fetch })
    expect(await onboarding.preflight()).toMatchObject({ reachable: true, alreadyBound: false })
    expect(await onboarding.run({ localOnly: false })).toMatchObject({ ok: true, activeNow: true })
    controller.attachReachability(reachability)
    expect((await controller.checkReachability()).reachability?.state).toBe('ok')
    expect(store.load()).toEqual({ enabled: true, endpoint: apiBase, knowledgeBaseIds: [KB], source: 'remote' })
    expect(logins.list().get(KNOWLEDGE_BINDING)).toMatchObject({ profileId: apiBase, material: API_KEY })
    expect(controller.bindingForRun()).toBe(KNOWLEDGE_BINDING)
    const route = `/api/v1/host/knowledge/bindings/${KNOWLEDGE_BINDING}`
    const activation = daemon.mock.calls.find(([, method, p]) => method === 'PUT' && p === route)
    expect(JSON.parse(activation?.[3] ?? '{}')).toMatchObject({ endpoint: apiBase, knowledge_base_ids: [KB] })
    const credential = daemon.mock.calls.find(([, method, p]) => method === 'PUT' && p === `${route}/credential`)
    expect(JSON.parse(credential?.[3] ?? '{}')).toMatchObject({ kind: 'api_key', value: API_KEY })

    // Upload uses the same timer dependency for request deadlines and job polling.
    const timers = new Map<number, () => void>()
    let nextTimer = 0
    const uploads = knowledgeUploads({
      provider: () => {
        // Same projection as the upload provider in main.ts: upload owns /api/v1.
        const binding = controller.viewBinding()
        const origin = settingsKnowledgeViewTarget(controller.status().endpoint)?.origin
        if (binding === null || !origin || !controller.uiSessionInputs(binding)) return null
        return { origin, credential: () => controller.uiSessionInputs(binding)?.credential ?? null, fetch, newRequestId: () => 'request-1' }
      },
      knowledgeBaseId: () => controller.status().knowledgeBaseIds[0] ?? null,
      readFile: async () => Buffer.from('hello'),
      stat: async () => ({ size: 5, isFile: true }),
      onChange: () => {},
      setTimer: (fn) => {
        const id = ++nextTimer
        timers.set(id, fn)
        return id
      },
      clearTimer: (id) => { timers.delete(id as number) },
    })
    try {
      const [upload] = await uploads.add([path.join(dir, 'fixture.md')])
      expect(upload).toMatchObject({ state: 'saved', jobId: 'job-1' })
      expect(timers.size).toBe(1)
      const [pollId, poll] = [...timers][0]!
      timers.delete(pollId)
      poll()
      await vi.waitFor(() => expect(uploads.list()[0]?.state).toBe('processing'))
      expect(await uploads.cancel(upload!.uploadId)).toMatchObject({ state: 'cancelled' })
      expect(fetch.mock.calls.map(([url]) => url)).toEqual([
        `${root}/health`, `${providerBase}/init`, `${providerBase}/ready`,
        `${providerBase}/ingest`, `${providerBase}/jobs/get`, `${providerBase}/jobs/cancel`,
      ])
    } finally {
      uploads.dispose()
    }
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
      ok: true, outcome: 'existing', knowledgeBaseId: KB, configMismatch: false, activeNow: true,
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
    expect(second).toEqual({ ok: true, outcome: 'created', knowledgeBaseId: KB, configMismatch: false, activeNow: true })
    expect(bodyOf(calls[1]!.init).idempotency_key).toBe(saved.idempotency_key)
    expect(fs.existsSync(pending)).toBe(false)
    expect(JSON.stringify(second)).not.toContain(API_KEY)
    expect(JSON.stringify(second)).not.toContain(CODE)

    // the activation file is spent on success; a new activation starts a fresh run
    expect(fs.existsSync(path.join(dir, 'knowledge-activation.json'))).toBe(false)
    setEndpoint('')
    writeActivation(dir, GOOD_FILE)
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

  it('a failed save is not_saved and keeps the pending key; a user save that landed first is already_bound', async () => {
    writeActivation(dir, GOOD_FILE)
    const failedSave = harness({ responses: [{ status: 200, body: okInit() }], adoptAs: 'not_saved' })
    expect(await failedSave.onboarding.run({ localOnly: false })).toEqual({ ok: false, code: 'not_saved' })
    expect(failedSave.configures).toHaveLength(1)
    expect(fs.existsSync(path.join(dir, PENDING))).toBe(true)

    const bound = harness({ responses: [{ status: 200, body: okInit() }], adoptAs: 'bound' })
    expect(await bound.onboarding.run({ localOnly: false })).toEqual({ ok: false, code: 'already_bound' })
  })

  it('the user\'s own setup winning spends the activation; being overtaken by another change is retryable', async () => {
    writeActivation(dir, GOOD_FILE)
    const bound = harness({ responses: [{ status: 200, body: okInit() }], adoptAs: 'bound' })
    expect(await bound.onboarding.run({ localOnly: false })).toEqual({ ok: false, code: 'already_bound' })
    expect(fs.existsSync(path.join(dir, PENDING))).toBe(false)
    expect(fs.existsSync(path.join(dir, 'knowledge-activation.json'))).toBe(false)

    writeActivation(dir, GOOD_FILE)
    const overtaken = harness({ responses: [{ status: 200, body: okInit() }], adoptAs: 'overtaken' })
    expect(await overtaken.onboarding.run({ localOnly: false })).toEqual({ ok: false, code: 'in_progress' })
    expect(fs.existsSync(path.join(dir, PENDING))).toBe(true)
  })

  it('a pending key belongs to its activation: a new activation sends a new key', async () => {
    writeActivation(dir, GOOD_FILE)
    const h = harness({ responses: [{ status: 0, throw: true }] })
    await h.onboarding.run({ localOnly: false })
    await h.onboarding.run({ localOnly: false })
    const first = bodyOf(h.calls[0]!.init).idempotency_key
    expect(bodyOf(h.calls[1]!.init).idempotency_key).toBe(first)
    writeActivation(dir, { ...GOOD_FILE, activation_code: 'another-activation-code' })
    await h.onboarding.run({ localOnly: false })
    expect(bodyOf(h.calls[2]!.init).idempotency_key).not.toBe(first)
  })

  it('saved while the daemon is not up is a success that is not active yet, and the files are spent', async () => {
    writeActivation(dir, GOOD_FILE)
    const h = harness({ responses: [{ status: 200, body: okInit() }], adoptAs: 'inactive' })
    expect(await h.onboarding.run({ localOnly: false })).toMatchObject({ ok: true, activeNow: false })
    expect(fs.existsSync(path.join(dir, PENDING))).toBe(false)
    expect(fs.existsSync(path.join(dir, 'knowledge-activation.json'))).toBe(false)
  })

  it('a local-only call while a remote run is in flight is answered on its own', async () => {
    writeActivation(dir, GOOD_FILE)
    const { onboarding } = harness({ responses: [{ status: 200, body: okInit() }] })
    const remote = onboarding.run({ localOnly: false })
    expect(await onboarding.run({ localOnly: true })).toEqual({ ok: false, code: 'local_unavailable' })
    await expect(remote).resolves.toMatchObject({ ok: true })
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
      localOffered: false,
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

// L11-1 (KB-C4-05 personal Lite, chat entry): 「只留在本机」 opens the local Lite the way the settings page does.
describe('local Lite onboarding (L11-1)', () => {
  let dir = ''

  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kb-onboard-local-'))
  })
  afterEach(() => {
    fs.rmSync(dir, { recursive: true, force: true })
  })

  function localHarness(opts: {
    endpoint?: string
    source?: 'remote' | 'local'
    /** false: this App has no Lite. */
    lite?: false | { offered?: boolean; after?: LiteKnowledgeStatus }
    /** unsaved: nothing written. applied_failed: the real controller saves first, then fails to apply
     * (daemon down, host not started) and still answers ok:false — the source did change (Codex C1). */
    setSourceAs?: 'ok' | 'unsaved' | 'apply_failed'
    controller?: boolean
  } = {}): {
    onboarding: ReturnType<typeof knowledgeOnboarding>
    events: string[]
    calls: string[]
    source: () => 'remote' | 'local'
  } {
    const events: string[] = []
    const calls: string[] = []
    let source: 'remote' | 'local' = opts.source ?? 'remote'
    const endpoint = opts.endpoint ?? ''
    const controller = {
      status: () => ({
        enabled: source === 'local', endpoint, knowledgeBaseIds: [] as string[], source,
        activeKnowledgeBaseIds: [] as string[], hasKey: endpoint !== '', active: false,
      }),
      adopt: async () => {
        events.push('adopt')
        return { saved: false as const, reason: 'invalid' as const }
      },
      setSource: async (next: 'remote' | 'local') => {
        events.push(`setSource:${next}`)
        if (opts.setSourceAs === 'unsaved') return { ok: false as const, reason: 'not_saved' as const }
        source = next
        if (opts.setSourceAs === 'apply_failed') return { ok: false as const, reason: 'unavailable' as const }
        return { ok: true as const }
      },
    }
    let on = false
    const after: LiteKnowledgeStatus = opts.lite && opts.lite.after ? opts.lite.after : { phase: 'ready' }
    const offered = opts.lite ? opts.lite.offered ?? true : true
    const lite: OnboardLite | null = opts.lite === false ? null : {
      offered: () => offered,
      enable: async () => {
        events.push('enable')
        on = true
        await Promise.resolve()
      },
      disable: async () => {
        events.push('disable')
        on = false
      },
      status: (): LiteKnowledgeStatus => (on ? after : { phase: 'off' }),
    }
    const onboarding = knowledgeOnboarding({
      dir,
      controller: () => (opts.controller === false ? null : controller as unknown as KnowledgeController),
      lite: () => lite,
      fetch: async (url) => {
        calls.push(url)
        throw new Error('local onboarding never reaches the network')
      },
      sleep: async () => {},
    })
    return { onboarding, events, calls, source: () => source }
  }

  it('saves local, then installs and launches; never reads the activation or the network', async () => {
    writeActivation(dir, { endpoint: 'not-a-url', activation_code: 'x' }, 0o600)
    const h = localHarness()
    expect(await h.onboarding.run({ localOnly: true })).toEqual({ ok: true, source: 'local', activeNow: true })
    expect(h.events).toEqual(['setSource:local', 'enable'])
    expect(h.calls).toEqual([])
    expect(h.source()).toBe('local')
    expect(fs.existsSync(path.join(dir, PENDING))).toBe(false)
    expect(fs.existsSync(path.join(dir, ACTIVATION))).toBe(true)
  })

  it('a Lite still starting is kept, reported as not active yet', async () => {
    const h = localHarness({ lite: { after: { phase: 'starting' } } })
    expect(await h.onboarding.run({ localOnly: true })).toEqual({ ok: true, source: 'local', activeNow: false })
    expect(h.events).toEqual(['setSource:local', 'enable'])
    expect(h.source()).toBe('local')
  })

  it('an install failure puts the source back and stops the Lite', async () => {
    const h = localHarness({ lite: { after: { phase: 'install_failed', failure: 'network' } } })
    expect(await h.onboarding.run({ localOnly: true })).toEqual({ ok: false, code: 'local_install_failed' })
    expect(h.events).toEqual(['setSource:local', 'enable', 'setSource:remote', 'disable'])
    expect(h.source()).toBe('remote')
  })

  it.each([
    [{ phase: 'failed', reason: 'start_failed' }],
    [{ phase: 'failed', reason: 'embedding_unavailable' }],
    [{ phase: 'unavailable' }],
  ] as Array<[LiteKnowledgeStatus]>)('a Lite that did not come up (%j) puts the source back', async (after) => {
    const h = localHarness({ lite: { after } })
    expect(await h.onboarding.run({ localOnly: true })).toEqual({ ok: false, code: 'local_start_failed' })
    expect(h.events).toEqual(['setSource:local', 'enable', 'setSource:remote', 'disable'])
    expect(h.source()).toBe('remote')
  })

  it('no Lite in this App, or no download source, changes nothing', async () => {
    for (const lite of [false, { offered: false }] as const) {
      const h = localHarness({ lite })
      expect(await h.onboarding.run({ localOnly: true })).toEqual({ ok: false, code: 'local_unavailable' })
      expect(h.events).toEqual([])
      expect(h.source()).toBe('remote')
    }
  })

  it('a knowledge base already in use is left alone: a saved remote endpoint, or local already the source', async () => {
    const remote = localHarness({ endpoint: 'https://already.example' })
    expect(await remote.onboarding.run({ localOnly: true })).toEqual({ ok: false, code: 'already_bound' })
    expect(remote.events).toEqual([])
    const local = localHarness({ source: 'local' })
    expect(await local.onboarding.run({ localOnly: true })).toEqual({ ok: false, code: 'already_bound' })
    expect(local.events).toEqual([])
  })

  it('no controller is unavailable and touches nothing', async () => {
    const h = localHarness({ controller: false })
    expect(await h.onboarding.run({ localOnly: true })).toEqual({ ok: false, code: 'unavailable' })
    expect(h.events).toEqual([])
  })

  it('a source that could not be saved is not_saved and the Lite is not touched', async () => {
    const h = localHarness({ setSourceAs: 'unsaved' })
    expect(await h.onboarding.run({ localOnly: true })).toEqual({ ok: false, code: 'not_saved' })
    expect(h.events).toEqual(['setSource:local'])
    expect(h.source()).toBe('remote')
  })

  it('a source saved but not applied (daemon down) still goes on with the Lite, like the settings page', async () => {
    const h = localHarness({ setSourceAs: 'apply_failed' })
    expect(await h.onboarding.run({ localOnly: true })).toEqual({ ok: true, source: 'local', activeNow: true })
    expect(h.events).toEqual(['setSource:local', 'enable'])
    expect(h.source()).toBe('local')
    const failing = localHarness({ setSourceAs: 'apply_failed', lite: { after: { phase: 'failed', reason: 'start_failed' } } })
    expect(await failing.onboarding.run({ localOnly: true })).toEqual({ ok: false, code: 'local_start_failed' })
    expect(failing.events).toEqual(['setSource:local', 'enable', 'setSource:remote', 'disable'])
    expect(failing.source()).toBe('remote')
  })

  it('two local runs at once share one', async () => {
    const h = localHarness()
    const [a, b] = await Promise.all([h.onboarding.run({ localOnly: true }), h.onboarding.run({ localOnly: true })])
    expect(a).toEqual({ ok: true, source: 'local', activeNow: true })
    expect(b).toEqual(a)
    expect(h.events.filter((e) => e === 'enable')).toHaveLength(1)
  })

  it('preflight offers the local Lite when this build has one, and counts local as bound', async () => {
    const offered = await localHarness().onboarding.preflight()
    expect(offered).toMatchObject({ activated: false, alreadyBound: false, localOffered: true, reachable: null })
    expect((await localHarness({ lite: { offered: false } }).onboarding.preflight()).localOffered).toBe(false)
    expect((await localHarness({ lite: false }).onboarding.preflight()).localOffered).toBe(false)
    expect((await localHarness({ source: 'local' }).onboarding.preflight()).alreadyBound).toBe(true)
  })
})
