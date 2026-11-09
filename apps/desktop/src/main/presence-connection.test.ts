import { describe, it, expect, afterEach, vi } from 'vitest'
import fs from 'node:fs'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { PRESENCE, PresenceConnection, parseSocketChallenge, type PresenceDecision, type SocketChallenge } from './presence-connection'

// M5 B3: the presence connection against a fake daemon on a real Unix socket,
// speaking K-B3's frames (#1715).

const NONCE = '0123456789abcdef0123456789abcdef'
const cleanups: Array<() => void | Promise<void>> = []
afterEach(async () => { for (const c of cleanups.splice(0).reverse()) await c() })

interface Peer {
  line: () => Promise<Record<string, unknown>>
  send: (f: Record<string, unknown>) => void
  end: () => void
}

/** Each connection runs `script`; returns the socket path and how many connected. */
async function fakeDaemon(script: (peer: Peer, n: number) => Promise<void>) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-pres-'))
  const sock = path.join(dir, 'e.sock')
  let connections = 0
  const server = net.createServer((socket) => {
    connections += 1
    let buf = ''
    const lines: string[] = []
    const waiting: Array<(l: string) => void> = []
    socket.on('data', (d) => {
      buf += d.toString('utf8')
      let nl: number
      while ((nl = buf.indexOf('\n')) >= 0) {
        const l = buf.slice(0, nl)
        buf = buf.slice(nl + 1)
        const w = waiting.shift()
        if (w) w(l)
        else lines.push(l)
      }
    })
    socket.on('error', () => {})
    const peer: Peer = {
      line: () => new Promise((resolve) => {
        const l = lines.shift()
        if (l !== undefined) resolve(JSON.parse(l))
        else waiting.push((x) => resolve(JSON.parse(x)))
      }),
      send: (f) => { socket.write(JSON.stringify({ v: 1, ...f }) + '\n') },
      end: () => socket.end(),
    }
    void script(peer, connections).catch(() => socket.destroy())
  })
  await new Promise<void>((resolve) => server.listen(sock, resolve))
  cleanups.push(() => { server.close(); fs.rmSync(dir, { recursive: true, force: true }) })
  return { sock, connections: () => connections }
}

/** The handshake as the daemon runs it; returns the hello. */
async function handshake(peer: Peer): Promise<Record<string, unknown>> {
  peer.send({ op: 'challenge', nonce: NONCE })
  const hello = await peer.line()
  peer.send({ op: PRESENCE.ready, identity: { kind: 'macos_seccode', identifier: 'ai.auraai.agent24' } })
  return hello
}

const challenge = (over: Record<string, unknown> = {}) => ({
  op: PRESENCE.challenge,
  challenge_id: 'ch1',
  nonce: 'n-ch1',
  operation: 'ApprovalDecision',
  method: 'POST',
  path: '/api/v1/approvals/apr_1',
  summary: 'Approve run run_1',
  requester: { identifier: 'ai.auraai.agent24', pid: 42 },
  expires_at: Math.floor(Date.now() / 1000) + 60,
  ...over,
})

function connection(sock: string, ask: (c: SocketChallenge, signal: AbortSignal) => Promise<PresenceDecision>) {
  const log = vi.fn()
  const pc = new PresenceConnection({ target: () => ({ socketPath: sock, generation: 'gen-1' }), ask, log })
  pc.start()
  cleanups.push(() => pc.stop())
  return { pc, log }
}

