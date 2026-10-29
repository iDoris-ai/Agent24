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

vi.mock('./backend-manager', () => ({ daemonConnection: () => null }))

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
    sent.push({ method, path: requestPath.split('?')[0] ?? '', body })
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

  it('takes the endpoints and knowledge bases the daemon takes', () => {
    for (const ok of ['https://kb.example/api/v1', 'http://127.0.0.1:8080/api/v1', 'http://[::1]:9000']) {
      expect(validEndpoint(ok), ok).toBe(true)
    }
    for (const bad of ['http://kb.example', 'http://localhost:8080', 'https://u:p@kb.example', 'https://kb.example/?q=1', 'ftp://x', '', 1]) {
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
    expect(sent[0]).toMatchObject({
      method: 'PUT', path: BINDING,
      body: JSON.stringify({ endpoint: GOOD.endpoint, knowledge_base_ids: GOOD.knowledgeBaseIds }),
    })
    expect(sent[1]).toMatchObject({ method: 'PUT', path: `${BINDING}/credential` })
    expect(JSON.parse(sent[1]!.body!)).toEqual({ kind: 'api_key', value: 'sk-SECRET', expires_at: now + 24 * 3600 })
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

  it('keeps a later key without sending it anywhere but the daemon', async () => {
    const c = make()
    await c.configure(GOOD)
    await c.setEnabled(true)
    sent = []
    await c.configure({ endpoint: GOOD.endpoint, knowledgeBaseIds: ['kb-a'] })
    // The key is kept; the new scope is activated with it.
    expect(c.status().hasKey).toBe(true)
    expect(JSON.parse(sent[0]!.body!)).toEqual({ endpoint: GOOD.endpoint, knowledge_base_ids: ['kb-a'] })
    const file = fs.readFileSync(path.join(dir, 'knowledge-settings.json'), 'utf8')
    expect(file).not.toContain('SECRET')
  })
})
