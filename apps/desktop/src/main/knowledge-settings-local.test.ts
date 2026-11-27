// Loop 9 3c: the local Lite as the Knowledge source, one at a time with the
// remote server, on the one daemon binding 'default'.
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { KnowledgeHost, SequenceClock, type HostTransport } from './knowledge-host'
import { KnowledgeLoginStore, type SecretBox } from './knowledge-logins'
import { KNOWLEDGE_BINDING, KnowledgeController, KnowledgeSettingsStore, type LocalKnowledgeSource } from './knowledge-settings'

vi.mock('./backend-manager', () => ({ daemonConnection: () => null, daemonElevation: () => null }))

const box: SecretBox = {
  isEncryptionAvailable: () => true,
  encryptString: (plain) => Buffer.from(`enc:${Buffer.from(plain).toString('base64')}`),
  decryptString: (cipher) => Buffer.from(cipher.toString().slice(4), 'base64').toString(),
}

const BINDING = `/api/v1/host/knowledge/bindings/${KNOWLEDGE_BINDING}`
const REMOTE = { endpoint: 'https://kb.example/api/v1', knowledgeBaseIds: ['kb-remote'], apiKey: 'sk-REMOTE' }

class FakeLite implements LocalKnowledgeSource {
  current: { origin: string; apiKey: string; knowledgeBaseId: string } | null = null
  private readonly listeners = new Set<() => void>()
  credential() { return this.current }
  onStatus(listener: () => void) {
    this.listeners.add(listener)
    return () => { this.listeners.delete(listener) }
  }
  set(value: FakeLite['current']) {
    this.current = value
    for (const l of this.listeners) l()
  }
}

const flush = async () => { for (let i = 0; i < 20; i++) await new Promise((r) => setImmediate(r)) }

