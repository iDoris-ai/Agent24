import { describe, expect, it } from 'vitest'
import * as fs from 'node:fs'
import * as os from 'node:os'
import * as path from 'node:path'
import { fileURLToPath } from 'node:url'
import { daemonBearer, parseDaemonState, readDaemonState, requireDaemonBearer, DaemonStateError } from './index'

interface Case {
  name: string
  json?: unknown
  text?: string
  expect: { outcome: 'legacy' | 'capabilities' | 'rejected'; bearer?: string; reason?: string }
}
const fixturePath = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../../protocol/fixtures/daemon-state/cases.json',
)
const cases = (JSON.parse(fs.readFileSync(fixturePath, 'utf8')) as { cases: Case[] }).cases
const rawOf = (c: Case): string => c.text ?? JSON.stringify(c.json)

describe('daemon-state: shared cross-language fixture', () => {
  it('has every shape the acceptance names', () => {
    const outcomes = new Set(cases.map((c) => c.expect.outcome))
    expect([...outcomes].sort()).toEqual(['capabilities', 'legacy', 'rejected'])
    expect(cases.some((c) => c.name.includes('unknown auth_mode'))).toBe(true)
    expect(cases.some((c) => c.name.includes('token missing'))).toBe(true)
  })

  for (const c of cases) {
    it(c.name, () => {
      const r = parseDaemonState(rawOf(c))
      if (c.expect.outcome === 'rejected') {
        expect(r.ok).toBe(false)
        if (!r.ok) expect(r.reason).toBe(c.expect.reason)
        return
      }
      expect(r.ok).toBe(true)
      if (!r.ok) return
      const b = daemonBearer(r.state)
      if (c.expect.outcome === 'legacy') expect(b).toEqual({ ok: true, token: c.expect.bearer })
      else {
        expect(r.state.authMode).toBe('capabilities')
        expect(b.ok).toBe(false)
      }
    })
  }
})

describe('daemon-state: fields and files', () => {
  it('maps the optional fields', () => {
    const r = parseDaemonState(
      JSON.stringify({ port: 1, pid: 2, version: 'v', generation: 'g', auth_mode: 'capabilities', protocol_version: 3, enroll_endpoint: '/e', startup_state: 'docked' }),
    )
    expect(r).toMatchObject({ ok: true, state: { port: 1, pid: 2, protocolVersion: 3, enrollEndpoint: '/e', startupState: 'docked', token: '' } })
  })

  it('error messages never echo field values (the file holds a bearer)', () => {
    const r = parseDaemonState(JSON.stringify({ port: 1, pid: 2, version: 'v', token: 's3cr3t', auth_mode: 'nope' }))
    expect(JSON.stringify(r)).not.toContain('s3cr3t')
    expect(JSON.stringify(r)).not.toContain('nope')
  })

  it('readDaemonState: absent file is "unreadable"; requireDaemonBearer returns null for it and throws for the rest', () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-ds-'))
    try {
      const file = path.join(dir, 'daemon.json')
      expect(readDaemonState(file)).toMatchObject({ ok: false, reason: 'unreadable' })
      expect(requireDaemonBearer(file)).toBeNull()
      fs.writeFileSync(file, JSON.stringify({ port: 9, pid: 1, version: 'v', generation: 'g', auth_mode: 'capabilities' }))
      expect(() => requireDaemonBearer(file)).toThrow(DaemonStateError)
      fs.writeFileSync(file, JSON.stringify({ port: 9, pid: 1, version: 'v', token: 't' }))
      expect(requireDaemonBearer(file)).toEqual({ port: 9, token: 't' })
    } finally {
      fs.rmSync(dir, { recursive: true, force: true })
    }
  })

  it('a refused record that still names port and pid carries them as endpoint (never the token)', () => {
    const r = parseDaemonState(JSON.stringify({ port: 7, pid: 8, version: 'v', token: 's3cr3t', auth_mode: 'future_mode' }))
    expect(r).toMatchObject({ ok: false, reason: 'invalid', endpoint: { port: 7, pid: 8 } })
    expect(JSON.stringify(r)).not.toContain('s3cr3t')
    expect(parseDaemonState('{not json')).not.toHaveProperty('endpoint')
    expect(parseDaemonState(JSON.stringify({ pid: 8, token: 't', version: 'v' }))).not.toHaveProperty('endpoint')
  })

  it('daemonBearer re-validates like Rust bearer_token(): a mutated state yields no bearer', () => {
    const r = parseDaemonState(JSON.stringify({ port: 1, pid: 2, version: 'v', token: 't' }))
    if (!r.ok) throw new Error('fixture')
    expect(daemonBearer(r.state)).toEqual({ ok: true, token: 't' })
    expect(daemonBearer({ ...r.state, enrollEndpoint: '/e' }).ok).toBe(false)
    expect(daemonBearer({ ...r.state, authMode: 'capabilities', generation: '' }).ok).toBe(false)
  })
})
