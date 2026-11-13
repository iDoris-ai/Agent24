import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { KnowledgeHost, SequenceClock, type HostTransport } from './knowledge-host'
import { KnowledgeLoginStore, type SecretBox } from './knowledge-logins'
import {
  KNOWLEDGE_BINDING,
  KnowledgeController,
  KnowledgeSettingsStore,
  validEndpoint,
  validKnowledgeBaseIds,
} from './knowledge-settings'

vi.mock('./backend-manager', () => ({ daemonConnection: () => null, daemonElevation: () => null }))

const box: SecretBox = {
  isEncryptionAvailable: () => true,
  encryptString: (plain) => Buffer.from(`enc:${Buffer.from(plain).toString('base64')}`),
  decryptString: (cipher) => Buffer.from(cipher.toString().slice(4), 'base64').toString(),
}

interface Sent { method: string; path: string; body?: string }

const BINDING = `/api/v1/host/knowledge/bindings/${KNOWLEDGE_BINDING}`
const GOOD = { endpoint: 'https://kb.example/api/v1', knowledgeBaseIds: ['kb-a', 'kb-b'], apiKey: 'sk-SECRET' }

describe('KnowledgeController (M3)', () => {
  let dir = ''
  let sent: Sent[] = []
  let now = 1_800_000_000
  let logins: KnowledgeLoginStore
  let host: KnowledgeHost

  const transport: HostTransport = async (_conn, method, requestPath, body) => {
    // As sent: the daemon answers 400 to any query on these routes (K-B6).
    sent.push({ method, path: requestPath, body })
    return { status: 204 }
  }
  const make = () => new KnowledgeController(host, logins, new KnowledgeSettingsStore(dir), { nowSeconds: () => now })

  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kb-settings-'))
    sent = []
    logins = new KnowledgeLoginStore(dir, box, 'darwin')
    host = new KnowledgeHost(logins, {
      connection: () => ({ port: 1, authHeaders: { Authorization: 'Bearer t' } }),
      transport,
      clock: new SequenceClock(() => 1_000),
      nowSeconds: () => now,
    })
    host.start()
  })
  afterEach(() => {
    fs.rmSync(dir, { recursive: true, force: true })
  })

  it('tells listeners after every change, with the new settings already in effect (KB-C2-03)', async () => {
    const controller = make()
    const seen: Array<{ enabled: boolean; endpoint: string }> = []
    const off = controller.onChange(() => {
      const s = controller.status()
      seen.push({ enabled: s.enabled, endpoint: s.endpoint })
    })
    await controller.configure(GOOD)
    await controller.setEnabled(true)
    await controller.setEnabled(false)
    expect(seen).toEqual([
      { enabled: false, endpoint: GOOD.endpoint },
      { enabled: true, endpoint: GOOD.endpoint },
      { enabled: false, endpoint: GOOD.endpoint },
    ])
    off()
    await controller.setEnabled(true)
    expect(seen).toHaveLength(3)
  })

  it('names a new view binding for a new key or endpoint, none while Off (KB-C2-03)', async () => {
    const controller = make()
    expect(controller.viewBinding()).toBeNull()
    await controller.configure(GOOD)
    expect(controller.viewBinding()).toBeNull()
    await controller.setEnabled(true)
    const first = controller.viewBinding()
    expect(first).not.toBeNull()
    expect(first).not.toContain(GOOD.apiKey)
    expect(first).not.toContain('kb.example')
    // Same endpoint and knowledge bases, another key: possibly another principal.
    await controller.configure({ ...GOOD, apiKey: 'sk-OTHER' })
    const second = controller.viewBinding()
    expect(second).not.toBeNull()
    expect(second).not.toBe(first)
    // Only the knowledge bases change: the same principal, the same binding.
    await controller.configure({ endpoint: GOOD.endpoint, knowledgeBaseIds: ['kb-a'] })
    expect(controller.viewBinding()).toBe(second)
    await controller.setEnabled(false)
    expect(controller.viewBinding()).toBeNull()
  })

  it('gives the UI session C, the generation and the knowledge bases only for the binding in effect (KB-C2-04a)', async () => {
    const controller = make()
    await controller.configure(GOOD)
    await controller.setEnabled(true)
    const first = controller.viewBinding()!
    const inputs = controller.uiSessionInputs(first)
    expect(inputs).toEqual({ credential: GOOD.apiKey, generation: expect.any(Number) })
    // U7.1: never 0, also on a fresh launch with the saved settings (identity starts at 0 again).
    const restarted = make()
    expect(restarted.uiSessionInputs(restarted.viewBinding()!)?.generation).toBe(1)
    expect(controller.uiSessionInputs('default#999')).toBeNull()
    // The bases change within the binding: the session's inputs stay (E comes from C, not from the bases).
    await controller.configure({ endpoint: GOOD.endpoint, knowledgeBaseIds: ['kb-a'] })
    expect(controller.uiSessionInputs(first)).toEqual(inputs)
    // A new key: the old binding gets nothing, not even the new key.
    await controller.configure({ ...GOOD, apiKey: 'sk-OTHER' })
    expect(controller.uiSessionInputs(first)).toBeNull()
    const second = controller.viewBinding()!
    expect(controller.uiSessionInputs(second)).toMatchObject({ credential: 'sk-OTHER' })
    expect(controller.uiSessionInputs(second)!.generation).toBeGreaterThan(inputs!.generation)
    await controller.setEnabled(false)
    expect(controller.uiSessionInputs(second)).toBeNull()
  })

  it('a new key moves the view binding even when its change is overtaken after the key was written', async () => {
    const store = new KnowledgeSettingsStore(dir)
    const save = store.save.bind(store)
    let hold = false
    let release: () => void = () => {}
    let held = false
    store.save = async (settings) => {
      if (hold) {
        held = true
        await new Promise<void>((resolve) => { release = resolve })
      }
      return save(settings)
    }
    const controller = new KnowledgeController(host, logins, store, { nowSeconds: () => now })
    await controller.configure(GOOD)
    await controller.setEnabled(true)
    const first = controller.viewBinding()
    hold = true
    const a = controller.configure({ ...GOOD, apiKey: 'sk-NEW' })
    for (let i = 0; i < 50 && !held; i++) await new Promise((resolve) => setTimeout(resolve, 1))
    expect(held).toBe(true)
    hold = false
    const b = controller.configure({ endpoint: GOOD.endpoint, knowledgeBaseIds: ['kb-a'] })
    release()
    await Promise.all([a, b])
    expect(logins.list().get(KNOWLEDGE_BINDING)?.material).toBe('sk-NEW')
    expect(controller.viewBinding()).not.toBeNull()
    expect(controller.viewBinding()).not.toBe(first)
  })

  it('a failing listener does not stop the change', async () => {
    const controller = make()
    controller.onChange(() => { throw new Error('boom') })
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    expect(await controller.configure(GOOD)).toEqual({ ok: true })
    warn.mockRestore()
  })

  it('takes the endpoints and knowledge bases the daemon takes', () => {
    for (const ok of ['https://kb.example/api/v1', 'http://127.0.0.1:8080/api/v1', 'http://[::1]:9000', 'https://kb.example./api/v1']) {
      expect(validEndpoint(ok), ok).toBe(true)
    }
    for (const bad of ['http://kb.example', 'http://localhost:8080', 'https://u:p@kb.example', 'https://kb.example/?q=1', 'ftp://x', '', 1,
      'https://*.kb.example', 'https://a;b.kb.example', 'https://a,b.kb.example', 'https://kb.example:0/api/v1', 'http://127.0.0.1:0']) {
      expect(validEndpoint(bad), String(bad)).toBe(false)
    }
    expect(validKnowledgeBaseIds(['kb-a'])).toBe(true)
    for (const bad of [[], [' '], ['a', 'a'], Array.from({ length: 65 }, (_, i) => `kb-${i}`), 'kb-a', [1]]) {
      expect(validKnowledgeBaseIds(bad), JSON.stringify(bad)).toBe(false)
    }
  })

  it('starts Off and sends nothing but a deactivation until switched On', async () => {
    const c = make()
    expect(c.status()).toMatchObject({ enabled: false, active: false, hasKey: false })
    expect(await c.configure(GOOD)).toEqual({ ok: true })
    expect(sent.map((s) => s.method)).toEqual(['DELETE'])
    expect(c.bindingForRun()).toBeNull()
  })

  it('switched On, activates with the endpoint and pushes the key as an api_key C', async () => {
    const c = make()
    await c.configure(GOOD)
    sent = []
    expect(await c.setEnabled(true)).toEqual({ ok: true })
    // K-B6: the sequence is in the body, the path has no query.
    expect(sent[0]).toMatchObject({ method: 'PUT', path: BINDING })
    const activation = JSON.parse(sent[0]!.body!)
    expect(activation).toEqual({ seq: expect.any(Number), endpoint: GOOD.endpoint, knowledge_base_ids: GOOD.knowledgeBaseIds })
    expect(sent[1]).toMatchObject({ method: 'PUT', path: `${BINDING}/credential` })
    const credential = JSON.parse(sent[1]!.body!)
    expect(credential).toEqual({ seq: expect.any(Number), kind: 'api_key', value: 'sk-SECRET', expires_at: now + 24 * 3600 })
    expect(credential.seq).toBeGreaterThan(activation.seq)
    expect(c.bindingForRun()).toBe(KNOWLEDGE_BINDING)
    expect(c.status()).toMatchObject({ enabled: true, active: true, hasKey: true })
    expect(JSON.stringify(c.status())).not.toContain('SECRET')
  })

  it('stays On across a restart, and Off once turned Off, even when the file cannot be written', async () => {
    const c = make()
    await c.configure(GOOD)
    await c.setEnabled(true)
    const again = make()
    expect(again.status().enabled).toBe(true)

    const save = vi.spyOn(KnowledgeSettingsStore.prototype, 'save').mockRejectedValue(new Error('disk full'))
    sent = []
    expect(await again.setEnabled(false)).toEqual({ ok: false, reason: 'not_saved' })
    // Off took effect anyway: deactivated in the daemon, no binding for a new run.
    expect(sent.map((s) => [s.method, s.path])).toEqual([['DELETE', BINDING]])
    expect(again.bindingForRun()).toBeNull()

    // On does not take effect unless saved.
    sent = []
    expect(await again.setEnabled(true)).toEqual({ ok: false, reason: 'not_saved' })
    expect(sent).toEqual([])
    expect(again.bindingForRun()).toBeNull()
    save.mockRestore()
  })

  it('refuses bad settings and changes nothing', async () => {
    const c = make()
    for (const bad of [
      { ...GOOD, endpoint: 'http://kb.example' },
      { ...GOOD, knowledgeBaseIds: [] },
      { ...GOOD, apiKey: 'has space' },
      { ...GOOD, apiKey: '' },
    ]) {
      expect(await c.configure(bad)).toEqual({ ok: false, reason: 'invalid' })
    }
    expect(sent).toEqual([])
    expect(c.status()).toMatchObject({ endpoint: '', hasKey: false })
  })

  it('pushes C again on the tick only once it is about to run out', async () => {
    const c = make()
    await c.configure(GOOD)
    await c.setEnabled(true)
    sent = []
    await c.tick()
    expect(sent).toEqual([])
    now += 23 * 3600 + 1
    await c.tick()
    expect(sent.map((s) => s.path)).toEqual([`${BINDING}/credential`])
  })

  it('never sends a saved key to a new endpoint: the new address needs the key again', async () => {
    const c = make()
    await c.configure(GOOD)
    await c.setEnabled(true)
    sent = []
    expect(await c.configure({ endpoint: 'https://evil.example/api/v1', knowledgeBaseIds: ['kb-a'] })).toEqual({ ok: true })
    expect(c.status()).toMatchObject({ hasKey: false, active: false })
    expect(c.bindingForRun()).toBeNull()
    // Incomplete: dropped, and no C pushed.
    expect(sent.map((s) => [s.method, s.path])).toEqual([['DELETE', BINDING]])
    await c.tick()
    expect(sent.filter((s) => s.path.endsWith('/credential'))).toEqual([])
  })

  it('an On or a change still being saved cannot undo a later Off', async () => {
    const c = make()
    await c.configure(GOOD)
    let release = () => {}
    const gate = new Promise<void>((resolve) => { release = resolve })
    const real = KnowledgeSettingsStore.prototype.save
    const save = vi.spyOn(KnowledgeSettingsStore.prototype, 'save').mockImplementationOnce(async function (this: KnowledgeSettingsStore, s) {
      await gate
      return real.call(this, s)
    })
    const on = c.setEnabled(true)
    const off = c.setEnabled(false)
    release()
    expect(await on).toEqual({ ok: false, reason: 'not_saved' })
    expect(await off).toEqual({ ok: true })
    expect(c.status().enabled).toBe(false)
    expect(c.bindingForRun()).toBeNull()
    // Written in call order: the file ends Off.
    expect(make().status().enabled).toBe(false)
    save.mockRestore()
  })

  it('a change whose key is still being written cannot undo a later Off on disk', async () => {
    const c = make()
    await c.configure(GOOD)
    await c.setEnabled(true)
    let release = () => {}
    const gate = new Promise<void>((resolve) => { release = resolve })
    const real = KnowledgeLoginStore.prototype.put
    const put = vi.spyOn(KnowledgeLoginStore.prototype, 'put').mockImplementationOnce(async function (this: KnowledgeLoginStore, ...args) {
      await gate
      return real.apply(this, args)
    })
    const change = c.configure({ ...GOOD, apiKey: 'sk-NEW' })
    const off = c.setEnabled(false)
    release()
    expect(await change).toEqual({ ok: false, reason: 'not_saved' })
    expect(await off).toEqual({ ok: true })
    expect(c.bindingForRun()).toBeNull()
    expect(make().status().enabled).toBe(false)
    put.mockRestore()
  })

  it('a change overtaken while waiting its turn writes no key', async () => {
    const c = make()
    await c.configure(GOOD)
    let release = () => {}
    const gate = new Promise<void>((resolve) => { release = resolve })
    const real = KnowledgeSettingsStore.prototype.save
    const save = vi.spyOn(KnowledgeSettingsStore.prototype, 'save').mockImplementationOnce(async function (this: KnowledgeSettingsStore, s) {
      await gate
      return real.call(this, s)
    })
    const on = c.setEnabled(true)
    const change = c.configure({ ...GOOD, apiKey: 'sk-NEW' })
    const off = c.setEnabled(false)
    release()
    expect(await change).toEqual({ ok: false, reason: 'not_saved' })
    await on
    await off
    expect(logins.list().get(KNOWLEDGE_BINDING)?.material).toBe(GOOD.apiKey)
    save.mockRestore()
  })

  it('keeps the saved key when a change with a new key cannot be saved', async () => {
    const c = make()
    await c.configure(GOOD)
    const save = vi.spyOn(KnowledgeSettingsStore.prototype, 'save').mockRejectedValueOnce(new Error('disk full'))
    expect(await c.configure({ endpoint: 'https://other.example/api/v1', knowledgeBaseIds: ['kb-x'], apiKey: 'sk-OTHER' }))
      .toEqual({ ok: false, reason: 'not_saved' })
    expect(logins.list().get(KNOWLEDGE_BINDING)).toMatchObject({ profileId: GOOD.endpoint, material: GOOD.apiKey })
    expect(c.status()).toMatchObject({ endpoint: GOOD.endpoint, hasKey: true })
    save.mockRestore()
  })

  it('leaves no key behind when a first change cannot be saved', async () => {
    const c = make()
    const save = vi.spyOn(KnowledgeSettingsStore.prototype, 'save').mockRejectedValueOnce(new Error('disk full'))
    expect(await c.configure(GOOD)).toEqual({ ok: false, reason: 'not_saved' })
    expect(logins.list().get(KNOWLEDGE_BINDING)).toBeUndefined()
    save.mockRestore()
  })

  it('names no binding for a run until the daemon on this connection has taken the activation', async () => {
    const c = make()
    await c.configure(GOOD)
    const isActive = vi.spyOn(host, 'isActive').mockReturnValue(false)
    await c.setEnabled(true)
    expect(c.status()).toMatchObject({ enabled: true, active: false })
    expect(c.bindingForRun()).toBeNull()
    isActive.mockRestore()
    expect(c.bindingForRun()).toBe(KNOWLEDGE_BINDING)
  })

  it('keeps a later key without sending it anywhere but the daemon', async () => {
    const c = make()
    await c.configure(GOOD)
    await c.setEnabled(true)
    sent = []
    await c.configure({ endpoint: GOOD.endpoint, knowledgeBaseIds: ['kb-a'] })
    // The key is kept; the new scope is activated with it.
    expect(c.status().hasKey).toBe(true)
    expect(JSON.parse(sent[0]!.body!)).toEqual({ seq: expect.any(Number), endpoint: GOOD.endpoint, knowledge_base_ids: ['kb-a'] })
    const file = fs.readFileSync(path.join(dir, 'knowledge-settings.json'), 'utf8')
    expect(file).not.toContain('SECRET')
  })
})
