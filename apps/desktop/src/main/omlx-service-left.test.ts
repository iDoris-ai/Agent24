import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { launchAgentPlistPath, markServiceLeft, serviceDaemonUp, serviceLeftBehind, serviceLeftPath, SERVICE_LEFT_FORMAT } from './omlx-service-left'

// PR#1 F1 follow-up (ruling 2026-11-13-pr1-remaining-lows-gate-f, L-a).
describe('the plist a service stop left behind (omlx-service-left)', () => {
  let dir = ''
  let file = ''
  let plist = ''
  const down = () => false
  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'svc-left-'))
    file = serviceLeftPath(path.join(dir, 'data'))
    plist = path.join(dir, 'ai.auraai.agent24.plist')
    fs.writeFileSync(plist, '<plist/>')
  })
  afterEach(() => {
    fs.rmSync(dir, { recursive: true, force: true })
    vi.restoreAllMocks()
  })

  it('records the plist that is still there, owner-only, and a quit then reads it as left behind', () => {
    markServiceLeft(file, plist)
    expect(fs.statSync(file).mode & 0o777).toBe(0o600)
    expect((JSON.parse(fs.readFileSync(file, 'utf8')) as { format: string }).format).toBe(SERVICE_LEFT_FORMAT)
    expect(serviceLeftBehind(file, plist, down)).toBe(true)
    // It lasts across quits and launches while nothing changed.
    expect(serviceLeftBehind(file, plist, down)).toBe(true)
  })

  it('records nothing when the plist is already gone (only the daemon copy could not be removed)', () => {
    fs.rmSync(plist)
    markServiceLeft(file, plist)
    expect(fs.existsSync(file)).toBe(false)
    markServiceLeft(file, null)
    expect(fs.existsSync(file)).toBe(false)
  })

  it('is not left behind without a record', () => {
    expect(serviceLeftBehind(file, plist, down)).toBe(false)
  })

  it('ends, and clears the record, once the plist is gone', () => {
    markServiceLeft(file, plist)
    fs.rmSync(plist)
    expect(serviceLeftBehind(file, plist, down)).toBe(false)
    expect(fs.existsSync(file)).toBe(false)
  })

  it('ends, and clears the record, once an install wrote the plist again in place (service.rs std::fs::write)', () => {
    markServiceLeft(file, plist)
    const ino = fs.statSync(plist).ino
    // Same inode, a later mtime (set explicitly: a write in the same tick may keep it).
    fs.writeFileSync(plist, '<plist>new</plist>')
    const later = new Date(fs.statSync(plist).mtimeMs + 5_000)
    fs.utimesSync(plist, later, later)
    expect(fs.statSync(plist).ino).toBe(ino)
    expect(serviceLeftBehind(file, plist, down)).toBe(false)
    expect(fs.existsSync(file)).toBe(false)
  })

  it('ends, and clears the record, once an install wrote the plist anew as another file', () => {
    markServiceLeft(file, plist)
    const temp = `${plist}.tmp`
    fs.writeFileSync(temp, '<plist>new</plist>')
    fs.renameSync(temp, plist)
    expect(serviceLeftBehind(file, plist, down)).toBe(false)
    expect(fs.existsSync(file)).toBe(false)
  })

  it('ends, and clears the record, once a service daemon is up again (launchd at the next login)', () => {
    markServiceLeft(file, plist)
    expect(serviceLeftBehind(file, plist, () => true)).toBe(false)
    expect(fs.existsSync(file)).toBe(false)
    // A look that throws reads as up: the service keeps its oMLX.
    markServiceLeft(file, plist)
    expect(serviceLeftBehind(file, plist, () => { throw new Error('read') })).toBe(false)
  })

  it('reads an unreadable or foreign record as not left behind, and clears it', () => {
    fs.mkdirSync(path.dirname(file), { recursive: true })
    for (const body of ['{not json', JSON.stringify({ format: 'other', version: 1, plist: { ino: 1, mtimeMs: 1 } })]) {
      fs.writeFileSync(file, body)
      expect(serviceLeftBehind(file, plist, down), body).toBe(false)
      expect(fs.existsSync(file)).toBe(false)
    }
  })

  it('never throws on an unwritable data directory', () => {
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    const blocked = path.join(dir, 'blocked')
    fs.writeFileSync(blocked, 'a file, not a directory')
    expect(() => markServiceLeft(serviceLeftPath(blocked), plist)).not.toThrow()
  })

  it('reads a service daemon as up only for a live, not-own daemon with a socket or a refused record', () => {
    const alive = (pid: number) => pid !== 4404
    const own = (pid: number) => pid === 4401
    const svc = { pid: 4400, enrollEndpoint: '/s.sock' }
    expect(serviceDaemonUp(svc, own, alive)).toBe(true)
    // This app's own child daemon is not the service.
    expect(serviceDaemonUp({ ...svc, pid: 4401 }, own, alive)).toBe(false)
    // A stale record: its pid is gone.
    expect(serviceDaemonUp({ ...svc, pid: 4404 }, own, alive)).toBe(false)
    // A legacy record without a socket is not the service's.
    expect(serviceDaemonUp({ pid: 4400 }, own, alive)).toBe(false)
    // A record the strict parser refused (a newer daemon) still names a live daemon.
    expect(serviceDaemonUp({ pid: 4400, rejected: 'version 9' }, own, alive)).toBe(true)
    expect(serviceDaemonUp({ pid: 4404, rejected: 'version 9' }, own, alive)).toBe(false)
    expect(serviceDaemonUp(null, own, alive)).toBe(false)
  })

  it('names the same plist as backend-manager.ts launchAgentPlistExists', () => {
    expect(launchAgentPlistPath({ HOME: '/Users/u' })).toBe('/Users/u/Library/LaunchAgents/ai.auraai.agent24.plist')
    expect(launchAgentPlistPath({})).toBeNull()
    const bm = fs.readFileSync(path.join(__dirname, 'backend-manager.ts'), 'utf8')
    expect(bm).toContain("path.join(home, 'Library', 'LaunchAgents', 'ai.auraai.agent24.plist')")
  })
})
