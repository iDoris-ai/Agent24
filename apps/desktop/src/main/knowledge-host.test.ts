import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import fs from 'node:fs'
import http from 'node:http'
import os from 'node:os'
import path from 'node:path'
import { KnowledgeHost, SequenceClock, elevatingTransport, unansweredRetryDelayMs, validBindingId, type HostTransport, type KnowledgeCredential } from './knowledge-host'
import { KnowledgeLoginStore, type KnowledgeLogin, type SecretBox } from './knowledge-logins'
import type { DaemonConnection } from './backend-manager'
import { proxyError } from './backend-proxy'
import { EnrolError, type ElevateRequest } from './daemon-enroll'
import type { BackendProxyRequest, BackendProxyResponse } from '../shared/ipc-types'

vi.mock('./backend-manager', () => ({ daemonConnection: () => null, daemonElevation: () => null }))

const box: SecretBox = {
  isEncryptionAvailable: () => true,
  encryptString: (plain) => Buffer.from(`enc:${Buffer.from(plain).toString('base64')}`),
  decryptString: (cipher) => Buffer.from(cipher.toString().slice(4), 'base64').toString(),
}

const login = (material: string): KnowledgeLogin => ({ profileId: 'p1', material, unusable: false, temporary: false })
const NOW = 1_800_000_000
const credential = (value: string, expiresAt = NOW + 600): KnowledgeCredential => ({ kind: 'bearer', value, expiresAt })

interface Sent {
  token: string
  method: string
  path: string
  seq: number
  body?: string
}

/** A daemon stand-in: records each request and answers from `respond`. */
function fakeDaemon() {
  const sent: Sent[] = []
  let conn: DaemonConnection | null = { port: 1, authHeaders: { Authorization: 'Bearer t1' } }
  let respond: (req: Sent) => { status: number; code?: string } = () => ({ status: 204 })
  const transport: HostTransport = async (connection, method, requestPath, body) => {
    // As sent: the daemon answers 400 to any query on these routes (K-B6).
    const route = requestPath
    let seq = Number.NaN
    if (body !== undefined) {
      try {
        const parsed = JSON.parse(body) as { seq?: unknown }
        if (typeof parsed.seq === 'number') seq = parsed.seq
      } catch {
        seq = Number.NaN
      }
    }
    const req: Sent = {
      token: connection.authHeaders['Authorization'] ?? '',
      method,
      path: route,
      seq,
      body,
    }
    sent.push(req)
    return respond(req)
  }
  return {
    sent,
    transport,
    connection: () => conn,
    setConnection: (next: DaemonConnection | null) => {
      conn = next
    },
    respondWith: (fn: typeof respond) => {
      respond = fn
    },
  }
}

const B1 = '/api/v1/host/knowledge/bindings/b1'

