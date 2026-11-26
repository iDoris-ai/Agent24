import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { WeknoraLiteHost, type WeknoraLiteHostStatus } from './weknora-lite-host'
import type { WeknoraLiteEmbedding } from './weknora-lite-sidecar'

// A stand-in for `WeKnora-lite --managed`: writes the protocol-2 handshake to fd 3,
// serves the ready route on 127.0.0.1 behind X-API-Key, and exits on lifeline EOF (fd 4).
// `mode` (a file in its cwd) makes it fail before the handshake, or crash once after ready.
const FAKE = `
const fs = require('node:fs'), http = require('node:http'), path = require('node:path')
const args = process.argv.slice(2)
const state = args[args.indexOf('--state-dir') + 1]
const mode = fs.existsSync('mode') ? fs.readFileSync('mode', 'utf8').trim() : 'ok'
const runs = path.join(state, 'runs')
const run = (fs.existsSync(runs) ? Number(fs.readFileSync(runs, 'utf8')) : 0) + 1
fs.writeFileSync(runs, String(run))
fs.writeFileSync(path.join(state, 'pid'), String(process.pid))
fs.writeFileSync(path.join(state, 'env.json'), JSON.stringify(Object.keys(process.env)))
if (mode === 'fail') process.exit(1)
const key = 'k' + run + '-'.repeat(40)
const life = new (require('node:net').Socket)({ fd: 4, readable: true, writable: false })
life.on('data', () => {}); life.on('end', () => process.exit(0)); life.on('error', () => process.exit(0))
const born = Date.now()
const server = http.createServer((req, res) => {
  res.statusCode = req.method === 'POST' && req.url === '/api/v1/agent24/knowledge/ready' && req.headers['x-api-key'] === key ? 200 : 401
  // degrade-once: the first run stops answering its probes after a while, but stays alive.
  if (mode === 'degrade-once' && run === 1 && Date.now() - born > 400) res.statusCode = 503
  res.end('{}')
})
server.listen(0, '127.0.0.1', () => {
  const origin = 'http://127.0.0.1:' + server.address().port
  // bad-handshake: a syntax error right after the key, so a quoting parser would echo it.
  if (mode === 'bad-handshake') { fs.writeSync(3, '{"type":"ready","protocol":2,"credential":{"api_key":"BADKEYabcdefghijklmnopqrstuvwxyz"x}}\\n'); return }
  fs.writeSync(3, JSON.stringify({ type: 'ready', protocol: 2, phase: 'process', origin, pid: process.pid, version: 'fake', commit: 'x', edition: 'lite',
    credential: { api_key: key, knowledge_base_id: 'kb-1', expires_at: null } }) + '\\n')
  fs.writeSync(3, JSON.stringify({ type: 'models', protocol: 2, embedding: { state: 'ready' }, chat: { state: 'missing' } }) + '\\n')
  fs.closeSync(3)
  if (mode === 'crash-once' && run === 1) setTimeout(() => process.exit(7), 300)
})
process.on('SIGTERM', () => process.exit(0))
`

const embedding: WeknoraLiteEmbedding = { baseUrl: 'http://127.0.0.1:9/v1', apiKey: 'idk_' + 'a'.repeat(64), model: 'bge-m3', dimension: 1024 }
const silent = { info: () => {}, warn: () => {} }

let root: string
let program: string
let hosts: WeknoraLiteHost[] = []

beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'lite-host-'))
  program = path.join(root, 'program')
  fs.mkdirSync(program)
  fs.writeFileSync(path.join(program, 'WeKnora-lite'), `#!${process.execPath}\n${FAKE}`, { mode: 0o755 })
})

afterEach(async () => {
  await Promise.all(hosts.map((h) => h.shutdown()))
  hosts = []
  fs.rmSync(root, { recursive: true, force: true })
})