describe('presence connection (M5 B3, K-B3 frames)', () => {
  it('says hello as the presence role, never asks for a token, and is ready once the daemon says so', async () => {
    let hello: Record<string, unknown> = {}
    const { sock } = await fakeDaemon(async (peer) => { hello = await handshake(peer); await new Promise(() => {}) })
    const { pc } = connection(sock, async () => 'deny')
    await vi.waitFor(() => expect(pc.ready).toBe(true))
    expect(hello).toEqual({ v: 1, op: 'hello', role: 'presence', generation: 'gen-1', nonce: NONCE })
  })

  it('asks for a challenge and answers on the same connection with its id, nonce and decision', async () => {
    let answer: Record<string, unknown> = {}
    const { sock } = await fakeDaemon(async (peer) => {
      await handshake(peer)
      peer.send(challenge())
      answer = await peer.line()
      peer.send({ op: PRESENCE.answered, challenge_id: 'ch1', accepted: true })
      await new Promise(() => {})
    })
    const asked: SocketChallenge[] = []
    connection(sock, async (c) => { asked.push(c); return 'allow' })
    await vi.waitFor(() => expect(answer).toEqual({ v: 1, op: PRESENCE.answer, challenge_id: 'ch1', nonce: 'n-ch1', decision: 'allow' }))
    expect(asked[0]).toMatchObject({ challenge_id: 'ch1', method: 'POST', path: '/api/v1/approvals/apr_1', summary: 'Approve run run_1', requester: { identifier: 'ai.auraai.agent24', pid: 42 } })
  })

  it('a box that fails answers deny', async () => {
    let answer: Record<string, unknown> = {}
    const { sock } = await fakeDaemon(async (peer) => {
      await handshake(peer)
      peer.send(challenge())
      answer = await peer.line()
      await new Promise(() => {})
    })
    connection(sock, async () => { throw new Error('no window') })
    await vi.waitFor(() => expect(answer['decision']).toBe('deny'))
  })

  it('answers each ping with its nonce', async () => {
    let pong: Record<string, unknown> = {}
    const { sock } = await fakeDaemon(async (peer) => {
      await handshake(peer)
      peer.send({ op: PRESENCE.ping, nonce: 'p1' })
      pong = await peer.line()
      await new Promise(() => {})
    })
    connection(sock, async () => 'deny')
    await vi.waitFor(() => expect(pong).toEqual({ v: 1, op: PRESENCE.pong, nonce: 'p1' }))
  })

  it('a cancel closes the open box and nothing is answered', async () => {
    let closed = false
    const lines: Record<string, unknown>[] = []
    const { sock } = await fakeDaemon(async (peer) => {
      await handshake(peer)
      peer.send(challenge())
      await new Promise((r) => setTimeout(r, 50))
      peer.send({ op: PRESENCE.cancel, challenge_id: 'ch1' })
      // Anything main sends from here on.
      for (;;) lines.push(await peer.line())
    })
    connection(sock, (_c, signal) => new Promise((resolve) => {
      signal.addEventListener('abort', () => { closed = true; resolve('allow') })
    }))
    await vi.waitFor(() => expect(closed).toBe(true))
    await new Promise((r) => setTimeout(r, 50))
    expect(lines).toEqual([])
  })

  it('a challenge that expires while the box is up is not answered', async () => {
    const lines: Record<string, unknown>[] = []
    let aborted = false
    const { sock } = await fakeDaemon(async (peer) => {
      await handshake(peer)
      peer.send(challenge({ expires_at: Date.now() / 1000 + 0.1 }))
      for (;;) lines.push(await peer.line())
    })
    connection(sock, (_c, signal) => new Promise((resolve) => {
      signal.addEventListener('abort', () => { aborted = true; setTimeout(() => resolve('allow'), 10) })
    }))
    await vi.waitFor(() => expect(aborted).toBe(true))
    await new Promise((r) => setTimeout(r, 50))
    expect(lines).toEqual([])
  })

  it('the connection ending closes the open box, and it connects again', async () => {
    let closed = false
    const { sock, connections } = await fakeDaemon(async (peer, n) => {
      await handshake(peer)
      if (n === 1) {
        peer.send(challenge())
        await new Promise((r) => setTimeout(r, 50))
        peer.end()
        return
      }
      await new Promise(() => {})
    })
    const { pc } = connection(sock, (_c, signal) => new Promise((resolve) => {
      signal.addEventListener('abort', () => { closed = true; resolve('allow') })
    }))
    await vi.waitFor(() => expect(closed).toBe(true))
    await vi.waitFor(() => expect(connections()).toBe(2), { timeout: 5_000 })
    await vi.waitFor(() => expect(pc.ready).toBe(true))
  })

  it.each([
    ['an enrolled frame instead of presence_ready', (peer: Peer) => { peer.send({ op: 'enrolled', token: 'tok', http_port: 1 }) }],
    ['a presence_conflict error', (peer: Peer) => { peer.send({ op: 'error', code: 'presence_conflict', message: 'busy' }) }],
  ])('%s: not ready, logged by code, tried again later', async (_name, reply) => {
    const { sock, connections } = await fakeDaemon(async (peer, n) => {
      peer.send({ op: 'challenge', nonce: NONCE })
      await peer.line()
      if (n === 1) { reply(peer); return }
      peer.send({ op: PRESENCE.ready, identity: {} })
      await new Promise(() => {})
    })
    const { pc, log } = connection(sock, async () => 'deny')
    await vi.waitFor(() => expect(connections()).toBe(2), { timeout: 5_000 })
    await vi.waitFor(() => expect(pc.ready).toBe(true))
    expect(log.mock.calls.flat().join('\n')).not.toContain('tok')
  })

  it.each([
    ['a second challenge while one is open', [challenge(), challenge({ challenge_id: 'ch2', nonce: 'n-ch2' })]],
    ['a malformed challenge', [challenge({ nonce: '' })]],
  ])('%s ends the connection without answering', async (_name, frames) => {
    const lines: Record<string, unknown>[] = []
    const asked: string[] = []
    const { sock, connections } = await fakeDaemon(async (peer, n) => {
      await handshake(peer)
      if (n === 1) for (const f of frames) peer.send(f)
      for (;;) lines.push(await peer.line())
    })
    connection(sock, (c, signal) => new Promise((resolve) => { asked.push(c.challenge_id); signal.addEventListener('abort', () => resolve('allow')) }))
    await vi.waitFor(() => expect(connections()).toBe(2), { timeout: 5_000 })
    await new Promise((r) => setTimeout(r, 50))
    expect(lines.filter((l) => l['op'] === PRESENCE.answer)).toEqual([])
    expect(asked.length).toBeLessThanOrEqual(1)
  })

  it('stays idle with no service daemon to connect to', async () => {
    const pc = new PresenceConnection({ target: () => null, ask: async () => 'deny', sleep: async (_ms, signal) => { await new Promise((r) => setTimeout(r, 5)); void signal } })
    pc.start()
    await new Promise((r) => setTimeout(r, 30))
    expect(pc.ready).toBe(false)
    await pc.stop()
  })
})

describe('parseSocketChallenge', () => {
  it('takes a K-B3 challenge and refuses one without a nonce, method or path', () => {
    expect(parseSocketChallenge(challenge())).toMatchObject({ challenge_id: 'ch1', nonce: 'n-ch1', method: 'POST' })
    for (const drop of ['nonce', 'method', 'path', 'challenge_id', 'expires_at', 'requester']) {
      const c: Record<string, unknown> = challenge()
      delete c[drop]
      expect(parseSocketChallenge(c), drop).toBeNull()
    }
  })
})
