import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { discoverDaemon } from './config.js'

// The shared daemon.json fixture (protocol/fixtures/daemon-state): data-dir,
// desktop, documents-accept and both bridges run this same file. The bridge
// bearer is the token file. daemon.json only supplies the port, and only
// after that file is a regular 0600 file that has not expired.
interface Case {
  name: string
  json?: unknown
  text?: string
  expect: { outcome: 'legacy' | 'capabilities' | 'rejected'; bearer?: string }
}
const fixture = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '..', 'protocol', 'fixtures', 'daemon-state', 'cases.json')
const cases = (JSON.parse(fs.readFileSync(fixture, 'utf8')) as { cases: Case[] }).cases

const BRIDGE = 'bridge-secret'
const FUTURE = 4_000_000_000

function writeToken(dir: string, token: string, expiresAt: number, mode = 0o600): string {
  const dev = path.join(dir, 'dev')
  fs.mkdirSync(dev, { recursive: true })
  fs.chmodSync(dev, 0o700)
  const file = path.join(dev, 'wechat-bridge.token')
  fs.writeFileSync(file, JSON.stringify({ token, expires_at: expiresAt }))
  fs.chmodSync(file, mode)
  return file
}

describe('discoverDaemon (token file, then the shared daemon.json parser)', () => {
  let dir: string
  const saved = { dataDir: process.env.AGENT24_DATA_DIR, base: process.env.A24_BASE_URL, token: process.env.A24_TOKEN }
  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'bridge-discover-'))
    process.env.AGENT24_DATA_DIR = dir
    delete process.env.A24_BASE_URL
    delete process.env.A24_TOKEN
  })
  afterEach(() => {
    for (const [k, v] of [['AGENT24_DATA_DIR', saved.dataDir], ['A24_BASE_URL', saved.base], ['A24_TOKEN', saved.token]] as const) {
      if (v === undefined) delete process.env[k]
      else process.env[k] = v
    }
    fs.rmSync(dir, { recursive: true, force: true })
  })

  for (const c of cases) {
    it(`fixture: ${c.name}`, () => {
      writeToken(dir, BRIDGE, FUTURE)
      fs.writeFileSync(path.join(dir, 'daemon.json'), c.text ?? JSON.stringify(c.json))
      if (c.expect.outcome === 'rejected') {
        expect(() => discoverDaemon()).toThrow()
      } else {
        const ep = discoverDaemon()
        expect(ep.token).toBe(BRIDGE)
        expect(ep.token).not.toBe(c.expect.bearer)
        expect(ep.base).toBe('http://127.0.0.1:4321')
      }
    })
  }

  it('uses the token file when daemon.json still carries a legacy bearer', () => {
    writeToken(dir, BRIDGE, FUTURE)
    fs.writeFileSync(
      path.join(dir, 'daemon.json'),
      JSON.stringify({ port: 7, pid: 2, version: 'v', token: 'legacy-secret' }),
    )
    expect(discoverDaemon()).toEqual({ base: 'http://127.0.0.1:7', token: BRIDGE })
  })

  it('refuses to start when the token file is missing and does not read daemon.json', () => {
    fs.writeFileSync(path.join(dir, 'daemon.json'), JSON.stringify({ port: 1, pid: 2, version: 'v', token: 'legacy-secret' }))
    fs.chmodSync(path.join(dir, 'daemon.json'), 0o000)
    expect(() => discoverDaemon()).toThrow(/agent24 dev bridge-token/)
    expect(() => discoverDaemon()).not.toThrow(/EACCES/)
  })

  it('refuses a token file that is not mode 0600', () => {
    writeToken(dir, BRIDGE, FUTURE, 0o644)
    fs.writeFileSync(path.join(dir, 'daemon.json'), '{}')
    fs.chmodSync(path.join(dir, 'daemon.json'), 0o000)
    expect(() => discoverDaemon()).toThrow(/0600/)
    expect(() => discoverDaemon()).not.toThrow(/EACCES/)
  })

  it('refuses an expired token file', () => {
    writeToken(dir, BRIDGE, 1)
    fs.writeFileSync(path.join(dir, 'daemon.json'), '{}')
    fs.chmodSync(path.join(dir, 'daemon.json'), 0o000)
    expect(() => discoverDaemon()).toThrow(/过期/)
  })

  it('a valid token file with no state file names the missing daemon', () => {
    writeToken(dir, BRIDGE, FUTURE)
    expect(() => discoverDaemon()).toThrow(/找不到运行中的 agent24d/)
  })

  it('A24_BASE_URL overrides the port and still requires the token file', () => {
    writeToken(dir, BRIDGE, FUTURE)
    process.env.A24_BASE_URL = 'http://127.0.0.1:9/'
    process.env.A24_TOKEN = 'legacy-secret'
    expect(discoverDaemon()).toEqual({ base: 'http://127.0.0.1:9', token: BRIDGE })
    process.env.A24_BASE_URL = 'not a url'
    expect(() => discoverDaemon()).toThrow(/valid URL/)
    process.env.A24_BASE_URL = 'ftp://127.0.0.1:9'
    expect(() => discoverDaemon()).toThrow(/http/)
    process.env.A24_BASE_URL = 'https://127.0.0.1:9'
    expect(() => discoverDaemon()).toThrow(/http/)
    process.env.A24_BASE_URL = 'http://example.com:9'
    expect(() => discoverDaemon()).toThrow(/127\.0\.0\.1/)
    process.env.A24_BASE_URL = 'http://127.0.0.2:9'
    expect(() => discoverDaemon()).toThrow(/127\.0\.0\.1/)
    process.env.A24_BASE_URL = 'http://[::1]:9/'
    expect(discoverDaemon()).toEqual({ base: 'http://[::1]:9', token: BRIDGE })
    process.env.A24_BASE_URL = 'http://localhost:9'
    expect(discoverDaemon()).toEqual({ base: 'http://localhost:9', token: BRIDGE })
  })

  it('refuses a token file that is a symlink', () => {
    const file = writeToken(dir, BRIDGE, FUTURE)
    const real = `${file}.real`
    fs.renameSync(file, real)
    fs.chmodSync(real, 0o600)
    fs.symlinkSync(real, file)
    expect(() => discoverDaemon()).toThrow(/符号链接/)
  })

  it('refuses when dev is a symlink', () => {
    const real = path.join(dir, 'real-dev')
    fs.mkdirSync(real)
    fs.chmodSync(real, 0o700)
    const file = path.join(real, 'wechat-bridge.token')
    fs.writeFileSync(file, JSON.stringify({ token: BRIDGE, expires_at: FUTURE }))
    fs.chmodSync(file, 0o600)
    fs.symlinkSync(real, path.join(dir, 'dev'))
    expect(() => discoverDaemon()).toThrow(/真实目录/)
  })
})