function host(overrides: { mode?: string; embedding?: () => Promise<WeknoraLiteEmbedding>; healthIntervalMs?: number } = {}): WeknoraLiteHost {
  if (overrides.mode) fs.writeFileSync(path.join(program, 'mode'), overrides.mode)
  const h = new WeknoraLiteHost({
    program: () => ({ binary: path.join(program, 'WeKnora-lite'), dir: program }),
    embedding: overrides.embedding ?? (async () => embedding),
    dataDir: path.join(root, 'data', 'weknora-lite'),
    logPath: path.join(root, 'logs', 'weknora-lite.log'),
    environment: { ...process.env, A24_SECRET: 'x', DB_PATH: '/elsewhere', JWT_SECRET: 'y' },
    logger: silent,
    readyPhaseTimeoutMs: 5_000,
    restartDelaysMs: [20, 20, 20, 20, 20],
    ...(overrides.healthIntervalMs ? { healthIntervalMs: overrides.healthIntervalMs } : {}),
  })
  hosts.push(h)
  return h
}

async function until(h: WeknoraLiteHost, pass: (s: WeknoraLiteHostStatus) => boolean, ms = 8_000): Promise<WeknoraLiteHostStatus> {
  const deadline = Date.now() + ms
  while (Date.now() < deadline) {
    const s = h.status()
    if (pass(s)) return s
    await new Promise((r) => setTimeout(r, 20))
  }
  throw new Error(`timed out; last ${JSON.stringify(h.status())}`)
}

const runs = () => Number(fs.readFileSync(path.join(root, 'data', 'weknora-lite', 'runs'), 'utf8'))
const alive = (pid: number) => { try { process.kill(pid, 0); return true } catch { return false } }