describe('KnowledgeHost (KB-HOST-01 PR-B2b)', () => {
  let dir = ''
  let store: KnowledgeLoginStore
  let daemon: ReturnType<typeof fakeDaemon>
  let now = 1_000
  const makeHost = (clock = new SequenceClock(() => now)) =>
    new KnowledgeHost(store, {
      connection: daemon.connection,
      transport: daemon.transport,
      clock,
      nowSeconds: () => NOW,
    })

  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kb-host-'))
    store = new KnowledgeLoginStore(dir, box, 'darwin')
    daemon = fakeDaemon()
    now = 1_000
  })
  afterEach(() => {
    fs.rmSync(dir, { recursive: true, force: true })
  })

  it('accepts exactly the binding ids the daemon accepts', () => {
    for (const ok of ['b1', 'kb:1', 'é', 'x'.repeat(256), 'a b', 'a%2Fb']) expect(validBindingId(ok), ok).toBe(true)
    for (const bad of [
      '',
      '.',
      '..',
      'x'.repeat(257),
      'é'.repeat(129),
      'a/b',
      'a\\b',
      'b\u0000',
      'b\n',
      'b\u007f',
      'b\u0085',
      '\ud800',
      1,
      null,
      undefined,
    ]) {
      expect(validBindingId(bad), JSON.stringify(bad)).toBe(false)
    }
  })

  it('numbers changes by the wall clock, strictly growing within a process and across a restart', async () => {
    const clock = new SequenceClock(() => now)
    expect([clock.next(), clock.next(), clock.next()]).toEqual([1_000, 1_001, 1_002])
    now = 900 // the clock went back: still growing
    expect(clock.next()).toBe(1_003)

    // The host restarts while the daemon (and its capability) stays.
    now = 5_000
    const first = makeHost()
    first.start()
    await first.activate('b1')
    now = 9_000
    const second = makeHost()
    second.start()
    await second.deactivate('b1')
    expect(daemon.sent.map((r) => r.seq)).toEqual([5_000, 9_000])
    expect(daemon.sent[1]!.seq).toBeLessThan(Number.MAX_SAFE_INTEGER / 1_000)
  })

  it('loads the logins once, and refuses every change before that', async () => {
    const load = vi.spyOn(store, 'load')
    const host = makeHost()
    for (const result of await Promise.all([
      host.activate('b1'),
      host.pushCredential('b1', credential('c')),
      host.destroy('b1'),
      host.refresh('b1', async () => credential('c')),
    ])) {
      expect(result).toEqual({ ok: false, reason: 'not_started' })
    }
    expect(daemon.sent).toEqual([])
    expect(load).not.toHaveBeenCalled()
    expect(host.start()).toEqual({ ok: true })
    expect(host.start()).toEqual({ ok: true })
    expect(load).toHaveBeenCalledTimes(1)
  })

  it('reports a load that throws as unreadable', () => {
    vi.spyOn(store, 'load').mockImplementation(() => {
      throw new Error('boom')
    })
    expect(makeHost().start()).toEqual({ ok: false, reason: 'unreadable' })
  })

  it('activates, pushes, revokes and deactivates over the private routes', async () => {
    const host = makeHost()
    host.start()
    expect(await host.activate('b1')).toEqual({ ok: true })
    expect(await host.pushCredential('b1', { kind: 'api_key', value: 'KEY', expiresAt: NOW + 60 })).toEqual({ ok: true })
    expect(await host.revokeCredential('b1')).toEqual({ ok: true })
    expect(await host.deactivate('b1')).toEqual({ ok: true })
    expect(daemon.sent.map((r) => `${r.method} ${r.path}`)).toEqual([
      `PUT ${B1}`,
      `PUT ${B1}/credential`,
      `DELETE ${B1}/credential`,
      `DELETE ${B1}`,
    ])
    expect(JSON.parse(daemon.sent[1]!.body!)).toEqual({ seq: 1_001, kind: 'api_key', value: 'KEY', expires_at: NOW + 60 })
    expect(daemon.sent.every((r) => r.token === 'Bearer t1')).toBe(true)
  })

  it('activates with the endpoint as a body, and replays it to a new daemon', async () => {
    const host = makeHost()
    host.start()
    await host.activate('b1', { endpoint: 'https://kb.example/api/v1', knowledgeBaseIds: ['kb-a'] })
    const body = JSON.stringify({ seq: 1_000, endpoint: 'https://kb.example/api/v1', knowledge_base_ids: ['kb-a'] })
    expect(daemon.sent[0]).toMatchObject({ method: 'PUT', path: B1, body })
    expect(host.isActive('b1')).toBe(true)
    // A new daemon has not taken it until the replay lands.
    let release = () => {}
    daemon.respondWith(() => ({ status: 204 }))
    daemon.setConnection({ port: 2, authHeaders: { Authorization: 'Bearer t2' } })
    const gate = new Promise<void>((resolve) => { release = resolve })
    const transport = daemon.transport
    const held: HostTransport = async (...args) => { await gate; return transport(...args) }
    const replaying = new KnowledgeHost(store, { connection: daemon.connection, transport: held, clock: new SequenceClock(() => now), nowSeconds: () => NOW })
    replaying.start()
    void replaying.activate('b1', { endpoint: 'https://kb.example/api/v1', knowledgeBaseIds: ['kb-a'] })
    expect(replaying.isActive('b1')).toBe(false)
    release()
    await vi.waitFor(() => expect(replaying.isActive('b1')).toBe(true))
    host.sync()
    await vi.waitFor(() => expect(daemon.sent.filter((r) => r.token === 'Bearer t2' && r.method === 'PUT' && r.body === body).length).toBeGreaterThan(0))
    await host.deactivate('b1')
    expect(host.isActive('b1')).toBe(false)
  })

  it('does not report a binding active on a restarted daemon before the replay lands, even ahead of the timer', async () => {
    const host = makeHost()
    host.start()
    await host.activate('b1', { endpoint: 'https://kb.example/api/v1', knowledgeBaseIds: ['kb-a'] })
    expect(host.isActive('b1')).toBe(true)
    daemon.setConnection({ port: 2, authHeaders: { Authorization: 'Bearer t2' } })
    // No sync() from the timer yet: asking alone notices the new daemon, and
    // the replay it queues has not landed when the answer is given.
    expect(host.isActive('b1')).toBe(false)
    await vi.waitFor(() => expect(host.isActive('b1')).toBe(true))
    expect(daemon.sent.some((r) => r.token === 'Bearer t2' && r.method === 'PUT' && r.path === B1)).toBe(true)
  })

  it('drops a binding at once, ahead of a refresh still waiting, which then sends nothing', async () => {
    const host = makeHost()
    host.start()
    await store.put('b1', login('m'))
    await host.activate('b1')
    let release = () => {}
    const pending = host.refresh('b1', () => new Promise((resolve) => { release = () => resolve(credential('late')) }))
    const off = await host.deactivate('b1')
    expect(off).toEqual({ ok: true })
    expect(daemon.sent.at(-1)).toMatchObject({ method: 'DELETE', path: B1 })
    release()
    expect(await pending).toEqual({ ok: false, reason: 'binding_inactive' })
    expect(daemon.sent.filter((r) => r.path === `${B1}/credential`)).toEqual([])
  })

  it('drops a binding at once, ahead of an activation queued behind a refresh, which then sends nothing', async () => {
    const host = makeHost()
    host.start()
    await store.put('b1', login('m'))
    await host.activate('b1')
    let release = () => {}
    const pending = host.refresh('b1', () => new Promise((resolve) => { release = () => resolve(credential('late')) }))
    const queued = host.activate('b1', { endpoint: 'https://kb.example/api/v1', knowledgeBaseIds: ['kb'] })
    expect(await host.deactivate('b1')).toEqual({ ok: true })
    const before = daemon.sent.length
    release()
    expect(await pending).toEqual({ ok: false, reason: 'binding_inactive' })
    expect(await queued).toEqual({ ok: false, reason: 'binding_inactive' })
    expect(daemon.sent.slice(before)).toEqual([])
    expect(host.isActive('b1')).toBe(false)
    expect(await host.pushCredential('b1', credential('after'))).toEqual({ ok: false, reason: 'binding_inactive' })
  })

  it('does not let a stale retry of an Off drop an activation that came after it', async () => {
    const host = makeHost()
    host.start()
    await host.activate('b1')
    let release = () => {}
    let first = true
    daemon.respondWith(((req: Sent) => {
      if (req.method === 'DELETE' && first) {
        first = false
        return new Promise((resolve) => { release = () => resolve({ status: 409, code: 'stale_sequence' }) })
      }
      return { status: 204 }
    }) as never)
    const off = host.deactivate('b1')
    expect(await host.activate('b1')).toEqual({ ok: true })
    release()
    expect(await off).toEqual({ ok: false, reason: 'binding_inactive' })
    expect(daemon.sent.filter((r) => r.method === 'DELETE')).toHaveLength(1)
    expect(host.isActive('b1')).toBe(true)
  })

  it('stops retrying a stale activation once the binding is dropped', async () => {
    const host = makeHost()
    host.start()
    let calls = 0
    daemon.respondWith((req) => {
      if (req.method === 'PUT' && ++calls === 1) {
        void host.deactivate('b1')
        return { status: 409, code: 'stale_sequence' }
      }
      return { status: 204 }
    })
    expect(await host.activate('b1')).toEqual({ ok: false, reason: 'binding_inactive' })
    expect(daemon.sent.filter((r) => r.method === 'PUT')).toHaveLength(1)
  })

  it('refuses a malformed binding id and a push for a binding it did not activate, sending nothing', async () => {
    const host = makeHost()
    host.start()
    expect(await host.activate('../x')).toEqual({ ok: false, reason: 'invalid_binding' })
    expect(await host.pushCredential('b1', credential('c'))).toEqual({ ok: false, reason: 'binding_inactive' })
    expect(daemon.sent).toEqual([])
  })

  it('sends a stale change again under a new number, up to three times', async () => {
    const host = makeHost()
    host.start()
    let stale = 2
    daemon.respondWith(() => (stale-- > 0 ? { status: 409, code: 'stale_sequence' } : { status: 204 }))
    expect(await host.activate('b1')).toEqual({ ok: true })
    const seqs = daemon.sent.map((r) => r.seq)
    expect(seqs).toHaveLength(3)
    expect(seqs[0]! < seqs[1]! && seqs[1]! < seqs[2]!).toBe(true)

    daemon.respondWith(() => ({ status: 409, code: 'stale_sequence' }))
    expect(await host.activate('b2')).toEqual({ ok: false, reason: 'stale_sequence' })
    expect(daemon.sent).toHaveLength(6)
  })

  it('tells the daemon’s refusals apart', async () => {
    const host = makeHost()
    host.start()
    await host.activate('b1')
    daemon.respondWith(() => ({ status: 409, code: 'binding_inactive' }))
    expect(await host.pushCredential('b1', credential('c'))).toEqual({ ok: false, reason: 'binding_inactive' })
    daemon.respondWith(() => ({ status: 503, code: 'knowledge_host_unavailable' }))
    expect(await host.activate('b1')).toEqual({ ok: false, reason: 'daemon_unavailable' })
    daemon.respondWith(() => ({ status: 400, code: 'invalid_request' }))
    expect(await host.activate('b1')).toEqual({ ok: false, reason: 'refused' })
    daemon.setConnection(null)
    expect(await host.activate('b1')).toEqual({ ok: false, reason: 'daemon_unavailable' })
  })

  it('replays the wanted bindings on a new daemon connection, before anything else for them', async () => {
    const host = makeHost()
    host.start()
    await host.activate('b1')
    await host.pushCredential('b1', credential('C1'))
    await host.activate('b2')
    await host.pushCredential('b2', credential('OLD', NOW)) // expired by now
    await host.activate('b3')
    await host.deactivate('b3')
    daemon.sent.length = 0

    daemon.setConnection({ port: 2, authHeaders: { Authorization: 'Bearer t2' } })
    await host.revokeCredential('b1')
    await host.activate('b2')
    const b1 = daemon.sent.filter((r) => r.path.startsWith(B1))
    expect(b1.map((r) => `${r.method} ${r.path}`)).toEqual([`PUT ${B1}`, `PUT ${B1}/credential`, `DELETE ${B1}/credential`])
    expect(JSON.parse(b1[1]!.body!).value).toBe('C1')
    // b2's expired C is not replayed; b3 was deactivated, so it is not replayed at all.
    expect(daemon.sent.filter((r) => r.path.includes('/b2')).map((r) => `${r.method} ${r.path}`)).toEqual([
      'PUT /api/v1/host/knowledge/bindings/b2',
      'PUT /api/v1/host/knowledge/bindings/b2',
    ])
    expect(daemon.sent.some((r) => r.path.includes('/b3'))).toBe(false)
    expect(daemon.sent.every((r) => r.token === 'Bearer t2')).toBe(true)

    // The same connection again: nothing is replayed.
    host.sync()
    await host.revokeCredential('b1')
    expect(daemon.sent.filter((r) => r.path === B1)).toHaveLength(1)
  })

  it('does not hand a new daemon a C that was revoked or dropped by a reactivation', async () => {
    const host = makeHost()
    host.start()
    for (const id of ['b1', 'b2']) {
      await host.activate(id)
      await host.pushCredential(id, credential(`C-${id}`))
    }
    await host.revokeCredential('b1')
    await host.activate('b2')
    daemon.sent.length = 0
    daemon.setConnection({ port: 2, authHeaders: { Authorization: 'Bearer t2' } })
    host.sync()
    await vi.waitFor(() => expect(daemon.sent).toHaveLength(2))
    await new Promise((resolve) => setTimeout(resolve, 10))
    expect(daemon.sent.map((r) => r.method + ' ' + r.path.split('/').slice(-1)[0])).toEqual(['PUT b1', 'PUT b2'])
  })

  it('tries a replay again on the next sync() until it lands, one at a time', async () => {
    const host = makeHost()
    host.start()
    await host.activate('b1')
    await host.pushCredential('b1', credential('C1'))
    daemon.sent.length = 0
    daemon.setConnection({ port: 2, authHeaders: { Authorization: 'Bearer t2' } })
    daemon.respondWith(() => ({ status: 503, code: 'knowledge_host_unavailable' }))
    host.sync()
    host.sync()
    await vi.waitFor(() => expect(daemon.sent).toHaveLength(1))
    await new Promise((resolve) => setTimeout(resolve, 10))
    expect(daemon.sent).toHaveLength(1)

    daemon.respondWith(() => ({ status: 204 }))
    host.sync()
    await vi.waitFor(() => expect(daemon.sent).toHaveLength(3))
    expect(daemon.sent.slice(1).map((r) => `${r.method} ${r.path}`)).toEqual([`PUT ${B1}`, `PUT ${B1}/credential`])
    host.sync()
    await new Promise((resolve) => setTimeout(resolve, 10))
    expect(daemon.sent).toHaveLength(3)
  })

  it('gives up a reconcile the daemon refuses instead of asking again on every sync()', async () => {
    const host = makeHost()
    host.start()
    await host.activate('b1')
    daemon.sent.length = 0
    daemon.setConnection({ port: 2, authHeaders: { Authorization: 'Bearer t2' } })
    daemon.respondWith(() => ({ status: 409, code: 'too_many_bindings' }))
    host.sync()
    await vi.waitFor(() => expect(daemon.sent).toHaveLength(1))
    host.sync()
    host.sync()
    await new Promise((resolve) => setTimeout(resolve, 10))
    expect(daemon.sent).toHaveLength(1)
  })

  it('activates again and re-pushes when the daemon says the binding is not active', async () => {
    const host = makeHost()
    host.start()
    await host.activate('b1')
    daemon.respondWith(() => ({ status: 409, code: 'binding_inactive' }))
    expect(await host.pushCredential('b1', credential('C1'))).toEqual({ ok: false, reason: 'binding_inactive' })
    daemon.sent.length = 0
    daemon.respondWith(() => ({ status: 204 }))
    host.sync()
    await vi.waitFor(() => expect(daemon.sent.map((r) => `${r.method} ${r.path}`)).toEqual([`PUT ${B1}`, `PUT ${B1}/credential`]))
  })

  it('reconciles a change that did not land for a passing reason, and not one the daemon refused', async () => {
    const host = makeHost()
    host.start()
    daemon.respondWith(() => ({ status: 400, code: 'invalid_request' }))
    expect(await host.activate('b2')).toEqual({ ok: false, reason: 'refused' })
    daemon.respondWith(() => ({ status: 503 }))
    expect(await host.activate('b1')).toEqual({ ok: false, reason: 'daemon_unavailable' })
    daemon.sent.length = 0
    daemon.respondWith(() => ({ status: 204 }))
    host.sync()
    await vi.waitFor(() => expect(daemon.sent.map((r) => `${r.method} ${r.path}`)).toEqual([`PUT ${B1}`]))
    await new Promise((resolve) => setTimeout(resolve, 10))
    expect(daemon.sent).toHaveLength(1)
  })

  it('replays on sync() alone, without waiting for a change', async () => {
    const host = makeHost()
    host.start()
    await host.activate('b1')
    daemon.sent.length = 0
    daemon.setConnection({ port: 2, authHeaders: { Authorization: 'Bearer t2' } })
    host.sync()
    await vi.waitFor(() => expect(daemon.sent.map((r) => `${r.method} ${r.path}`)).toEqual([`PUT ${B1}`]))
  })

  it('pushes a C obtained from the login, and never for an unusable or missing login', async () => {
    const host = makeHost()
    host.start()
    await store.put('b1', login('REFRESH'))
    await host.activate('b1')
    const obtain = vi.fn(async (l: Readonly<KnowledgeLogin>) => credential(`from-${l.material}`))
    expect(await host.refresh('b1', obtain)).toEqual({ ok: true })
    expect(JSON.parse(daemon.sent.at(-1)!.body!).value).toBe('from-REFRESH')

    expect(await host.refresh('b2', obtain)).toEqual({ ok: false, reason: 'login_unavailable' })
    await store.markUnusable('b1')
    expect(await host.refresh('b1', obtain)).toEqual({ ok: false, reason: 'login_unavailable' })
    expect(obtain).toHaveBeenCalledTimes(1)
    await store.put('b3', login('R3'))
    expect(await host.refresh('b3', async () => Promise.reject(new Error('SECRET')))).toEqual({
      ok: false,
      reason: 'credential_unavailable',
    })
  })

  it('never runs a destroy alongside a refresh of the same binding', async () => {
    const host = makeHost()
    host.start()
    await store.put('b1', login('REFRESH'))
    await host.activate('b1')
    let release: (c: KnowledgeCredential) => void = () => {}
    const refreshing = host.refresh('b1', () => new Promise((resolve) => (release = resolve)))
    const destroying = host.destroy('b1')
    await new Promise((resolve) => setTimeout(resolve, 10))
    // The destroy waits for the refresh: the login is still usable, nothing removed.
    expect(store.list().get('b1')?.unusable).toBe(false)
    release(credential('NEW'))
    expect(await refreshing).toEqual({ ok: true })
    expect(await destroying).toEqual({ ok: true })
    expect(daemon.sent.slice(-2).map((r) => `${r.method} ${r.path}`)).toEqual([`PUT ${B1}/credential`, `DELETE ${B1}`])
    expect(store.list().has('b1')).toBe(false)
  })

  it('destroys a login: unusable first, binding and C dropped in the daemon, login removed, never replayed', async () => {
    const host = makeHost()
    host.start()
    await store.put('b1', login('REFRESH'))
    await host.activate('b1')
    await host.pushCredential('b1', credential('C1'))
    const order: string[] = []
    const mark = vi.spyOn(store, 'markUnusable').mockImplementation(async () => {
      order.push('mark')
    })
    const remove = vi.spyOn(store, 'remove').mockImplementation(async () => {
      order.push('remove')
    })
    daemon.respondWith((req) => {
      order.push(`${req.method} ${req.path}`)
      return { status: 204 }
    })
    expect(await host.destroy('b1')).toEqual({ ok: true })
    expect(order).toEqual(['mark', `DELETE ${B1}`, 'remove'])
    expect(mark).toHaveBeenCalledWith('b1')
    expect(remove).toHaveBeenCalledWith('b1')

    daemon.setConnection({ port: 2, authHeaders: { Authorization: 'Bearer t2' } })
    host.sync()
    await new Promise((resolve) => setTimeout(resolve, 10))
    expect(order).toHaveLength(3)
  })

  it('reports a destroy whose daemon drop failed, and drops the binding on a later sync()', async () => {
    const host = makeHost()
    host.start()
    await store.put('b1', login('REFRESH'))
    await host.activate('b1')
    await host.pushCredential('b1', credential('C1'))
    daemon.respondWith(() => ({ status: 503, code: 'knowledge_host_unavailable' }))
    expect(await host.destroy('b1')).toEqual({ ok: false, reason: 'daemon_unavailable' })
    // The login is gone all the same; only the daemon is behind.
    expect(store.list().has('b1')).toBe(false)
    daemon.sent.length = 0
    daemon.respondWith(() => ({ status: 204 }))
    host.sync()
    await vi.waitFor(() => expect(daemon.sent.map((r) => `${r.method} ${r.path}`)).toEqual([`DELETE ${B1}`]))
    host.sync()
    await new Promise((resolve) => setTimeout(resolve, 10))
    expect(daemon.sent).toHaveLength(1)
  })

  it('does not report a destroy as done when the unusable mark or the removal is not saved', async () => {
    const host = makeHost()
    host.start()
    await store.put('b1', login('REFRESH'))
    await host.activate('b1')
    const mark = vi.spyOn(store, 'markUnusable').mockRejectedValueOnce(new Error('disk full'))
    const remove = vi.spyOn(store, 'remove')
    expect(await host.destroy('b1')).toEqual({ ok: false, reason: 'not_saved' })
    // The daemon still dropped the binding; the login was not removed.
    expect(daemon.sent.at(-1)!.method).toBe('DELETE')
    expect(remove).not.toHaveBeenCalled()

    mark.mockRestore()
    remove.mockRejectedValueOnce(new Error('disk full'))
    expect(await host.destroy('b1')).toEqual({ ok: false, reason: 'not_saved' })
  })

  it('talks to the daemon over loopback HTTP with main’s credential, reading only the error code', async () => {
    const seen: { method?: string; url?: string; auth?: string; type?: string; body: string }[] = []
    const server = http.createServer((req, res) => {
      let body = ''
      req.on('data', (chunk: Buffer) => (body += chunk.toString()))
      req.on('end', () => {
        seen.push({ method: req.method, url: req.url, auth: req.headers.authorization, type: req.headers['content-type'], body })
        if (req.url?.includes('/credential')) {
          res.writeHead(409, { 'Content-Type': 'application/json' })
          res.end(JSON.stringify({ error: { code: 'binding_inactive', message: 'The binding is not active' } }))
        } else {
          res.writeHead(204)
          res.end()
        }
      })
    })
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
    const port = (server.address() as { port: number }).port
    try {
      const host = new KnowledgeHost(store, {
        connection: () => ({ port, authHeaders: { Authorization: 'Bearer HOST' } }),
        clock: new SequenceClock(() => 7),
      })
      host.start()
      expect(await host.activate('b 1')).toEqual({ ok: true })
      expect(await host.pushCredential('b 1', credential('C1'))).toEqual({ ok: false, reason: 'binding_inactive' })
      expect(seen[0]).toMatchObject({ method: 'PUT', url: '/api/v1/host/knowledge/bindings/b%201', auth: 'Bearer HOST', type: 'application/json', body: JSON.stringify({ seq: 7 }) })
      expect(seen[1]).toMatchObject({ url: '/api/v1/host/knowledge/bindings/b%201/credential', type: 'application/json' })
      expect(JSON.parse(seen[1]!.body)).toEqual({ seq: 8, kind: 'bearer', value: 'C1', expires_at: NOW + 600 })
    } finally {
      await new Promise((resolve) => server.close(resolve))
    }
    // A response cut short does not hang the binding's queue.
    const cut = http.createServer((_req, res) => {
      res.writeHead(409, { 'Content-Type': 'application/json' })
      res.write('{"error":{"co')
      setTimeout(() => res.socket?.destroy(), 20)
    })
    await new Promise<void>((resolve) => cut.listen(0, '127.0.0.1', resolve))
    const cutPort = (cut.address() as { port: number }).port
    try {
      const host = new KnowledgeHost(store, { connection: () => ({ port: cutPort, authHeaders: {} }) })
      host.start()
      expect(await host.activate('b1')).toEqual({ ok: false, reason: 'daemon_unavailable' })
      expect(await host.deactivate('b1')).toEqual({ ok: false, reason: 'daemon_unavailable' })
    } finally {
      cut.closeAllConnections()
      await new Promise((resolve) => cut.close(resolve))
    }
    // Nothing listens there any more.
    const gone = new KnowledgeHost(store, { connection: () => ({ port, authHeaders: {} }) })
    gone.start()
    expect(await gone.activate('b1')).toEqual({ ok: false, reason: 'daemon_unavailable' })
  })

  it('carries no credential or login material in any result', async () => {
    const host = makeHost()
    host.start()
    await store.put('b1', login('REFRESH-SECRET'))
    await host.activate('b1')
    const results = [
      await host.pushCredential('b1', credential('C-SECRET')),
      await host.refresh('b1', async () => Promise.reject(new Error('C-SECRET'))),
    ]
    daemon.respondWith(() => ({ status: 400, code: 'invalid_request' }))
    results.push(await host.pushCredential('b1', credential('C-SECRET')))
    daemon.setConnection(null)
    results.push(await host.refresh('b1', async () => credential('C-SECRET')))
    expect(JSON.stringify(results)).not.toMatch(/SECRET/)
  })
})