describe('KnowledgeController local source (Loop 9 3c)', () => {
  let dir = ''
  let sent: Array<{ method: string; path: string; body?: string }> = []
  let logins: KnowledgeLoginStore
  let host: KnowledgeHost
  let lite: FakeLite
  const now = 1_800_000_000
  const transport: HostTransport = async (_conn, method, requestPath, body) => {
    sent.push({ method, path: requestPath, body })
    return { status: 204 }
  }
  const make = () => new KnowledgeController(host, logins, new KnowledgeSettingsStore(dir), { nowSeconds: () => now, local: lite })
  const activations = () => sent.filter((s) => s.method === 'PUT' && s.path === BINDING).map((s) => JSON.parse(s.body ?? '{}'))
  const credentials = () => sent.filter((s) => s.method === 'PUT' && s.path === `${BINDING}/credential`).map((s) => JSON.parse(s.body ?? '{}'))

  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kb-local-'))
    sent = []
    lite = new FakeLite()
    logins = new KnowledgeLoginStore(dir, box, 'darwin')
    host = new KnowledgeHost(logins, {
      connection: () => ({ port: 1, authHeaders: { Authorization: 'Bearer t' } }),
      transport,
      clock: new SequenceClock(() => 1_000),
      nowSeconds: () => now,
    })
    host.start()
  })
  afterEach(() => { fs.rmSync(dir, { recursive: true, force: true }) })

  it('binds the running Lite with its in-memory key, keeps the remote settings and key, and switches back', async () => {
    const controller = make()
    expect((await controller.configure(REMOTE)).ok).toBe(true)
    expect((await controller.setEnabled(true)).ok).toBe(true)
    const remoteView = controller.viewBinding()

    // Local chosen while the Lite is not up yet: On, nothing bound.
    expect((await controller.setSource('local')).ok).toBe(true)
    expect(controller.status()).toMatchObject({ enabled: true, source: 'local', endpoint: REMOTE.endpoint, active: false, activeKnowledgeBaseIds: [] })
    expect(controller.bindingForRun()).toBeNull()
    expect(controller.uiSessionInputs(controller.viewBinding() ?? '')).toBeNull()

    // The Lite comes up: the binding follows on its own, with the Lite's key as C.
    sent = []
    lite.set({ origin: 'http://127.0.0.1:41234', apiKey: 'lite-KEY-1', knowledgeBaseId: 'kb-local' })
    await flush()
    expect(activations().at(-1)).toMatchObject({ endpoint: 'http://127.0.0.1:41234/api/v1', knowledge_base_ids: ['kb-local'] })
    expect(credentials().at(-1)).toMatchObject({ kind: 'api_key', value: 'lite-KEY-1' })
    expect(controller.bindingForRun()).toBe(KNOWLEDGE_BINDING)
    expect(controller.activeTarget()).toEqual({ endpoint: 'http://127.0.0.1:41234/api/v1', knowledgeBaseIds: ['kb-local'] })
    const localView = controller.viewBinding()
    expect(localView).not.toBe(remoteView)
    expect(controller.uiSessionInputs(localView ?? '')?.credential).toBe('lite-KEY-1')
    // The remote key is where it was; the Lite's key never reaches the login store or the settings file.
    expect(logins.list().get(KNOWLEDGE_BINDING)?.material).toBe('sk-REMOTE')
    for (const name of fs.readdirSync(dir)) {
      const body = fs.readFileSync(path.join(dir, name)).toString()
      expect(body, name).not.toContain('lite-KEY-1')
      expect(body, name).not.toContain(Buffer.from('lite-KEY-1').toString('base64'))
    }

    // A restart hands out a new key: new view, new C.
    sent = []
    lite.set({ origin: 'http://127.0.0.1:41234', apiKey: 'lite-KEY-2', knowledgeBaseId: 'kb-local' })
    await flush()
    expect(credentials().at(-1)).toMatchObject({ value: 'lite-KEY-2' })
    expect(controller.viewBinding()).not.toBe(localView)

    // Back to remote: the saved server and key, untouched.
    sent = []
    expect((await controller.setSource('remote')).ok).toBe(true)
    expect(activations().at(-1)).toMatchObject({ endpoint: REMOTE.endpoint, knowledge_base_ids: REMOTE.knowledgeBaseIds })
    expect(credentials().at(-1)).toMatchObject({ value: 'sk-REMOTE' })
    expect(controller.activeTarget()).toEqual({ endpoint: REMOTE.endpoint, knowledgeBaseIds: REMOTE.knowledgeBaseIds })
    expect(controller.uiSessionInputs(controller.viewBinding() ?? '')?.credential).toBe('sk-REMOTE')

    // The choice survives a restart of main.
    expect(make().status().source).toBe('remote')
    await controller.setSource('local')
    expect(make().status().source).toBe('local')
  })

  it('0.7.127 acceptance: the local endpoint has the same shape as a remote one (the API base, /api/v1)', async () => {
    const controller = make()
    lite.current = { origin: 'http://127.0.0.1:41234', apiKey: 'lite-KEY', knowledgeBaseId: 'kb-local' }
    await controller.setSource('local')
    // The daemon and the K2 check both append /agent24/knowledge/<op> to the endpoint
    // (agent24-knowledge http.rs `{base}{op.path()}`, knowledge-reachability.ts); the Lite serves them under /api/v1.
    const pushed = activations().at(-1)?.endpoint as string
    expect(pushed).toBe('http://127.0.0.1:41234/api/v1')
    expect(new URL(`${pushed}/agent24/knowledge/ready`).pathname).toBe('/api/v1/agent24/knowledge/ready')
    expect(controller.probeInputs()).toMatchObject({ endpoint: 'http://127.0.0.1:41234/api/v1', key: 'lite-KEY' })
    // Same shape as what the remote settings hold.
    expect(new URL(pushed).pathname).toBe(new URL(REMOTE.endpoint).pathname)
  })

  it('drops the binding when the Lite goes away while it is the source', async () => {
    const controller = make()
    lite.current = { origin: 'http://127.0.0.1:41234', apiKey: 'lite-KEY', knowledgeBaseId: 'kb-local' }
    await controller.setSource('local')
    expect(controller.bindingForRun()).toBe(KNOWLEDGE_BINDING)
    sent = []
    lite.set(null)
    await flush()
    expect(sent.some((s) => s.method === 'DELETE' && s.path === BINDING)).toBe(true)
    expect(controller.bindingForRun()).toBeNull()
    expect(controller.probeInputs()).toBeNull()
  })

  it('a Lite that changes while remote is the source changes nothing', async () => {
    const controller = make()
    await controller.configure(REMOTE)
    await controller.setEnabled(true)
    const view = controller.viewBinding()
    sent = []
    lite.set({ origin: 'http://127.0.0.1:41234', apiKey: 'lite-KEY', knowledgeBaseId: 'kb-local' })
    await flush()
    expect(sent).toEqual([])
    expect(controller.viewBinding()).toBe(view)
  })

  it('onboarding fills the remote settings but leaves the local choice alone', async () => {
    const controller = make()
    await controller.setSource('local')
    const adopted = await controller.adopt({ endpoint: REMOTE.endpoint, knowledgeBaseIds: ['kb-default'], apiKey: 'sk-ONBOARD' })
    expect(adopted.saved).toBe(true)
    expect(controller.status()).toMatchObject({ source: 'local', endpoint: REMOTE.endpoint })
    expect(controller.activeTarget()).toBeNull()
  })
})
