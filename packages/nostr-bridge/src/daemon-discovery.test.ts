import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { discoverDaemon } from './config.js'

// The shared daemon.json fixture (protocol/fixtures/daemon-state): data-dir,
// desktop, documents-accept and both bridges run this same file.
interface Case {
  name: string
  json?: unknown
  text?: string
  expect: { outcome: 'legacy' | 'capabilities' | 'rejected'; bearer?: string }
}
const fixture = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '..', 'protocol', 'fixtures', 'daemon-state', 'cases.json')
const cases = (JSON.parse(fs.readFileSync(fixture, 'utf8')) as { cases: Case[] }).cases

describe('discoverDaemon (single daemon.json parser)', () => {
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
      fs.writeFileSync(path.join(dir, 'daemon.json'), c.text ?? JSON.stringify(c.json))
      if (c.expect.outcome === 'legacy') {
        const ep = discoverDaemon()
        expect(ep?.token).toBe(c.expect.bearer)
        expect(ep?.base).toMatch(/^http:\/\/127\.0\.0\.1:\d+$/)
      } else {
        // capabilities / invalid / unknown auth_mode / missing token: loud, never an empty bearer
        expect(() => discoverDaemon()).toThrow(/daemon\.json/)
      }
    })
  }

  it('capabilities mode names the reason (no token is shared)', () => {
    fs.writeFileSync(path.join(dir, 'daemon.json'), JSON.stringify({ port: 1, pid: 2, version: 'v', generation: 'g', auth_mode: 'capabilities' }))
    expect(() => discoverDaemon()).toThrow(/capabilities/)
  })

  it('no state file → null (nothing running), unchanged', () => {
    expect(discoverDaemon()).toBeNull()
  })

  it('A24_BASE_URL + non-empty A24_TOKEN still overrides the file (tests / non-standard setups)', () => {
    process.env.A24_BASE_URL = 'http://127.0.0.1:9/'
    process.env.A24_TOKEN = 't'
    expect(discoverDaemon()).toEqual({ base: 'http://127.0.0.1:9', token: 't' })
  })

  it('the override without a usable token or URL fails at startup (no empty-bearer run)', () => {
    process.env.A24_BASE_URL = 'http://127.0.0.1:9'
    expect(() => discoverDaemon()).toThrow(/A24_TOKEN/)
    process.env.A24_TOKEN = ''
    expect(() => discoverDaemon()).toThrow(/A24_TOKEN/)
    process.env.A24_TOKEN = '   '
    expect(() => discoverDaemon()).toThrow(/A24_TOKEN/)
    process.env.A24_TOKEN = 't'
    process.env.A24_BASE_URL = 'not a url'
    expect(() => discoverDaemon()).toThrow(/valid URL/)
    process.env.A24_BASE_URL = 'ftp://127.0.0.1:9'
    expect(() => discoverDaemon()).toThrow(/http/)
  })
})