// M5 B6: in service mode every binding change is Elevated (K-B6). The host
// sends through B3's sendElevating: a 403 presence_required is confirmed once
// on the enrolment's turn and retried with the grant; with no enrolment it is
// one plain request (the loopback test above).
describe('KnowledgeHost through elevation (M5 B6)', () => {
  const GRANT = 'grant-one-shot'
  let dir: string
  let store: KnowledgeLoginStore

  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kh-elevate-'))
    store = new KnowledgeLoginStore(dir, box, 'darwin')
  })

  afterEach(() => {
    fs.rmSync(dir, { recursive: true, force: true })
  })

  /** A service-mode daemon: every binding change without a grant is 403
   * presence_required; with the grant it is applied (204). */
  async function serviceDaemon() {
    const seen: { method?: string; url?: string; auth?: string; grant?: string; body: string }[] = []
    const server = http.createServer((req, res) => {
      let body = ''
      req.on('data', (chunk: Buffer) => (body += chunk.toString()))
      req.on('end', () => {
        const grant = req.headers['x-agent24-elevation']
        seen.push({ method: req.method, url: req.url, auth: req.headers.authorization, grant: typeof grant === 'string' ? grant : undefined, body })
        if (grant === GRANT) {
          res.writeHead(204)
          res.end()
          return
        }
        res.writeHead(403, { 'Content-Type': 'application/json' })
        res.end(JSON.stringify({ error: { code: 'presence_required', message: 'Presence confirmation required' } }))
      })
    })
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
    const port = (server.address() as { port: number }).port
    return { seen, port, close: () => new Promise((resolve) => server.close(resolve)) }
  }

  /** The enrolment's turn: records each elevation, answers with the grant or
   * with `fail`. The token is the one the connection carries. */
  function enrolment(fail?: unknown) {
    const asked: ElevateRequest[] = []
    const turn = async <T>(fn: (elevate: (request: ElevateRequest) => Promise<string>, token: string) => Promise<T>): Promise<T> =>
      fn(async (request) => {
        asked.push(request)
        if (fail !== undefined) throw fail
        return GRANT
      }, 'DAILY')
    return { asked, turn }
  }

  it('confirms each of the four changes once and retries it with the grant, the bytes it was granted for', async () => {
    const daemon = await serviceDaemon()
    const { asked, turn } = enrolment()
    try {
      const host = new KnowledgeHost(store, {
        connection: () => ({ port: daemon.port, authHeaders: { Authorization: 'Bearer DAILY' } }),
        elevation: () => turn,
        clock: new SequenceClock(() => 7),
      })
      host.start()
      const endpoint = { endpoint: 'https://kb.example/api/v1', knowledgeBaseIds: ['kb-a'] }
      expect(await host.activate('b1', endpoint)).toEqual({ ok: true })
      expect(await host.pushCredential('b1', credential('C-SECRET'))).toEqual({ ok: true })
      expect(await host.revokeCredential('b1')).toEqual({ ok: true })
      expect(await host.deactivate('b1')).toEqual({ ok: true })
      const changes = [
        ['PUT', '/api/v1/host/knowledge/bindings/b1'],
        ['PUT', '/api/v1/host/knowledge/bindings/b1/credential'],
        ['DELETE', '/api/v1/host/knowledge/bindings/b1/credential'],
        ['DELETE', '/api/v1/host/knowledge/bindings/b1'],
      ]
      expect(asked.map((a) => [a.method, a.path])).toEqual(changes)
      // Per change: first try, again in the turn, then the retry with the grant.
      expect(daemon.seen).toHaveLength(12)
      for (const [i, [method, url]] of changes.entries()) {
        const tries = daemon.seen.slice(3 * i, 3 * i + 3)
        expect(tries.map((t) => [t.method, t.url, t.grant])).toEqual([
          [method, url, undefined],
          [method, url, undefined],
          [method, url, GRANT],
        ])
        // The grant binds the body: the elevation names exactly what is sent.
        expect(tries.every((t) => t.body === asked[i]!.body)).toBe(true)
        expect(tries.every((t) => t.auth === 'Bearer DAILY')).toBe(true)
      }
      expect(JSON.parse(asked[0]!.body)).toEqual({ seq: 7, endpoint: 'https://kb.example/api/v1', knowledge_base_ids: ['kb-a'] })
      // The daemon writes the approval text; the fallback main sends never carries C.
      expect(asked.map((a) => a.summary).join(' ')).not.toMatch(/SECRET/)
    } finally {
      await daemon.close()
    }
  })

  it('reports a change the user did not confirm as refused, and does not ask again on sync()', async () => {
    const daemon = await serviceDaemon()
    const { asked, turn } = enrolment(new EnrolError('presence_denied', 'denied'))
    try {
      const host = new KnowledgeHost(store, {
        connection: () => ({ port: daemon.port, authHeaders: { Authorization: 'Bearer DAILY' } }),
        elevation: () => turn,
      })
      host.start()
      expect(await host.activate('b1')).toEqual({ ok: false, reason: 'refused' })
      expect(host.isActive('b1')).toBe(false)
      const before = daemon.seen.length
      host.sync()
      await new Promise((resolve) => setTimeout(resolve, 20))
      expect(asked).toHaveLength(1)
      expect(daemon.seen).toHaveLength(before)
      expect(daemon.seen.some((t) => t.grant !== undefined)).toBe(false)
    } finally {
      await daemon.close()
    }
  })

  it('maps only proxyRequest’s own failures to daemon_unavailable, and a daemon answer to its code', async () => {
    const conn = () => ({ port: 1, authHeaders: { Authorization: 'Bearer HOST' } })
    const own = [
      proxyError('timeout', 'x'),
      proxyError('proxy_failed', 'x'),
      proxyError('payload_too_large', 'x'),
      proxyError('backend_not_ready', 'x'),
    ]
    for (const reply of own) {
      const transport = elevatingTransport(conn, () => null, async () => reply)
      await expect(transport(conn(), 'PUT', '/api/v1/host/knowledge/bindings/b1', '{"seq":1}')).rejects.toThrow('Knowledge host request failed')
    }
    const daemonAnswers: [BackendProxyResponse, { status: number; code?: string }][] = [
      [{ ok: true, status: 204, data: null }, { status: 204, code: undefined }],
      [{ ok: false, status: 409, data: { error: { code: 'stale_sequence', message: 'm' } } }, { status: 409, code: 'stale_sequence' }],
      [{ ok: false, status: 503, data: { error: { code: 'knowledge_host_unavailable', message: 'm' } } }, { status: 503, code: 'knowledge_host_unavailable' }],
      [{ ok: false, status: 413, data: { error: { code: 'payload_too_large', message: 'm' } } }, { status: 413, code: 'payload_too_large' }],
      [{ ok: false, status: 400, data: { error: { code: 7 } } }, { status: 400, code: undefined }],
    ]
    for (const [reply, mapped] of daemonAnswers) {
      const transport = elevatingTransport(conn, () => null, async () => reply)
      await expect(transport(conn(), 'PUT', '/api/v1/host/knowledge/bindings/b1', '{"seq":1}')).resolves.toEqual(mapped)
    }
  })

  it('sends the body it was given, parsed once, and nothing of it in a failure', async () => {
    const conn = () => ({ port: 1, authHeaders: {} })
    const got: BackendProxyRequest[] = []
    const transport = elevatingTransport(conn, () => null, async (_endpoint, req) => {
      got.push(req)
      return proxyError('proxy_failed', 'x')
    })
    const body = JSON.stringify({ seq: 3, kind: 'bearer', value: 'C-SECRET', expires_at: NOW })
    const error = await transport(conn(), 'PUT', '/api/v1/host/knowledge/bindings/b1/credential', body).catch((e: Error) => e)
    expect(JSON.stringify(got[0]!.body)).toBe(body)
    expect(String(error)).not.toMatch(/SECRET/)
  })
})

