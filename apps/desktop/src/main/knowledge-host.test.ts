import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import fs from 'node:fs'
import http from 'node:http'
import os from 'node:os'
import path from 'node:path'
import { KnowledgeHost, SequenceClock, validBindingId, type HostTransport, type KnowledgeCredential } from './knowledge-host'
import { KnowledgeLoginStore, type KnowledgeLogin, type SecretBox } from './knowledge-logins'
import type { DaemonConnection } from './backend-manager'

vi.mock('./backend-manager', () => ({ daemonConnection: () => null }))

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
    const [route, query] = requestPath.split('?')
    const req: Sent = {
      token: connection.authHeaders['Authorization'] ?? '',
      method,
      path: route ?? '',
      seq: Number(new URLSearchParams(query).get('seq')),
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
    expect(JSON.parse(daemon.sent[1]!.body!)).toEqual({ kind: 'api_key', value: 'KEY', expires_at: NOW + 60 })
    expect(daemon.sent.every((r) => r.token === 'Bearer t1')).toBe(true)
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
    daemon.respondWith(() => ({ status: 503 }))
    expect(await host.activate('b1')).toEqual({ ok: false, reason: 'daemon_unavailable' })
    daemon.respondWith(() => ({ status: 400, code: 'invalid_request' }))
    expect(await host.activate('b2')).toEqual({ ok: false, reason: 'refused' })
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
      expect(seen[0]).toMatchObject({ method: 'PUT', url: '/api/v1/host/knowledge/bindings/b%201?seq=7', auth: 'Bearer HOST', body: '' })
      expect(seen[1]).toMatchObject({ url: '/api/v1/host/knowledge/bindings/b%201/credential?seq=8', type: 'application/json' })
      expect(JSON.parse(seen[1]!.body)).toEqual({ kind: 'bearer', value: 'C1', expires_at: NOW + 600 })
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
