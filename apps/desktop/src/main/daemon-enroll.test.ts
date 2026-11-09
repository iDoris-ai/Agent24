import { describe, it, expect, afterEach, vi } from 'vitest'
import fs from 'node:fs'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { createHash } from 'node:crypto'
import { bodySha256, enrol, EnrolError, MAX_FRAME_BYTES, ENROL_STEP_MS, SUMMARY_MAX_CHARS, type ElevateRequest, type EnrolSession } from './daemon-enroll'

// A fake daemon on a real Unix socket: `script` runs once per connection with
// helpers to read the client's frames and write raw lines.
type Peer = {
  socket: net.Socket
  line: () => Promise<Record<string, unknown>>
  write: (raw: string) => void
  /** Bytes the client sent before the daemon spoke. */
  early: () => number
}

const NONCE = '0123456789abcdef0123456789ABCDEF'
const TOKEN = 'daily-secret-token-value'
const cleanups: Array<() => void> = []
afterEach(() => { for (const c of cleanups.splice(0)) c() })

async function fakeDaemon(script: (peer: Peer) => Promise<void>): Promise<string> {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-enrol-'))
  const sock = path.join(dir, 'e.sock')
  const server = net.createServer((socket) => {
    let buf = ''
    let spoke = false
    let early = 0
    const waiting: Array<(l: string) => void> = []
    const lines: string[] = []
    socket.on('data', (d) => {
      if (!spoke) early += d.length
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
      socket,
      line: () => new Promise((resolve) => {
        const l = lines.shift()
        if (l !== undefined) resolve(JSON.parse(l))
        else waiting.push((x) => resolve(JSON.parse(x)))
      }),
      write: (raw) => { spoke = true; socket.write(raw) },
      early: () => early,
    }
    void script(peer).catch(() => socket.destroy())
  })
  await new Promise<void>((resolve) => server.listen(sock, resolve))
  cleanups.push(() => { server.close(); fs.rmSync(dir, { recursive: true, force: true }) })
  return sock
}

const frame = (f: Record<string, unknown>) => JSON.stringify({ v: 1, ...f }) + '\n'
const enrolled = (token = TOKEN, port = 5555) =>
  frame({ op: 'enrolled', token, expires_in: 3600, tier: 'daily', identity: { kind: 'macos_seccode', identifier: 'ai.auraai.agent24' }, http_port: port })

async function codeOf(p: Promise<unknown>): Promise<string> {
  try {
    await p
  } catch (error) {
    expect(error).toBeInstanceOf(EnrolError)
    expect((error as Error).message).not.toContain(TOKEN)
    return (error as EnrolError).code
  }
  throw new Error('expected a rejection')
}

describe('enrol (M5 B2, protocol v1)', () => {
  it('waits for the challenge, echoes its nonce with the generation, and returns the daily token', async () => {
    let hello: Record<string, unknown> = {}
    let early = -1
    const sock = await fakeDaemon(async (peer) => {
      await new Promise((r) => setTimeout(r, 50))
      early = peer.early()
      peer.write(frame({ op: 'challenge', conn_id: 'c1', nonce: NONCE }))
      hello = await peer.line()
      peer.write(enrolled())
    })
    const session = await enrol({ socketPath: sock, generation: 'gen-7', httpPort: 5555 })
    expect(early).toBe(0)
    expect(hello).toEqual({ v: 1, op: 'hello', generation: 'gen-7', nonce: NONCE })
    expect(session.token).toBe(TOKEN)
    session.close()
  })

  it('renews on the same connection; the new token replaces the old', async () => {
    let renew: Record<string, unknown> = {}
    const sock = await fakeDaemon(async (peer) => {
      peer.write(frame({ op: 'challenge', nonce: NONCE }))
      await peer.line()
      peer.write(enrolled())
      renew = await peer.line()
      peer.write(enrolled('renewed-token'))
    })
    const session = await enrol({ socketPath: sock, generation: 'g', httpPort: 5555 })
    await expect(session.renew()).resolves.toBe('renewed-token')
    expect(renew).toEqual({ v: 1, op: 'renew' })
    expect(session.token).toBe('renewed-token')
    session.close()
  })

  /** A daemon that enrols, then answers each renew with `answer(n)` (null: no answer). */
  async function renewing(answer: (n: number) => string | null) {
    let renews = 0
    const sock = await fakeDaemon(async (peer) => {
      peer.write(frame({ op: 'challenge', nonce: NONCE }))
      await peer.line()
      peer.write(enrolled())
      for (;;) {
        await peer.line()
        renews += 1
        const reply = answer(renews)
        if (reply !== null) peer.write(reply)
      }
    })
    return { session: await enrol({ socketPath: sock, generation: 'g', httpPort: 5555 }), renews: () => renews }
  }

  it('a renew answered with another port is refused and ends the session (B3a, PR-Daemon #1725)', async () => {
    const { session } = await renewing(() => enrolled('elsewhere', 6666))
    expect(await codeOf(session.renew())).toBe('endpoint_mismatch')
    expect(session.token).toBe(TOKEN)
    const why = await new Promise<EnrolError>((resolve) => session.onClose(resolve))
    expect(why.code).toBe('endpoint_mismatch')
  })

  it('renews asked for at once share one renew frame (B3a)', async () => {
    const { session, renews } = await renewing((n) => enrolled(`renewed-${n}`))
    const a = session.renew()
    const b = session.renew()
    expect(b).toBe(a)
    await expect(a).resolves.toBe('renewed-1')
    await expect(session.renew()).resolves.toBe('renewed-2')
    expect(renews()).toBe(2)
    session.close()
  })

  it('a renew the daemon never answers times out (B3a)', async () => {
    const { session } = await renewing(() => null)
    expect(await codeOf(session.renew())).toBe('timeout')
  }, ENROL_STEP_MS + 5_000)

  it('a renew on a connection already gone fails at once, and the next renew is a fresh try (B3a, PR-Daemon #1725)', async () => {
    const { session } = await renewing((n) => enrolled(`renewed-${n}`))
    session.close()
    const first = session.renew()
    expect(await codeOf(first)).toBe('closed')
    const second = session.renew()
    expect(second).not.toBe(first)
    expect(await codeOf(second)).toBe('closed')
  })

  it('a renew the daemon refuses closes the connection and says so once', async () => {
    const sock = await fakeDaemon(async (peer) => {
      peer.write(frame({ op: 'challenge', nonce: NONCE }))
      await peer.line()
      peer.write(enrolled())
      await peer.line()
      peer.write(frame({ op: 'error', code: 'not_enrolled', message: 'no such token' }))
    })
    const session = await enrol({ socketPath: sock, generation: 'g', httpPort: 5555 })
    const closed: string[] = []
    session.onClose((why) => closed.push(why.code))
    expect(await codeOf(session.renew())).toBe('not_enrolled')
    expect(closed).toEqual(['not_enrolled'])
    session.close()
    expect(closed).toHaveLength(1)
  })

  it('the daemon going away ends the session', async () => {
    const sock = await fakeDaemon(async (peer) => {
      peer.write(frame({ op: 'challenge', nonce: NONCE }))
      await peer.line()
      peer.write(enrolled())
      peer.socket.end()
    })
    const session = await enrol({ socketPath: sock, generation: 'g', httpPort: 5555 })
    const why = await new Promise<EnrolError>((resolve) => session.onClose(resolve))
    expect(why.code).toBe('daemon_unavailable')
  })

  it.each([
    ['an identity the daemon rejects', frame({ op: 'error', code: 'identity_rejected', message: 'unsigned' }), 'identity_rejected'],
    ['a stale generation', frame({ op: 'error', code: 'generation_mismatch', message: '' }), 'generation_mismatch'],
    ['another protocol version', JSON.stringify({ v: 2, op: 'enrolled', token: TOKEN, http_port: 5555 }) + '\n', 'unsupported_version'],
    ['a token for another port', enrolled(TOKEN, 6666), 'endpoint_mismatch'],
    ['no token', frame({ op: 'enrolled', token: '', http_port: 5555 }), 'bad_frame'],
    ['a frame that is not JSON', 'not json\n', 'bad_frame'],
    ['a valid enrolled frame over 16 KiB', frame({ op: 'enrolled', token: TOKEN, http_port: 5555, pad: 'x'.repeat(MAX_FRAME_BYTES) }), 'bad_frame'],
    ['16 KiB with no end of line', 'x'.repeat(MAX_FRAME_BYTES + 1), 'bad_frame'],
  ])('refuses %s after hello', async (_name, reply, code) => {
    const sock = await fakeDaemon(async (peer) => {
      peer.write(frame({ op: 'challenge', nonce: NONCE }))
      await peer.line()
      peer.write(reply)
    })
    expect(await codeOf(enrol({ socketPath: sock, generation: 'g', httpPort: 5555 }))).toBe(code)
  })

  /** An enrolled frame padded so that it is `total` bytes with its newline. */
  const enrolledOf = (total: number) => {
    const base = enrolled().length
    return frame({ op: 'enrolled', token: TOKEN, expires_in: 3600, tier: 'daily', identity: { kind: 'macos_seccode', identifier: 'ai.auraai.agent24' }, http_port: 5555, pad: 'x'.repeat(total - base - ',"pad":""'.length) })
  }

  it('takes a frame of exactly 16 KiB with its newline and refuses one byte more, as the Rust readers do (Codex B2 C3)', async () => {
    for (const [total, ok] of [[MAX_FRAME_BYTES, true], [MAX_FRAME_BYTES + 1, false]] as const) {
      const reply = enrolledOf(total)
      expect(Buffer.byteLength(reply)).toBe(total)
      const sock = await fakeDaemon(async (peer) => {
        peer.write(frame({ op: 'challenge', nonce: NONCE }))
        await peer.line()
        peer.write(reply)
      })
      const result = enrol({ socketPath: sock, generation: 'g', httpPort: 5555 })
      if (ok) (await result).close()
      else expect(await codeOf(result)).toBe('bad_frame')
    }
  })

  it.each([
    ['a challenge with a short nonce', frame({ op: 'challenge', nonce: 'abc' })],
    ['a challenge with a non-hex nonce', frame({ op: 'challenge', nonce: 'z'.repeat(32) })],
    ['something other than a challenge', enrolled()],
  ])('never answers %s with a hello', async (_name, first) => {
    let answered = false
    const sock = await fakeDaemon(async (peer) => {
      peer.write(first)
      await peer.line()
      answered = true
    })
    expect(await codeOf(enrol({ socketPath: sock, generation: 'g', httpPort: 5555 }))).toBe('bad_frame')
    await new Promise((r) => setTimeout(r, 30))
    expect(answered).toBe(false)
  })

  it('a daemon that never speaks times out the handshake', async () => {
    const sock = await fakeDaemon(async () => {})
    const started = Date.now()
    expect(await codeOf(enrol({ socketPath: sock, generation: 'g', httpPort: 5555 }))).toBe('timeout')
    expect(Date.now() - started).toBeGreaterThanOrEqual(ENROL_STEP_MS - 50)
  }, ENROL_STEP_MS + 5_000)

  it('no socket is daemon_unavailable', async () => {
    const missing = path.join(os.tmpdir(), `a24-enrol-missing-${process.pid}.sock`)
    expect(await codeOf(enrol({ socketPath: missing, generation: 'g', httpPort: 5555 }))).toBe('daemon_unavailable')
  })
})

describe('elevate on the token connection (M5 B3)', () => {
  const GRANT = 'one-shot-grant-value'
  /** Elevate in a turn, then `use` the grant in the same turn. */
  const elevateIn = <T>(s: EnrolSession, r: ElevateRequest, use: (grant: string) => Promise<T>): Promise<T> =>
    s.turn(async (elevate) => use(await elevate(r)))
  const request = { method: 'POST', path: '/api/v1/approvals/apr_1', body: '{"decision":"approve"}', summary: 'Agent24: POST /api/v1/approvals/apr_1' }

  /** A daemon that enrols, then answers each `elevate` with `reply(n)` and each renew with a token. */
  async function daemon(reply: (n: number) => string[]) {
    const got: Record<string, unknown>[] = []
    let elevates = 0
    const sock = await fakeDaemon(async (peer) => {
      peer.write(frame({ op: 'challenge', nonce: NONCE }))
      await peer.line()
      peer.write(enrolled())
      for (;;) {
        const f = await peer.line()
        got.push(f)
        if (f['op'] === 'renew') peer.write(enrolled(`renewed-${got.length}`))
        if (f['op'] === 'elevate') {
          elevates += 1
          for (const line of reply(elevates)) peer.write(line)
        }
      }
    })
    const session = await enrol({ socketPath: sock, generation: 'g', httpPort: 5555 })
    cleanups.push(() => session.close())
    return { session, got }
  }
  const granted = (id = 'ch1') => [frame({ op: 'elevation_pending', challenge_id: id }), frame({ op: 'elevated', challenge_id: id, grant: GRANT, expires_in: 60 })]

  it('asks with the method, path, body hash and summary, and runs `use` with the grant', async () => {
    const { session, got } = await daemon(() => granted())
    const used: string[] = []
    await expect(elevateIn(session, request, async (g) => { used.push(g); return 'sent' })).resolves.toBe('sent')
    expect(used).toEqual([GRANT])
    expect(got).toEqual([{
      v: 1, op: 'elevate', method: 'POST', path: '/api/v1/approvals/apr_1',
      body_sha256: createHash('sha256').update('{"decision":"approve"}').digest('hex'),
      summary: 'Agent24: POST /api/v1/approvals/apr_1',
    }])
  })

  it('hashes no body as the empty string, and cuts the summary at 200 characters, not UTF-16 units', async () => {
    expect(bodySha256('')).toBe('e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855')
    const { session, got } = await daemon(() => granted())
    await elevateIn(session, { ...request, body: '', summary: '😀'.repeat(250) }, async () => null)
    expect(Array.from(String(got[0]?.['summary']))).toHaveLength(SUMMARY_MAX_CHARS)
    expect(got[0]?.['body_sha256']).toBe(bodySha256(''))
  })

  it('a renew asked meanwhile goes only after `use` has finished: the grant is bound to the token it was asked with', async () => {
    const { session, got } = await daemon(() => granted())
    const order: string[] = []
    let release!: () => void
    const elevating = elevateIn(session, request, async () => {
      order.push('use')
      await new Promise<void>((r) => { release = r })
      order.push('used')
    })
    const renewed = session.renew().then(() => { order.push('renewed') })
    await vi.waitFor(() => expect(order).toEqual(['use']))
    await new Promise((r) => setTimeout(r, 30))
    expect(got.map((f) => f['op'])).toEqual(['elevate'])
    release()
    await Promise.all([elevating, renewed])
    expect(order).toEqual(['use', 'used', 'renewed'])
    expect(got.map((f) => f['op'])).toEqual(['elevate', 'renew'])
  })

  it('a refusal from the daemon rejects with its code and leaves the connection open', async () => {
    const { session, got } = await daemon((n) => n === 1
      ? [frame({ op: 'elevation_pending', challenge_id: 'ch1' }), frame({ op: 'error', code: 'presence_denied', message: 'Denied' })]
      : granted('ch2'))
    expect(await codeOf(elevateIn(session, request, async () => null))).toBe('presence_denied')
    await expect(elevateIn(session, request, async (g) => g)).resolves.toBe(GRANT)
    await expect(session.renew()).resolves.toMatch(/^renewed-/)
    expect(got.map((f) => f['op'])).toEqual(['elevate', 'elevate', 'renew'])
  })

  it.each([
    ['elevated with no pending', () => [frame({ op: 'elevated', challenge_id: 'ch1', grant: GRANT })]],
    ['elevated for another challenge', () => [frame({ op: 'elevation_pending', challenge_id: 'ch1' }), frame({ op: 'elevated', challenge_id: 'ch2', grant: GRANT })]],
    ['a second pending', () => [frame({ op: 'elevation_pending', challenge_id: 'ch1' }), frame({ op: 'elevation_pending', challenge_id: 'ch1' })]],
    ['a pending with no id', () => [frame({ op: 'elevation_pending', challenge_id: '' })]],
    ['elevated with no grant', () => [frame({ op: 'elevation_pending', challenge_id: 'ch1' }), frame({ op: 'elevated', challenge_id: 'ch1', grant: '' })]],
    ['an unexpected frame', () => [frame({ op: 'elevation_pending', challenge_id: 'ch1' }), enrolled()]],
  ])('%s: bad_frame, `use` never runs, and the connection ends', async (_name, reply) => {
    const { session } = await daemon(reply)
    let used = false
    const closed = new Promise<EnrolError>((resolve) => session.onClose(resolve))
    const err = elevateIn(session, request, async () => { used = true })
    expect(await codeOf(err)).toBe('bad_frame')
    expect((await closed).code).toBe('bad_frame')
    expect(used).toBe(false)
  })

  it('elevate kept past its turn is refused, and sends nothing', async () => {
    const { session, got } = await daemon(() => granted())
    let kept!: (r: ElevateRequest) => Promise<string>
    await session.turn(async (elevate) => { kept = elevate })
    expect(await codeOf(kept(request))).toBe('closed')
    await session.renew()
    expect(got.map((f) => f['op'])).toEqual(['renew'])
  })

  it('a turn is given the token it is bound to: the current one, renewed or not (Codex B3b r2 C1)', async () => {
    const { session } = await daemon(() => granted())
    await expect(session.turn(async (_e, token) => token)).resolves.toBe(TOKEN)
    const renewed = await session.renew()
    await expect(session.turn(async (_e, token) => token)).resolves.toBe(renewed)
  })

  it('a turn on a connection that has ended never runs (Codex B3b r2 C1)', async () => {
    const { session } = await daemon(() => granted())
    session.close()
    let ran = false
    expect(await codeOf(session.turn(async () => { ran = true }))).toBe('closed')
    expect(ran).toBe(false)
  })

  /** A daemon that enrols, then hands each frame main sends to `script`. */
  async function scripted(script: (f: Record<string, unknown>, peer: Peer) => Promise<void> | void, opts: { elevationWaitMs?: number } = {}) {
    const got: Record<string, unknown>[] = []
    const sock = await fakeDaemon(async (peer) => {
      peer.write(frame({ op: 'challenge', nonce: NONCE }))
      await peer.line()
      peer.write(enrolled())
      for (;;) {
        const f = await peer.line()
        got.push(f)
        await script(f, peer)
      }
    })
    const session = await enrol({ socketPath: sock, generation: 'g', httpPort: 5555 }, undefined, opts)
    cleanups.push(() => session.close())
    return { session, got }
  }

  it('a second elevate while one is out is refused: one frame only (PR-Daemon #1736 C1)', async () => {
    const { session, got } = await scripted((f, peer) => { if (f['op'] === 'elevate') for (const l of granted()) peer.write(l) })
    const second = await session.turn(async (elevate) => {
      const first = elevate(request)
      const again = await codeOf(elevate(request))
      await first
      return again
    })
    expect(second).toBe('busy')
    expect(got.map((f) => f['op'])).toEqual(['elevate'])
  })

  it('a turn whose fn returns while an elevate is out ends only once it has settled: a renew waits (C2)', async () => {
    let answer!: () => void
    const { session, got } = await scripted(async (f, peer) => {
      if (f['op'] === 'elevate') {
        peer.write(frame({ op: 'elevation_pending', challenge_id: 'ch1' }))
        await new Promise<void>((r) => { answer = r })
        peer.write(frame({ op: 'elevated', challenge_id: 'ch1', grant: GRANT }))
      }
      if (f['op'] === 'renew') peer.write(enrolled('renewed'))
    })
    let asked!: Promise<string>
    let ended = false
    const turn = session.turn(async (elevate) => { asked = elevate(request) }).then(() => { ended = true })
    const renewed = session.renew()
    await vi.waitFor(() => expect(answer).toBeTypeOf('function'))
    await new Promise((r) => setTimeout(r, 50))
    expect(ended).toBe(false)
    expect(got.map((f) => f['op'])).toEqual(['elevate'])
    answer()
    await expect(asked).resolves.toBe(GRANT)
    await turn
    await expect(renewed).resolves.toBe('renewed')
    expect(got.map((f) => f['op'])).toEqual(['elevate', 'renew'])
  })

  it('a grant queued behind a frame that broke the connection is not taken (C4)', async () => {
    const { session } = await scripted((f, peer) => {
      if (f['op'] === 'elevate') peer.write(frame({ op: 'elevation_pending', challenge_id: 'ch1' }) + frame({ op: 'elevated', challenge_id: 'ch1', grant: GRANT }) + 'not json\n')
    })
    let used = false
    await new Promise((r) => setTimeout(r, 0))
    const code = await codeOf(elevateIn(session, request, async () => { used = true }))
    expect(['closed', 'bad_frame']).toContain(code)
    expect(used).toBe(false)
  })

  it('the wait is one deadline for the whole exchange, not per frame (C5)', async () => {
    const { session } = await scripted(async (f, peer) => {
      if (f['op'] !== 'elevate') return
      await new Promise((r) => setTimeout(r, 120))
      peer.write(frame({ op: 'elevation_pending', challenge_id: 'ch1' }))
      await new Promise((r) => setTimeout(r, 120))
      peer.write(frame({ op: 'elevated', challenge_id: 'ch1', grant: GRANT }))
    }, { elevationWaitMs: 200 })
    let used = false
    expect(await codeOf(elevateIn(session, request, async () => { used = true }))).toBe('presence_timeout')
    expect(used).toBe(false)
  })

  it('hashes the exact body bytes, whitespace and all', async () => {
    const { session, got } = await scripted((f, peer) => { if (f['op'] === 'elevate') for (const l of granted()) peer.write(l) })
    await elevateIn(session, { ...request, body: ' {"a": 1}\n' }, async () => null)
    expect(got[0]?.['body_sha256']).toBe(createHash('sha256').update(' {"a": 1}\n').digest('hex'))
  })

  it('on a connection already gone, rejects at once and never runs `use`', async () => {
    const { session } = await daemon(() => granted())
    session.close()
    let used = false
    expect(await codeOf(elevateIn(session, request, async () => { used = true }))).toBe('closed')
    expect(used).toBe(false)
  })
})