// (c2) F2 and F3 (ruling 2026-11-13-c-fix-f1-f3): which daemon the host talks
// to is told by the enrolled service daemon's pid:generation, by value, so a
// renewal replays nothing; and what a confirmation that did not happen means.
describe('KnowledgeHost: daemon identity and confirmation outcomes (M5 B6 c2)', () => {
  let dir = ''
  let store: KnowledgeLoginStore
  let daemon: ReturnType<typeof fakeDaemon>
  let identity: string | null
  let nowMs = 1_000_000

  const make = () => new KnowledgeHost(store, {
    connection: daemon.connection,
    transport: daemon.transport,
    daemonIdentity: () => identity,
    nowMs: () => nowMs,
    nowSeconds: () => NOW,
  })
  const lines = () => daemon.sent.map((r) => `${r.method} ${r.path}`)
  const settle = () => new Promise((resolve) => setTimeout(resolve, 20))
  const MIN = 60_000

  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kb-host-c2-'))
    store = new KnowledgeLoginStore(dir, box, 'darwin')
    daemon = fakeDaemon()
    identity = '100:g1'
    nowMs = 1_000_000
  })
  afterEach(() => {
    fs.rmSync(dir, { recursive: true, force: true })
  })

  async function activeWithC(host: KnowledgeHost) {
    host.start()
    expect(await host.activate('b1')).toEqual({ ok: true })
    expect(await host.pushCredential('b1', credential('C1'))).toEqual({ ok: true })
    daemon.sent.length = 0
  }

  it('replays nothing on a renewal: a new token from the same service daemon', async () => {
    const host = make()
    await activeWithC(host)
    daemon.setConnection({ port: 1, authHeaders: { Authorization: 'Bearer renewed' } })
    host.sync()
    await settle()
    expect(lines()).toEqual([])
    expect(host.isActive('b1')).toBe(true)
  })

  it('replays nothing when the token connection ends and enrols again with the same daemon', async () => {
    const host = make()
    await activeWithC(host)
    daemon.setConnection(null)
    host.sync()
    daemon.setConnection({ port: 1, authHeaders: { Authorization: 'Bearer enrolled-again' } })
    host.sync()
    await settle()
    expect(lines()).toEqual([])
    expect(host.isActive('b1')).toBe(true)
  })

  it('replays on a restarted service daemon (another generation, or another pid)', async () => {
    for (const next of ['100:g2', '101:g1']) {
      daemon = fakeDaemon()
      identity = '100:g1'
      const host = make()
      await activeWithC(host)
      identity = next
      daemon.setConnection({ port: 9, authHeaders: { Authorization: 'Bearer new-daemon' } })
      host.sync()
      await vi.waitFor(() => expect(lines()).toEqual([`PUT ${B1}`, `PUT ${B1}/credential`]))
    }
  })

  it('replays when the service daemon gives way to our own child, and back', async () => {
    const host = make()
    await activeWithC(host)
    // Our own child: no enrolment, so the connection itself is the identity.
    identity = null
    daemon.setConnection({ port: 2, authHeaders: { Authorization: 'Bearer host-token' } })
    host.sync()
    await vi.waitFor(() => expect(lines()).toEqual([`PUT ${B1}`, `PUT ${B1}/credential`]))
    daemon.sent.length = 0
    // And the service daemon again.
    identity = '200:g9'
    daemon.setConnection({ port: 3, authHeaders: { Authorization: 'Bearer daily' } })
    host.sync()
    await vi.waitFor(() => expect(lines()).toEqual([`PUT ${B1}`, `PUT ${B1}/credential`]))
  })

  it('without an enrolment, keeps telling daemons apart by the connection, as before', async () => {
    identity = null
    const host = make()
    await activeWithC(host)
    daemon.setConnection({ port: 1, authHeaders: { Authorization: 'Bearer t2' } })
    host.sync()
    await vi.waitFor(() => expect(lines()).toEqual([`PUT ${B1}`, `PUT ${B1}/credential`]))
  })

  it('tells a refusal, an unanswered confirmation and one that could not be asked apart', async () => {
    const outcomes: [string, string][] = [
      ['presence_denied', 'refused'],
      ['presence_timeout', 'unanswered'],
      ['presence_unavailable', 'daemon_unavailable'],
      ['closed', 'daemon_unavailable'],
      ['busy', 'daemon_unavailable'],
      ['timeout', 'daemon_unavailable'],
      ['daemon_unavailable', 'daemon_unavailable'],
      ['bad_frame', 'daemon_unavailable'],
      // Any other 403 is still the daemon's refusal.
      ['host_authority_required', 'refused'],
      ['endpoint_mismatch', 'refused'],
    ]
    for (const [code, reason] of outcomes) {
      daemon = fakeDaemon()
      const host = make()
      host.start()
      daemon.respondWith(() => ({ status: 403, code }))
      expect(await host.activate('b1'), code).toEqual({ ok: false, reason })
    }
  })

  it('never asks again on its own after the user refused, however late; the user changing it asks again', async () => {
    const host = make()
    host.start()
    daemon.respondWith(() => ({ status: 403, code: 'presence_denied' }))
    expect(await host.activate('b1')).toEqual({ ok: false, reason: 'refused' })
    for (const later of [0, 4_000, 2 * 3600_000, 365 * 24 * 3600_000]) {
      nowMs += later
      host.sync()
      await settle()
    }
    expect(lines()).toEqual([`PUT ${B1}`])
    daemon.respondWith(() => ({ status: 204 }))
    expect(await host.activate('b1')).toEqual({ ok: true })
    expect(lines()).toEqual([`PUT ${B1}`, `PUT ${B1}`])
  })

  it('backs off an unanswered confirmation: 10 min, doubling, at most 1 h', () => {
    expect([1, 2, 3, 4, 5, 9].map(unansweredRetryDelayMs)).toEqual([10 * MIN, 20 * MIN, 40 * MIN, 60 * MIN, 60 * MIN, 60 * MIN])
  })

  it('replays an unanswered activation only when its backoff is over, never on the 4 s sync, and backs off further', async () => {
    const host = make()
    host.start()
    daemon.respondWith(() => ({ status: 403, code: 'presence_timeout' }))
    expect(await host.activate('b1')).toEqual({ ok: false, reason: 'unanswered' })
    let asked = 1
    for (const step of [10, 20, 40, 60, 60]) {
      nowMs += step * MIN - 1
      for (let i = 0; i < 3; i++) host.sync()
      await settle()
      expect(daemon.sent.length, `before ${step} min`).toBe(asked)
      nowMs += 1
      host.sync()
      await vi.waitFor(() => expect(daemon.sent.length).toBe(asked + 1))
      asked += 1
      await settle()
    }
    expect(new Set(lines())).toEqual(new Set([`PUT ${B1}`]))
    // Answered at last: active, and nothing more is asked.
    daemon.respondWith(() => ({ status: 204 }))
    nowMs += 60 * MIN
    host.sync()
    await vi.waitFor(() => expect(host.isActive('b1')).toBe(true))
    nowMs += 24 * 3600_000
    host.sync()
    await settle()
    expect(daemon.sent.length).toBe(asked + 1)
  })

  it('starts the backoff again from 10 min after a success, the user changing the binding, or another daemon', async () => {
    const timeoutThenAsk = async (host: KnowledgeHost) => {
      daemon.respondWith(() => ({ status: 403, code: 'presence_timeout' }))
      // Three unanswered in a row: the next wait would be 40 min.
      expect(await host.activate('b1')).toEqual({ ok: false, reason: 'unanswered' })
      for (const step of [10, 20]) {
        nowMs += step * MIN
        host.sync()
        const before = daemon.sent.length
        await vi.waitFor(() => expect(daemon.sent.length).toBe(before + 1))
        await settle()
      }
    }
    const waitsTenMinutes = async (host: KnowledgeHost) => {
      const before = daemon.sent.length
      nowMs += 10 * MIN - 1
      host.sync()
      await settle()
      expect(daemon.sent.length).toBe(before)
      nowMs += 1
      host.sync()
      await vi.waitFor(() => expect(daemon.sent.length).toBe(before + 1))
      await settle()
    }

    // The user changes the binding: its own activation is asked at once, and a
    // timeout of that starts from the first step.
    let host = make()
    host.start()
    await timeoutThenAsk(host)
    expect(await host.activate('b1')).toEqual({ ok: false, reason: 'unanswered' })
    await waitsTenMinutes(host)

    // Another daemon: everything is replayed at once, the backoff from the first step.
    daemon = fakeDaemon()
    host = make()
    host.start()
    await timeoutThenAsk(host)
    identity = '100:g2'
    daemon.setConnection({ port: 9, authHeaders: { Authorization: 'Bearer new-daemon' } })
    const before = daemon.sent.length
    host.sync()
    await vi.waitFor(() => expect(daemon.sent.length).toBe(before + 1))
    await settle()
    await waitsTenMinutes(host)

    // A success clears it: the next timeout starts from the first step.
    daemon = fakeDaemon()
    identity = '100:g1'
    host = make()
    host.start()
    await timeoutThenAsk(host)
    daemon.respondWith(() => ({ status: 204 }))
    nowMs += 40 * MIN
    host.sync()
    await vi.waitFor(() => expect(host.isActive('b1')).toBe(true))
    daemon.respondWith(() => ({ status: 403, code: 'presence_timeout' }))
    expect(await host.pushCredential('b1', credential('C2'))).toEqual({ ok: false, reason: 'unanswered' })
    await waitsTenMinutes(host)
  })

  it("the user's own change of a binding waiting out its backoff is asked once, and the next sync asks nothing more", async () => {
    const host = make()
    host.start()
    daemon.respondWith(() => ({ status: 403, code: 'presence_timeout' }))
    expect(await host.activate('b1')).toEqual({ ok: false, reason: 'unanswered' })
    daemon.respondWith(() => ({ status: 204 }))
    expect(await host.activate('b1')).toEqual({ ok: true })
    expect(lines()).toEqual([`PUT ${B1}`, `PUT ${B1}`])
    nowMs += 24 * 3600_000
    host.sync()
    await settle()
    expect(lines()).toEqual([`PUT ${B1}`, `PUT ${B1}`])
    // The same for an Off.
    daemon.respondWith(() => ({ status: 403, code: 'presence_timeout' }))
    expect(await host.deactivate('b1')).toEqual({ ok: false, reason: 'unanswered' })
    daemon.respondWith(() => ({ status: 204 }))
    expect(await host.deactivate('b1')).toEqual({ ok: true })
    nowMs += 24 * 3600_000
    host.sync()
    await settle()
    expect(lines()).toEqual([`PUT ${B1}`, `PUT ${B1}`, `DELETE ${B1}`, `DELETE ${B1}`])
  })

  it('a C push not answered in time comes back with the activation once its backoff is over', async () => {
    const host = make()
    await activeWithC(host)
    daemon.respondWith(() => ({ status: 403, code: 'presence_timeout' }))
    expect(await host.pushCredential('b1', credential('C2'))).toEqual({ ok: false, reason: 'unanswered' })
    daemon.sent.length = 0
    daemon.respondWith(() => ({ status: 204 }))
    host.sync()
    await settle()
    expect(lines()).toEqual([])
    nowMs += 10 * MIN
    host.sync()
    await vi.waitFor(() => expect(lines()).toEqual([`PUT ${B1}`, `PUT ${B1}/credential`]))
    expect(JSON.parse(daemon.sent[1].body ?? '{}').value).toBe('C2')
  })

  it('a refused binding is not asked again by a replay on another daemon; the user activating it asks again (ruling c2-refused-scope)', async () => {
    const host = make()
    host.start()
    daemon.respondWith(() => ({ status: 403, code: 'presence_denied' }))
    expect(await host.activate('b1')).toEqual({ ok: false, reason: 'refused' })
    for (const next of ['100:g2', null]) {
      identity = next
      daemon.setConnection({ port: 9, authHeaders: { Authorization: `Bearer ${next ?? 'host'}` } })
      host.sync()
      await settle()
    }
    expect(lines()).toEqual([`PUT ${B1}`])
    expect(host.isActive('b1')).toBe(false)
    daemon.respondWith(() => ({ status: 204 }))
    expect(await host.activate('b1')).toEqual({ ok: true })
    expect(lines()).toEqual([`PUT ${B1}`, `PUT ${B1}`])
    // Taken again, it is replayed on the next daemon as before.
    identity = '100:g3'
    daemon.setConnection({ port: 10, authHeaders: { Authorization: 'Bearer g3' } })
    host.sync()
    await vi.waitFor(() => expect(lines()).toEqual([`PUT ${B1}`, `PUT ${B1}`, `PUT ${B1}`]))
  })

  it('a refused C push of a binding active before is not replayed on another daemon, and the binding is not active there', async () => {
    const host = make()
    await activeWithC(host)
    daemon.respondWith(() => ({ status: 403, code: 'presence_denied' }))
    expect(await host.pushCredential('b1', credential('C2'))).toEqual({ ok: false, reason: 'refused' })
    // Not in effect at once, though the old C may run for up to an hour more (fail-closed).
    expect(host.isActive('b1')).toBe(false)
    daemon.respondWith(() => ({ status: 204 }))
    identity = '100:g2'
    daemon.setConnection({ port: 9, authHeaders: { Authorization: 'Bearer new-daemon' } })
    host.sync()
    await settle()
    expect(lines()).toEqual([`PUT ${B1}/credential`])
    expect(host.isActive('b1')).toBe(false)
    // The user activating it again: in effect again.
    expect(await host.activate('b1')).toEqual({ ok: true })
    expect(host.isActive('b1')).toBe(true)
  })

  it('a replay refused on one daemon is not asked again on the next (activation, or its C)', async () => {
    for (const refuse of ['activation', 'credential']) {
      daemon = fakeDaemon()
      identity = '100:g1'
      const host = make()
      await activeWithC(host)
      daemon.respondWith((req) => {
        const isC = req.path.endsWith('/credential')
        return (refuse === 'credential') === isC ? { status: 403, code: 'presence_denied' } : { status: 204 }
      })
      identity = '100:g2'
      daemon.setConnection({ port: 9, authHeaders: { Authorization: 'Bearer g2' } })
      host.sync()
      const replay = refuse === 'credential' ? [`PUT ${B1}`, `PUT ${B1}/credential`] : [`PUT ${B1}`]
      await vi.waitFor(() => expect(lines(), refuse).toEqual(replay))
      await settle()
      daemon.respondWith(() => ({ status: 204 }))
      identity = '100:g3'
      daemon.setConnection({ port: 10, authHeaders: { Authorization: 'Bearer g3' } })
      host.sync()
      await settle()
      expect(lines(), refuse).toEqual(replay)
    }
  })

  it('an Off the user gives again after a refused one is retried as before when the daemon is not up', async () => {
    const host = make()
    await activeWithC(host)
    daemon.respondWith(() => ({ status: 403, code: 'presence_denied' }))
    expect(await host.deactivate('b1')).toEqual({ ok: false, reason: 'refused' })
    daemon.respondWith(() => ({ status: 503 }))
    expect(await host.deactivate('b1')).toEqual({ ok: false, reason: 'daemon_unavailable' })
    daemon.respondWith(() => ({ status: 204 }))
    host.sync()
    await vi.waitFor(() => expect(lines()).toEqual([`DELETE ${B1}`, `DELETE ${B1}`, `DELETE ${B1}`]))
  })

  it('a replay queued behind a change the user then refuses is not sent', async () => {
    let release = () => {}
    const asked = new Promise<void>((resolve) => { release = resolve })
    const realTransport = daemon.transport
    let gate = false
    const host = new KnowledgeHost(store, {
      connection: daemon.connection,
      transport: async (conn, method, requestPath, body) => {
        const result = await realTransport(conn, method, requestPath, body)
        if (gate && requestPath.endsWith('/credential')) {
          gate = false
          await asked
          return { status: 403, code: 'presence_denied' }
        }
        return result
      },
      daemonIdentity: () => identity,
      nowMs: () => nowMs,
      nowSeconds: () => NOW,
    })
    await activeWithC(host)
    // A C push waits for its confirmation; meanwhile the daemon changes, and its replay queues behind.
    gate = true
    const push = host.pushCredential('b1', credential('C2'))
    await settle()
    identity = '100:g2'
    daemon.setConnection({ port: 9, authHeaders: { Authorization: 'Bearer g2' } })
    host.sync()
    release()
    expect(await push).toEqual({ ok: false, reason: 'refused' })
    await settle()
    expect(lines()).toEqual([`PUT ${B1}/credential`])
  })

  // A host whose next matching request waits for `answer` before the daemon replies.
  function gatedHost(match: (method: string, requestPath: string) => boolean) {
    let answer: (reply: { status: number; code?: string }) => void = () => {}
    let armed = false
    const realTransport = daemon.transport
    const host = new KnowledgeHost(store, {
      connection: daemon.connection,
      transport: async (conn, method, requestPath, body) => {
        const result = await realTransport(conn, method, requestPath, body)
        if (armed && match(method, requestPath)) {
          armed = false
          return new Promise((resolve) => { answer = resolve })
        }
        return result
      },
      daemonIdentity: () => identity,
      nowMs: () => nowMs,
      nowSeconds: () => NOW,
    })
    return { host, arm: () => { armed = true }, answer: (reply: { status: number; code?: string }) => answer(reply) }
  }

  for (const op of ['activate', 'deactivate'] as const) {
    for (const [code, status] of [['presence_denied', 403], ['presence_timeout', 403], ['', 204]] as const) {
      it(`the user's own ${op} of a binding waiting out a backoff is asked once, though a sync runs while it waits (${code || 'confirmed'})`, async () => {
        const method = op === 'activate' ? 'PUT' : 'DELETE'
        const g = gatedHost((m, requestPath) => m === method && !requestPath.endsWith('/credential'))
        g.host.start()
        if (op === 'deactivate') expect(await g.host.activate('b1')).toEqual({ ok: true })
        // Unanswered once: in unsynced, with a backoff.
        daemon.respondWith(() => ({ status: 403, code: 'presence_timeout' }))
        expect(await g.host.activate('b1')).toEqual({ ok: false, reason: 'unanswered' })
        daemon.respondWith(() => ({ status: 204 }))
        daemon.sent.length = 0
        g.arm()
        const change = op === 'activate' ? g.host.activate('b1') : g.host.deactivate('b1')
        await settle()
        // The 4 s sync while the user's dialog is open.
        g.host.sync()
        await settle()
        g.answer(code ? { status, code } : { status })
        await change
        await settle()
        g.host.sync()
        await settle()
        expect(lines()).toEqual([`${method} ${B1}`])
      })
    }
  }

  it("the user's own Off during a backoff starts it again from 10 min when it goes unanswered too", async () => {
    const host = make()
    await activeWithC(host)
    daemon.respondWith(() => ({ status: 403, code: 'presence_timeout' }))
    expect(await host.activate('b1')).toEqual({ ok: false, reason: 'unanswered' })
    nowMs += 10 * MIN
    host.sync()
    await vi.waitFor(() => expect(daemon.sent.length).toBe(2))
    await settle()
    // Two unanswered: the next wait would be 20 min. The user turns it off, unanswered again.
    expect(await host.deactivate('b1')).toEqual({ ok: false, reason: 'unanswered' })
    const after = daemon.sent.length
    nowMs += 10 * MIN - 1
    host.sync()
    await settle()
    expect(daemon.sent.length).toBe(after)
    nowMs += 1
    host.sync()
    await vi.waitFor(() => expect(lines().slice(after)).toEqual([`DELETE ${B1}`]))
  })

  it("the user's own activate right after a new daemon is asked once: no replay is queued ahead of it", async () => {
    const host = make()
    await activeWithC(host)
    identity = '100:g2'
    daemon.setConnection({ port: 9, authHeaders: { Authorization: 'Bearer g2' } })
    // No sync has noticed the new daemon yet; the user's change is the first call.
    expect(await host.activate('b1')).toEqual({ ok: true })
    await settle()
    host.sync()
    await settle()
    expect(lines()).toEqual([`PUT ${B1}`])
  })

  it('a C push confirmed while a backoff of its replay is pending ends it: nothing more is replayed', async () => {
    const host = make()
    await activeWithC(host)
    daemon.respondWith(() => ({ status: 403, code: 'presence_timeout' }))
    expect(await host.pushCredential('b1', credential('C2'))).toEqual({ ok: false, reason: 'unanswered' })
    daemon.respondWith(() => ({ status: 204 }))
    // The user is back for the next push, before the backoff is over.
    expect(await host.pushCredential('b1', credential('C3'))).toEqual({ ok: true })
    nowMs += 24 * 3600_000
    host.sync()
    await settle()
    expect(lines()).toEqual([`PUT ${B1}/credential`, `PUT ${B1}/credential`])
    expect(host.isActive('b1')).toBe(true)
  })

  for (const code of ['presence_timeout', 'presence_denied']) {
    it(`an Off while a replay waits for its answer is not held back by that replay's outcome (${code})`, async () => {
      const g = gatedHost((m, requestPath) => m === 'PUT' && !requestPath.endsWith('/credential'))
      await activeWithC(g.host)
      identity = '100:g2'
      daemon.setConnection({ port: 9, authHeaders: { Authorization: 'Bearer g2' } })
      g.arm()
      g.host.sync()
      await settle()
      expect(lines()).toEqual([`PUT ${B1}`])
      // The user turns it off while the replay's dialog is open; that Off does not land yet.
      daemon.respondWith(() => ({ status: 503 }))
      expect(await g.host.deactivate('b1')).toEqual({ ok: false, reason: 'daemon_unavailable' })
      g.answer({ status: 403, code })
      await settle()
      // The Off is retried on the next sync, not after a backoff, nor held as refused.
      daemon.respondWith(() => ({ status: 204 }))
      g.host.sync()
      await vi.waitFor(() => expect(lines()).toEqual([`PUT ${B1}`, `DELETE ${B1}`, `DELETE ${B1}`]))
    })
  }

  it("the user's own activate queued behind a push is the replay on a daemon that changes meanwhile", async () => {
    const g = gatedHost((m, requestPath) => requestPath.endsWith('/credential'))
    await activeWithC(g.host)
    g.arm()
    const push = g.host.pushCredential('b1', credential('C2'))
    await settle()
    const change = g.host.activate('b1')
    // The daemon changes while the user's change waits behind the push.
    identity = '100:g2'
    daemon.setConnection({ port: 9, authHeaders: { Authorization: 'Bearer g2' } })
    g.host.sync()
    g.answer({ status: 503 })
    await push
    expect(await change).toEqual({ ok: true })
    await settle()
    g.host.sync()
    await settle()
    expect(lines()).toEqual([`PUT ${B1}/credential`, `PUT ${B1}`])
  })

  it('a C push confirmed during a backoff clears it: a later passing failure is retried on the next sync', async () => {
    const host = make()
    await activeWithC(host)
    daemon.respondWith(() => ({ status: 403, code: 'presence_timeout' }))
    expect(await host.pushCredential('b1', credential('C2'))).toEqual({ ok: false, reason: 'unanswered' })
    daemon.respondWith(() => ({ status: 204 }))
    expect(await host.pushCredential('b1', credential('C3'))).toEqual({ ok: true })
    daemon.respondWith(() => ({ status: 503 }))
    expect(await host.pushCredential('b1', credential('C4'))).toEqual({ ok: false, reason: 'daemon_unavailable' })
    daemon.respondWith(() => ({ status: 204 }))
    const before = daemon.sent.length
    host.sync()
    await vi.waitFor(() => expect(lines().slice(before)).toEqual([`PUT ${B1}`, `PUT ${B1}/credential`]))
  })

  it("an Off while a replay's C waits for its answer is not held back by that answer", async () => {
    const g = gatedHost((m, requestPath) => requestPath.endsWith('/credential'))
    await activeWithC(g.host)
    identity = '100:g2'
    daemon.setConnection({ port: 9, authHeaders: { Authorization: 'Bearer g2' } })
    g.arm()
    g.host.sync()
    await settle()
    expect(lines()).toEqual([`PUT ${B1}`, `PUT ${B1}/credential`])
    daemon.respondWith(() => ({ status: 503 }))
    expect(await g.host.deactivate('b1')).toEqual({ ok: false, reason: 'daemon_unavailable' })
    g.answer({ status: 403, code: 'presence_timeout' })
    await settle()
    daemon.respondWith(() => ({ status: 204 }))
    g.host.sync()
    await vi.waitFor(() => expect(lines()).toEqual([`PUT ${B1}`, `PUT ${B1}/credential`, `DELETE ${B1}`, `DELETE ${B1}`]))
  })

  it('a replay queued behind a change that then goes unanswered waits out the backoff', async () => {
    const g = gatedHost((m, requestPath) => requestPath.endsWith('/credential'))
    await activeWithC(g.host)
    g.arm()
    const push = g.host.pushCredential('b1', credential('C2'))
    await settle()
    identity = '100:g2'
    daemon.setConnection({ port: 9, authHeaders: { Authorization: 'Bearer g2' } })
    g.host.sync()
    g.answer({ status: 403, code: 'presence_timeout' })
    expect(await push).toEqual({ ok: false, reason: 'unanswered' })
    await settle()
    expect(lines()).toEqual([`PUT ${B1}/credential`])
    nowMs += 10 * MIN
    g.host.sync()
    await vi.waitFor(() => expect(lines()).toEqual([`PUT ${B1}/credential`, `PUT ${B1}`, `PUT ${B1}/credential`]))
  })

  it('a binding refused while it waits out a backoff is not asked again when the backoff is over', async () => {
    const host = make()
    host.start()
    daemon.respondWith(() => ({ status: 403, code: 'presence_timeout' }))
    expect(await host.activate('b1')).toEqual({ ok: false, reason: 'unanswered' })
    daemon.respondWith(() => ({ status: 403, code: 'presence_denied' }))
    expect(await host.pushCredential('b1', credential('C2'))).toEqual({ ok: false, reason: 'refused' })
    nowMs += 24 * 3600_000
    host.sync()
    await settle()
    expect(lines()).toEqual([`PUT ${B1}`, `PUT ${B1}/credential`])
  })

  it('a C push not answered in time while the activation was taken comes back with the activation', async () => {
    // reconcile: the activation lands, the C push in it goes unanswered.
    const host = make()
    await activeWithC(host)
    identity = '100:g2'
    daemon.setConnection({ port: 9, authHeaders: { Authorization: 'Bearer new-daemon' } })
    daemon.respondWith((req) => (req.path.endsWith('/credential') ? { status: 403, code: 'presence_timeout' } : { status: 204 }))
    host.sync()
    await vi.waitFor(() => expect(lines()).toEqual([`PUT ${B1}`, `PUT ${B1}/credential`]))
    await settle()
    daemon.respondWith(() => ({ status: 204 }))
    nowMs += 10 * MIN - 1
    host.sync()
    await settle()
    expect(daemon.sent.length).toBe(2)
    nowMs += 1
    host.sync()
    await vi.waitFor(() => expect(lines()).toEqual([`PUT ${B1}`, `PUT ${B1}/credential`, `PUT ${B1}`, `PUT ${B1}/credential`]))
  })

  it('an unanswered reply from the old daemon makes the replay on the new one wait out a first backoff, not a second', async () => {
    for (const late of ['activation', 'credential']) {
      daemon = fakeDaemon()
      identity = '100:g1'
      let gate: Promise<void> | null = null
      let release = () => {}
      const realTransport = daemon.transport
      // The replay on the old daemon waits for its answer while the daemon changes.
      const host = new KnowledgeHost(store, {
        connection: daemon.connection,
        transport: async (conn, method, requestPath, body) => {
          const isC = requestPath.endsWith('/credential')
          if (gate && (late === 'credential') === isC) {
            const wait = gate
            gate = null
            await wait
          }
          return realTransport(conn, method, requestPath, body)
        },
        daemonIdentity: () => identity,
        nowMs: () => nowMs,
        nowSeconds: () => NOW,
      })
      host.start()
      expect(await host.activate('b1')).toEqual({ ok: true })
      daemon.respondWith((req) => (late === 'credential' || !req.path.endsWith('/credential') ? { status: 403, code: 'presence_timeout' } : { status: 204 }))
      if (late === 'credential') {
        expect(await host.pushCredential('b1', credential('C1'))).toEqual({ ok: false, reason: 'unanswered' })
        daemon.respondWith((req) => (req.path.endsWith('/credential') ? { status: 403, code: 'presence_timeout' } : { status: 204 }))
      } else {
        expect(await host.activate('b1')).toEqual({ ok: false, reason: 'unanswered' })
      }
      gate = new Promise<void>((resolve) => { release = resolve })
      nowMs += 10 * MIN
      host.sync()
      await settle()
      identity = '100:g2'
      daemon.setConnection({ port: 9, authHeaders: { Authorization: 'Bearer g2' } })
      host.sync()
      release()
      await settle()
      await settle()
      // Not answered: the user is away, so the new daemon is not asked at once either.
      const after = daemon.sent.length
      nowMs += 10 * MIN - 1
      host.sync()
      await settle()
      expect(daemon.sent.length, late).toBe(after)
      // The backoff the old daemon set is its first step (10 min, not 20).
      nowMs += 1
      host.sync()
      await vi.waitFor(() => expect(daemon.sent.length, late).toBeGreaterThan(after))
    }
  })

  it('tries a change that could not be asked on the next sync, as a daemon not up', async () => {
    const host = make()
    host.start()
    daemon.respondWith(() => ({ status: 403, code: 'presence_unavailable' }))
    expect(await host.activate('b1')).toEqual({ ok: false, reason: 'daemon_unavailable' })
    daemon.respondWith(() => ({ status: 204 }))
    host.sync()
    await vi.waitFor(() => expect(host.isActive('b1')).toBe(true))
    expect(lines()).toEqual([`PUT ${B1}`, `PUT ${B1}`])
  })
})
