// Loop 9 3c: install-then-launch of the local Lite, and the embedding the daemon hands it.
import http from 'node:http'
import type { AddressInfo } from 'node:net'
import { afterEach, describe, expect, it } from 'vitest'
import { LITE_EMBEDDING_PATH, LiteKnowledge, fetchLiteEmbedding } from './knowledge-lite'
import type { WeknoraLiteStatus } from './weknora-lite-component'
import type { WeknoraLiteHostStatus } from './weknora-lite-host'

class FakeInstaller {
  current: WeknoraLiteStatus = { state: 'not-installed', total: 100 }
  installs = 0
  next: WeknoraLiteStatus[] = []
  gate: Promise<void> | null = null
  status() { return { ...this.current } }
  onProgress() { return () => {} }
  async install() {
    this.installs++
    this.current = { state: 'downloading', received: 40, total: 100 }
    if (this.gate) await this.gate
    this.current = this.next.shift() ?? { state: 'installed', dir: '/x' }
    return this.status()
  }
}

class FakeHost {
  current: WeknoraLiteHostStatus = { state: 'stopped', gaveUp: false }
  starts = 0
  stops = 0
  status() { return this.current }
  onStatus() { return () => {} }
  async start() { this.starts++; this.current = { state: 'healthy', gaveUp: false }; return this.current }
  async stop() { this.stops++; this.current = { state: 'stopped', gaveUp: false }; return this.current }
}

describe('LiteKnowledge', () => {
  it('is off until enabled; then installs, launches and reads ready', async () => {
    const installer = new FakeInstaller()
    const host = new FakeHost()
    const lite = new LiteKnowledge({ installer, host })
    expect(lite.status()).toEqual({ phase: 'off' })
    await lite.enable()
    expect([installer.installs, host.starts]).toEqual([1, 1])
    expect(lite.status()).toEqual({ phase: 'ready' })
    // Installed already: a second enable only launches.
    await lite.enable()
    expect([installer.installs, host.starts]).toEqual([1, 2])
  })

  it('shows download progress, and a failed download until retried', async () => {
    const installer = new FakeInstaller()
    const host = new FakeHost()
    const lite = new LiteKnowledge({ installer, host })
    let open = () => {}
    installer.gate = new Promise((resolve) => { open = resolve })
    installer.next = [{ state: 'failed', failure: 'network' }]
    const run = lite.enable()
    await new Promise((resolve) => setImmediate(resolve))
    expect(lite.status()).toEqual({ phase: 'downloading', received: 40, total: 100 })
    // A second click joins the same run.
    expect(lite.enable()).toBe(run)
    open()
    await run
    expect(lite.status()).toEqual({ phase: 'install_failed', failure: 'network' })
    expect(host.starts).toBe(0)
    installer.gate = null
    await lite.enable()
    expect(lite.status()).toEqual({ phase: 'ready' })
    expect(installer.installs).toBe(2)
  })

  it('switching away while it installs does not launch, and stops a running Lite', async () => {
    const installer = new FakeInstaller()
    const host = new FakeHost()
    const lite = new LiteKnowledge({ installer, host })
    let open = () => {}
    installer.gate = new Promise((resolve) => { open = resolve })
    const run = lite.enable()
    await lite.disable()
    open()
    await run
    expect(host.starts).toBe(0)
    expect(lite.status()).toEqual({ phase: 'off' })
    await lite.enable()
    await lite.disable()
    expect(host.stops).toBe(2)
  })

  it('Codex C2: switched away and back while it was starting, it starts again instead of joining the stopped start', async () => {
    const installer = new FakeInstaller()
    installer.current = { state: 'installed', dir: '/x' }
    const host = new FakeHost()
    let open = () => {}
    const gate = new Promise<void>((resolve) => { open = resolve })
    host.start = async () => { host.starts++; await gate; if (host.stops === 0) host.current = { state: 'healthy', gaveUp: false }; return host.current }
    const lite = new LiteKnowledge({ installer, host })
    const first = lite.enable()
    await new Promise((resolve) => setImmediate(resolve))
    expect(host.starts).toBe(1)
    await lite.disable()
    const second = lite.enable()
    expect(second).not.toBe(first)
    open()
    await second
    expect(host.starts).toBe(2)
  })

  it('says when it gave up, and whether the embedding (iDoris) was the reason', async () => {
    const installer = new FakeInstaller()
    installer.current = { state: 'installed', dir: '/x' }
    const host = new FakeHost()
    let problem: string | null = 'idoris_unavailable'
    const lite = new LiteKnowledge({ installer, host, embeddingProblem: () => problem })
    await lite.enable()
    host.current = { state: 'stopped', gaveUp: true }
    expect(lite.status()).toEqual({ phase: 'failed', reason: 'embedding_unavailable' })
    problem = null
    expect(lite.status()).toEqual({ phase: 'failed', reason: 'start_failed' })
    host.current = { state: 'starting', gaveUp: false }
    expect(lite.status()).toEqual({ phase: 'starting' })
  })
})

describe('fetchLiteEmbedding', () => {
  const servers: http.Server[] = []
  afterEach(async () => {
    for (const s of servers.splice(0)) await new Promise((r) => s.close(r))
  })
  async function serve(status: number, body: unknown): Promise<{ port: number; seen: http.IncomingMessage[] }> {
    const seen: http.IncomingMessage[] = []
    const server = http.createServer((req, res) => {
      seen.push(req)
      res.writeHead(status, { 'content-type': 'application/json' })
      res.end(JSON.stringify(body))
    })
    servers.push(server)
    await new Promise<void>((r) => server.listen(0, '127.0.0.1', r))
    return { port: (server.address() as AddressInfo).port, seen }
  }

  it('asks the daemon with the ProductHost credential and returns what the Lite needs', async () => {
    const { port, seen } = await serve(200, { base_url: 'http://127.0.0.1:9100/v1', api_key: 'idk_x', model: 'idoris/embed', dimension: 1024 })
    const embedding = await fetchLiteEmbedding({ port, authHeaders: { Authorization: 'Bearer host' } })
    expect(embedding).toEqual({ baseUrl: 'http://127.0.0.1:9100/v1', apiKey: 'idk_x', model: 'idoris/embed', dimension: 1024 })
    expect(seen[0].method).toBe('POST')
    expect(seen[0].url).toBe(LITE_EMBEDDING_PATH)
    expect(seen[0].headers.authorization).toBe('Bearer host')
  })

  it('a refusal, a bad answer or no daemon is unavailable, with a code and no key', async () => {
    const refused = await serve(422, { error: { code: 'not_configured' } })
    await expect(fetchLiteEmbedding({ port: refused.port, authHeaders: {} })).rejects.toThrow('idoris_unavailable')
    const odd = await serve(200, { base_url: 1 })
    await expect(fetchLiteEmbedding({ port: odd.port, authHeaders: {} })).rejects.toThrow('invalid_response')
    await expect(fetchLiteEmbedding(null)).rejects.toThrow('daemon_unavailable')
  })
})