describe('WeknoraLiteHost', () => {
  it('starts, reads the handshake, and keeps the key in main only', async () => {
    const h = host()
    expect(h.credential()).toBeNull()
    await h.start()
    const s = await until(h, (x) => x.state === 'healthy')
    expect(s.origin).toMatch(/^http:\/\/127\.0\.0\.1:\d+$/)
    expect(s.knowledgeBaseId).toBe('kb-1')
    // The status (what 3c may show) never carries the key.
    expect(JSON.stringify(s)).not.toContain('k1--')
    expect(h.credential()).toMatchObject({ apiKey: `k1${'-'.repeat(40)}`, knowledgeBaseId: 'kb-1', origin: s.origin })

    const data = path.join(root, 'data', 'weknora-lite')
    expect(fs.statSync(data).mode & 0o777).toBe(0o700)
    const yaml = path.join(data, 'agent24-builtin-models.yaml')
    expect(fs.statSync(yaml).mode & 0o777).toBe(0o600)
    expect(fs.readFileSync(yaml, 'utf8')).not.toContain(embedding.apiKey)
    const env = JSON.parse(fs.readFileSync(path.join(data, 'env.json'), 'utf8')) as string[]
    expect(env).toEqual(expect.arrayContaining(['BUILTIN_MODELS_CONFIG', 'SSRF_WHITELIST', 'AGENT24_KNOWLEDGE_POLICY', 'A24LITE_EMBED_KEY']))
    expect(env.filter((k) => /^(A24_|DB_|JWT_)/.test(k))).toEqual([])
    // Nothing of the data lands in the program dir.
    expect(fs.readdirSync(program).sort()).toEqual(['WeKnora-lite'])
  })

  it('stop reaps the process and drops the credential', async () => {
    const h = host()
    await h.start()
    await until(h, (x) => x.state === 'healthy')
    const pid = Number(fs.readFileSync(path.join(root, 'data', 'weknora-lite', 'pid'), 'utf8'))
    expect(alive(pid)).toBe(true)
    const s = await h.stop()
    expect(s.state).toBe('stopped')
    expect(h.credential()).toBeNull()
    expect(alive(pid)).toBe(false)
  })

  it('restarts after a crash and comes back healthy with the new key', async () => {
    const h = host({ mode: 'crash-once' })
    await h.start()
    await until(h, (x) => x.state === 'healthy')
    await until(h, () => runs() === 2)
    await until(h, (x) => x.state === 'healthy')
    expect(h.credential()!.apiKey).toBe(`k2${'-'.repeat(40)}`)
    expect(h.status().gaveUp).toBe(false)
  })

  it('gives up after five restarts, then an explicit start tries again', async () => {
    const h = host({ mode: 'fail' })
    await h.start()
    const s = await until(h, (x) => x.gaveUp && x.state === 'stopped')
    expect(runs()).toBe(6)
    expect(s.error).toBeTruthy()
    await new Promise((r) => setTimeout(r, 150))
    expect(runs()).toBe(6)

    fs.writeFileSync(path.join(program, 'mode'), 'ok')
    await h.start()
    expect((await until(h, (x) => x.state === 'healthy')).gaveUp).toBe(false)
  })

  it('backs off when the embedding is not available, without launching', async () => {
    let calls = 0
    const h = host({ embedding: async () => { calls += 1; throw new Error('iDoris is not running') } })
    await h.start()
    const s = await until(h, (x) => x.gaveUp)
    expect(calls).toBe(6)
    expect(s.error).toBe('iDoris is not running')
    expect(fs.existsSync(path.join(root, 'data', 'weknora-lite', 'runs'))).toBe(false)
  })

  it('does not restart after shutdown', async () => {
    const h = host()
    await h.start()
    await until(h, (x) => x.state === 'healthy')
    await h.shutdown()
    expect((await h.start()).state).toBe('stopped')
    await new Promise((r) => setTimeout(r, 150))
    expect(runs()).toBe(1)
  })

  it('PR-Daemon #2108 G1: a start right after a stop (still reaping) brings the Lite up again', async () => {
    const h = host()
    await h.start()
    await until(h, (x) => x.state === 'healthy')
    const stopping = h.stop()
    await h.start()
    await stopping
    await until(h, (x) => x.state === 'healthy')
    expect(runs()).toBe(2)
  })

  it('G2: stopped while preparing, it reads stopped and never launches', async () => {
    let release: (e: WeknoraLiteEmbedding) => void = () => {}
    const h = host({ embedding: () => new Promise((resolve) => { release = resolve }) })
    const seen: string[] = []
    h.onStatus((s) => seen.push(s.state))
    const started = h.start()
    await until(h, (x) => x.state === 'starting')
    await h.stop()
    expect(h.status().state).toBe('stopped')
    release(embedding)
    await started
    await new Promise((r) => setTimeout(r, 100))
    expect(h.status().state).toBe('stopped')
    expect(seen.at(-1)).toBe('stopped')
    expect(fs.existsSync(path.join(root, 'data', 'weknora-lite', 'runs'))).toBe(false)
  })

  it('G3: alive but no longer answering (degraded) is restarted like a crash', async () => {
    const h = host({ mode: 'degrade-once', healthIntervalMs: 50 })
    await h.start()
    await until(h, (x) => x.state === 'healthy')
    await until(h, (x) => x.state === 'healthy' && runs() === 2, 10_000)
    expect(h.credential()?.apiKey).toBe(`k2${'-'.repeat(40)}`)
  })

  it('PR-Daemon #2108: a broken handshake line through the launcher never puts the key in the error or the log', async () => {
    const warnings: string[] = []
    if (fs.existsSync(path.join(program, 'mode'))) fs.rmSync(path.join(program, 'mode'))
    fs.writeFileSync(path.join(program, 'mode'), 'bad-handshake')
    const h = new WeknoraLiteHost({
      program: () => ({ binary: path.join(program, 'WeKnora-lite'), dir: program }),
      embedding: async () => embedding,
      dataDir: path.join(root, 'data', 'weknora-lite'),
      logPath: path.join(root, 'logs', 'weknora-lite.log'),
      logger: { info: () => {}, warn: (event, fields) => warnings.push(`${event} ${JSON.stringify(fields ?? {})}`) },
      readyPhaseTimeoutMs: 5_000,
      restartDelaysMs: [20],
    })
    hosts.push(h)
    await h.start()
    const s = await until(h, (x) => x.gaveUp)
    expect(s.error ?? '').toContain('not JSON')
    for (const text of [s.error ?? '', JSON.stringify(s), ...warnings]) {
      for (const piece of ['BADKEY', 'mnop', 'wxyz']) expect(text).not.toContain(piece)
    }
    expect(h.credential()).toBeNull()
  })
})
