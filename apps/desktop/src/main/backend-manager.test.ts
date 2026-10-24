import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { EventEmitter } from 'node:events'
import { PassThrough } from 'node:stream'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import type { ChildProcess } from 'node:child_process'
import {
  deriveStatus,
  daemonStatePath,
  readExternalDaemonState,
  BackendManager,
  getBackendEndpoint,
  daemonConnection,
  selectedAuthMode,
  type DaemonAuthMode,
  type ExternalDaemonSource,
} from './backend-manager'

describe('deriveStatus (F1b tray daemon status)', () => {
  it('a user-initiated stop pins `stopped`, even if a stale endpoint lingers', () => {
    expect(deriveStatus({ userStopped: true, hasEndpoint: false, healthy: false })).toBe('stopped')
    // must NOT read as running/starting — a manual stop is authoritative
    expect(deriveStatus({ userStopped: true, hasEndpoint: true, healthy: true })).toBe('stopped')
  })

  it('a healthy endpoint is `running`', () => {
    expect(deriveStatus({ userStopped: false, hasEndpoint: true, healthy: true })).toBe('running')
  })

  it('spawned-but-not-ready (endpoint present, not yet healthy) is `starting`', () => {
    expect(deriveStatus({ userStopped: false, hasEndpoint: true, healthy: false })).toBe('starting')
  })

  it('no endpoint yet (or briefly between respawns), not user-stopped, is `starting`', () => {
    expect(deriveStatus({ userStopped: false, hasEndpoint: false, healthy: false })).toBe('starting')
  })
})

describe('readExternalDaemonState (DEP-A5 daemon.json discovery)', () => {
  let tmpHome: string
  let originalHome: string | undefined
  let originalDataDir: string | undefined

  beforeEach(() => {
    originalHome = process.env['HOME']
    originalDataDir = process.env['AGENT24_DATA_DIR']
    tmpHome = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-daemon-state-'))
    process.env['HOME'] = tmpHome
    delete process.env['AGENT24_DATA_DIR']
  })

  afterEach(() => {
    if (originalHome === undefined) delete process.env['HOME']
    else process.env['HOME'] = originalHome
    if (originalDataDir === undefined) delete process.env['AGENT24_DATA_DIR']
    else process.env['AGENT24_DATA_DIR'] = originalDataDir
    fs.rmSync(tmpHome, { recursive: true, force: true })
  })

  function writeState(json: unknown): void {
    const dir = path.join(tmpHome, '.agent24')
    fs.mkdirSync(dir, { recursive: true })
    fs.writeFileSync(path.join(dir, 'daemon.json'), JSON.stringify(json))
  }

  it('parses a well-formed daemon.json (mirrors the Rust DaemonState shape)', () => {
    writeState({ port: 4321, token: 'abc', pid: 999, version: '0.5.0', generation: 'g1', auth_mode: 'legacy_single_token' })
    expect(readExternalDaemonState()).toEqual({ port: 4321, token: 'abc', pid: 999, authMode: 'legacy' })
  })

  it('returns null when the file is absent', () => {
    expect(readExternalDaemonState()).toBeNull()
  })

  it('returns null on malformed JSON rather than throwing', () => {
    const dir = path.join(tmpHome, '.agent24')
    fs.mkdirSync(dir, { recursive: true })
    fs.writeFileSync(path.join(dir, 'daemon.json'), '{not json')
    expect(readExternalDaemonState()).toBeNull()
  })

  it('returns null when required fields are missing or mistyped', () => {
    writeState({ token: 'abc', pid: 1 }) // no port
    expect(readExternalDaemonState()).toBeNull()
  })

  it('marks a capability-mode record, which shares no token', () => {
    writeState({ port: 1, pid: 2, auth_mode: 'capabilities', generation: 'g1' })
    expect(readExternalDaemonState()).toEqual({ port: 1, token: '', pid: 2, authMode: 'capabilities' })
  })

  it('treats an unknown auth_mode as not shareable, and a record without one as legacy', () => {
    writeState({ port: 1, token: 'x', pid: 2, auth_mode: 'something_new' })
    expect(readExternalDaemonState()?.authMode).toBe('capabilities')
    writeState({ port: 1, token: 'x', pid: 2 })
    expect(readExternalDaemonState()?.authMode).toBe('legacy')
  })

  it('returns null when HOME is unset — mirrors the Rust state_dir() contract', () => {
    delete process.env['HOME']
    expect(readExternalDaemonState()).toBeNull()
  })

  it('daemonStatePath() defaults to $HOME/.agent24/daemon.json when AGENT24_DATA_DIR is unset', () => {
    expect(daemonStatePath()).toBe(path.join(tmpHome, '.agent24', 'daemon.json'))
  })

  it('daemonStatePath() uses AGENT24_DATA_DIR as-is (no .agent24 suffix) — MAINT-03', () => {
    const dataDir = path.join(tmpHome, 'isolated')
    process.env['AGENT24_DATA_DIR'] = dataDir
    expect(daemonStatePath()).toBe(path.join(dataDir, 'daemon.json'))
    delete process.env['HOME'] // HOME is not needed once the override is set
    expect(daemonStatePath()).toBe(path.join(dataDir, 'daemon.json'))
  })

  it('daemonStatePath() ignores an empty or relative AGENT24_DATA_DIR, like the Rust side', () => {
    process.env['AGENT24_DATA_DIR'] = ''
    expect(daemonStatePath()).toBe(path.join(tmpHome, '.agent24', 'daemon.json'))
    process.env['AGENT24_DATA_DIR'] = 'relative/dir'
    expect(daemonStatePath()).toBe(path.join(tmpHome, '.agent24', 'daemon.json'))
  })

  it('reads daemon.json from AGENT24_DATA_DIR, not $HOME/.agent24, when set', () => {
    writeState({ port: 1111, token: 'home', pid: 1 })
    const dataDir = path.join(tmpHome, 'isolated')
    fs.mkdirSync(dataDir, { recursive: true })
    fs.writeFileSync(path.join(dataDir, 'daemon.json'), JSON.stringify({ port: 2222, token: 'iso', pid: 2 }))
    process.env['AGENT24_DATA_DIR'] = dataDir
    expect(readExternalDaemonState()).toEqual({ port: 2222, token: 'iso', pid: 2, authMode: 'legacy' })
  })
})

describe('BackendManager daemon reuse (DEP-A5)', () => {
  // Minimal EventEmitter-based ChildProcess double — enough for wireChild()
  // (guards on `.stdout`/`.stderr`, listens via `.on`) without touching a
  // real binary. `stdout`/`stderr` are null so wireChild() skips readline
  // wiring entirely (this test never needs to simulate a ready line).
  function fakeChild(pid: number): ChildProcess {
    const ee = new EventEmitter() as unknown as ChildProcess
    Object.assign(ee, { pid, stdout: null, stderr: null, kill: vi.fn().mockReturnValue(true) })
    return ee
  }

  it('daemon.json present and healthy → reuses it, never spawns', async () => {
    const spawnProcess = vi.fn(() => fakeChild(111))
    const external: ExternalDaemonSource = {
      read: () => ({ port: 5555, token: 'tok', pid: 4242, authMode: 'legacy' }),
      checkHealth: vi.fn().mockResolvedValue(true),
    }
    const manager = new BackendManager({ external, spawnProcess })

    await manager.start()

    expect(spawnProcess).not.toHaveBeenCalled()
    expect(manager.isExternalDaemon()).toBe(true)
    expect(manager.externalDaemonPid()).toBe(4242)
    expect(getBackendEndpoint()).toEqual({ port: 5555 })
    expect(daemonConnection()).toEqual({ port: 5555, authHeaders: { Authorization: 'Bearer tok' } })
    expect(external.checkHealth).toHaveBeenCalledWith({ port: 5555 })
    expect(manager.status()).toBe('running')

    manager.stop()
  })

  it('daemon.json present but unhealthy → falls back to spawning its own', async () => {
    const child = fakeChild(222)
    const spawnProcess = vi.fn(() => child)
    const external: ExternalDaemonSource = {
      read: () => ({ port: 5555, token: 'tok', pid: 4242, authMode: 'legacy' }),
      checkHealth: vi.fn().mockResolvedValue(false),
    }
    const manager = new BackendManager({ external, spawnProcess })

    await manager.start()

    expect(spawnProcess).toHaveBeenCalledTimes(1)
    expect(manager.isExternalDaemon()).toBe(false)

    manager.stop()
  })

  it('no daemon.json at all → falls back to spawning its own', async () => {
    const spawnProcess = vi.fn(() => fakeChild(333))
    const external: ExternalDaemonSource = { read: () => null, checkHealth: vi.fn() }
    const manager = new BackendManager({ external, spawnProcess })

    await manager.start()

    expect(spawnProcess).toHaveBeenCalledTimes(1)
    expect(external.checkHealth).not.toHaveBeenCalled()
    expect(manager.isExternalDaemon()).toBe(false)

    manager.stop()
  })

  it('stopDaemon() on a reused external daemon disconnects but never kills it', async () => {
    const spawnProcess = vi.fn(() => fakeChild(111))
    const external: ExternalDaemonSource = {
      read: () => ({ port: 5555, token: 'tok', pid: 4242, authMode: 'legacy' }),
      checkHealth: vi.fn().mockResolvedValue(true),
    }
    const manager = new BackendManager({ external, spawnProcess })
    await manager.start()
    expect(getBackendEndpoint()).not.toBeNull()

    manager.stopDaemon()

    // Never spawned anything to begin with, so there is nothing our own
    // kill path could have touched — the assertion that matters is that
    // stopDaemon() didn't reach for spawnProcess/kill at all, and that the
    // manager cleanly forgot the connection instead.
    expect(spawnProcess).not.toHaveBeenCalled()
    expect(manager.isExternalDaemon()).toBe(false)
    expect(getBackendEndpoint()).toBeNull()
    expect(manager.status()).toBe('stopped')

    manager.stop()
  })

  it('stop() (app quit) on a reused external daemon disconnects but never kills it', async () => {
    const spawnProcess = vi.fn(() => fakeChild(111))
    const external: ExternalDaemonSource = {
      read: () => ({ port: 5555, token: 'tok', pid: 4242, authMode: 'legacy' }),
      checkHealth: vi.fn().mockResolvedValue(true),
    }
    const manager = new BackendManager({ external, spawnProcess })
    await manager.start()

    manager.stop()

    expect(spawnProcess).not.toHaveBeenCalled()
    expect(manager.isExternalDaemon()).toBe(false)
    expect(getBackendEndpoint()).toBeNull()
  })

  it('restart() on a reused external daemon disconnects (never kills it) and spawns its own', async () => {
    const child = fakeChild(111)
    const spawnProcess = vi.fn(() => child)
    const external: ExternalDaemonSource = {
      read: () => ({ port: 5555, token: 'tok', pid: 4242, authMode: 'legacy' }),
      checkHealth: vi.fn().mockResolvedValue(true),
    }
    const manager = new BackendManager({ external, spawnProcess })
    await manager.start()
    expect(manager.isExternalDaemon()).toBe(true)

    manager.restart()

    expect(spawnProcess).toHaveBeenCalledTimes(1)
    expect(manager.isExternalDaemon()).toBe(false)

    manager.stop()
  })
})

describe('BackendManager near-simultaneous startup recovery (PR #602 review, Medium finding)', () => {
  // Mirrors HEALTH_INTERVAL_MS / MAX_HEALTH_FAILURES from backend-manager.ts
  // (not exported — duplicated here deliberately so the test drives the real
  // setInterval-based health loop rather than calling a private method).
  const HEALTH_INTERVAL_MS = 5_000
  const MAX_HEALTH_FAILURES = 3

  function fakeChild(pid: number): ChildProcess {
    const ee = new EventEmitter() as unknown as ChildProcess
    Object.assign(ee, { pid, stdout: null, stderr: null, kill: vi.fn().mockReturnValue(true) })
    return ee
  }

  afterEach(() => {
    vi.useRealTimers()
  })

  it('re-detects an external daemon that only becomes visible after this app already spawned its own, instead of looping self-respawns forever', async () => {
    vi.useFakeTimers()

    // Scenario: CLI (`agent24 daemon start`) and the desktop app start at
    // nearly the same time. At desktop start(), the CLI hasn't finished
    // writing daemon.json yet, so tryReuseExternal() misses it (step 1) and
    // the desktop spawns its own child (step 1). That child loses the Rust
    // singleton lock and never binds a port, so it never reports healthy
    // (step 2) — health checks against it always resolve false. Some time
    // later the CLI's daemon.json is visible and its daemon is healthy
    // (step 3).
    let externalAvailable = false
    const children: ChildProcess[] = []
    const spawnProcess = vi.fn(() => {
      const child = fakeChild(1000 + children.length)
      children.push(child)
      return child
    })
    const external: ExternalDaemonSource = {
      read: () => (externalAvailable ? { port: 5555, token: 'tok', pid: 4242, authMode: 'legacy' } : null),
      checkHealth: vi.fn((endpoint: { port: number } | null) => {
        // The external daemon's own endpoint is only healthy once it
        // becomes available; a self-spawned child's endpoint (currentEndpoint
        // stays null here since stdout is null / no ready line ever arrives,
        // exactly like the real lost-singleton-lock failure) never is.
        if (endpoint && endpoint.port === 5555) return Promise.resolve(externalAvailable)
        return Promise.resolve(false)
      }),
    }
    const manager = new BackendManager({ external, spawnProcess })

    await manager.start()
    expect(spawnProcess).toHaveBeenCalledTimes(1) // step 1: missed external, spawned its own

    // Drive past MAX_HEALTH_FAILURES consecutive failed health checks on the
    // self-spawned (never-healthy) child (step 2).
    for (let i = 0; i < MAX_HEALTH_FAILURES; i++) {
      await vi.advanceTimersByTimeAsync(HEALTH_INTERVAL_MS)
    }
    // At the failure threshold the external daemon is still invisible, so
    // this pass falls back to spawning again — same as before the fix.
    expect(spawnProcess).toHaveBeenCalledTimes(2)

    // The CLI's daemon.json is now visible and its daemon is healthy (step 3).
    externalAvailable = true

    // Drive past the failure threshold again on the second self-spawned
    // (still never-healthy) child.
    for (let i = 0; i < MAX_HEALTH_FAILURES; i++) {
      await vi.advanceTimersByTimeAsync(HEALTH_INTERVAL_MS)
    }

    // step 4: should now be connected to the external daemon, not spawning a
    // third self-managed child, and the earlier self-spawned children should
    // have been killed rather than left running alongside the external one.
    expect(manager.isExternalDaemon()).toBe(true)
    expect(getBackendEndpoint()).toEqual({ port: 5555 })
    expect(spawnProcess).toHaveBeenCalledTimes(2) // no third spawn
    expect(children[0].kill).toHaveBeenCalledWith('SIGTERM')
    expect(children[1].kill).toHaveBeenCalledWith('SIGTERM')

    manager.stop()
  })
})

describe('daemon credential stays out of the endpoint (KB-HOST-01 PR-A1)', () => {
  const savedBackend = process.env['AGENT24_BACKEND']

  afterEach(() => {
    if (savedBackend === undefined) delete process.env['AGENT24_BACKEND']
    else process.env['AGENT24_BACKEND'] = savedBackend
  })

  function childAnnouncing(ready: Record<string, unknown>): ChildProcess {
    const stdout = new PassThrough()
    const ee = new EventEmitter() as unknown as ChildProcess
    Object.assign(ee, { pid: 7, stdout, stderr: null, kill: vi.fn().mockReturnValue(true) })
    setImmediate(() => stdout.write(JSON.stringify({ type: 'ready', ...ready }) + '\n'))
    return ee
  }

  async function startWith(ready: Record<string, unknown>, authMode: DaemonAuthMode = 'legacy'): Promise<BackendManager> {
    const external: ExternalDaemonSource = { read: () => null, checkHealth: vi.fn().mockResolvedValue(true) }
    const manager = new BackendManager({ external, authMode, spawnProcess: () => childAnnouncing(ready) })
    await manager.start()
    await vi.waitFor(() => expect(getBackendEndpoint()).not.toBeNull())
    return manager
  }

  it('the rust ready token becomes a bearer header, never part of the endpoint', async () => {
    delete process.env['AGENT24_BACKEND']
    const manager = await startWith({ port: 4100, token: 'secret' })
    expect(getBackendEndpoint()).toEqual({ port: 4100 })
    expect(daemonConnection()).toEqual({ port: 4100, authHeaders: { Authorization: 'Bearer secret' } })
    manager.stop()
  })

  it('a rust daemon that announced no token gets no connection: nothing is sent without a credential', async () => {
    delete process.env['AGENT24_BACKEND']
    const manager = await startWith({ port: 4101 })
    expect(getBackendEndpoint()).toEqual({ port: 4101 })
    expect(daemonConnection()).toBeNull()
    manager.stop()
  })

  it('the health tick probes the current port', async () => {
    delete process.env['AGENT24_BACKEND']
    const checkHealth = vi.fn().mockResolvedValue(true)
    const external: ExternalDaemonSource = { read: () => null, checkHealth }
    const manager = new BackendManager({ external, authMode: 'legacy', spawnProcess: () => childAnnouncing({ port: 4102, token: 't' }) })
    await manager.start()
    await vi.waitFor(() => expect(getBackendEndpoint()).not.toBeNull())
    await (manager as unknown as { tick(): Promise<void> }).tick()
    expect(checkHealth).toHaveBeenLastCalledWith({ port: 4102 })
    manager.stop()
  })

  it('no credential outlives its daemon: exit, error, restart and stop all clear it', async () => {
    delete process.env['AGENT24_BACKEND']
    const children: ChildProcess[] = []
    const external: ExternalDaemonSource = { read: () => null, checkHealth: vi.fn().mockResolvedValue(true) }
    const manager = new BackendManager({
      external,
      authMode: 'legacy',
      spawnProcess: () => {
        const child = childAnnouncing({ port: 4200 + children.length, token: `t${children.length}` })
        children.push(child)
        return child
      },
    })
    await manager.start()
    await vi.waitFor(() => expect(daemonConnection()).not.toBeNull())

    children[0].emit('exit', 1)
    expect(daemonConnection()).toBeNull()

    manager.restart()
    await vi.waitFor(() => expect(daemonConnection()?.port).toBe(4201))
    children[1].emit('error', new Error('spawn failed'))
    expect(daemonConnection()).toBeNull()

    manager.restart()
    expect(daemonConnection()).toBeNull()
    await vi.waitFor(() => expect(daemonConnection()?.port).toBe(4202))

    manager.stop()
    expect(daemonConnection()).toBeNull()
  })

  it('a dead external daemon\'s credential is gone before the fallback child is ready', async () => {
    let healthy = true
    const external: ExternalDaemonSource = {
      read: () => ({ port: 5558, token: 'external', pid: 4245, authMode: 'legacy' }),
      checkHealth: vi.fn(() => Promise.resolve(healthy)),
    }
    // The fallback child never announces readiness (no stdout).
    const silentChild = (): ChildProcess => {
      const ee = new EventEmitter() as unknown as ChildProcess
      Object.assign(ee, { pid: 9, stdout: null, stderr: null, kill: vi.fn().mockReturnValue(true) })
      return ee
    }
    const manager = new BackendManager({ external, spawnProcess: silentChild })
    await manager.start()
    expect(daemonConnection()).not.toBeNull()

    healthy = false
    const tick = (manager as unknown as { tick(): Promise<void> }).tick.bind(manager)
    for (let i = 0; i < 3; i++) await tick() // MAX_HEALTH_FAILURES
    expect(manager.isExternalDaemon()).toBe(false)
    expect(daemonConnection()).toBeNull()
    manager.stop()
  })

  it('the node mock takes no credential and is still reachable', async () => {
    process.env['AGENT24_BACKEND'] = 'node'
    const manager = await startWith({ port: 8765 })
    expect(daemonConnection()).toEqual({ port: 8765, authHeaders: {} })
    manager.stop()
  })

  it('a reused legacy record without a token gets no connection', async () => {
    const external: ExternalDaemonSource = {
      read: () => ({ port: 5556, token: '', pid: 4243, authMode: 'legacy' }),
      checkHealth: vi.fn().mockResolvedValue(true),
    }
    const manager = new BackendManager({ external, spawnProcess: vi.fn() })
    await manager.start()
    expect(getBackendEndpoint()).toEqual({ port: 5556 })
    expect(daemonConnection()).toBeNull()
    manager.stop()
  })
})

describe('capability-mode startup (KB-HOST-01 PR-A2)', () => {
  const savedBackend = process.env['AGENT24_BACKEND']

  afterEach(() => {
    if (savedBackend === undefined) delete process.env['AGENT24_BACKEND']
    else process.env['AGENT24_BACKEND'] = savedBackend
    vi.restoreAllMocks()
  })

  function childAnnouncing(ready: Record<string, unknown>): ChildProcess {
    const stdout = new PassThrough()
    const ee = new EventEmitter() as unknown as ChildProcess
    Object.assign(ee, { pid: 8, stdout, stderr: null, kill: vi.fn().mockReturnValue(true) })
    setImmediate(() => stdout.write(JSON.stringify({ type: 'ready', ...ready }) + '\n'))
    return ee
  }

  it('the rust daemon runs in capability mode except on Windows; the node mock stays legacy', () => {
    expect(selectedAuthMode('rust', 'darwin')).toBe('capabilities')
    expect(selectedAuthMode('rust', 'linux')).toBe('capabilities')
    expect(selectedAuthMode('rust', 'win32')).toBe('legacy')
    expect(selectedAuthMode('node', 'darwin')).toBe('legacy')
  })

  it('spawns in capability mode and takes the host token from the private ready line', async () => {
    delete process.env['AGENT24_BACKEND']
    const spawnProcess = vi.fn(() =>
      childAnnouncing({ port: 4300, auth_mode: 'capabilities', product_host_token: 'host', generation: 'g' }),
    )
    const external: ExternalDaemonSource = { read: () => null, checkHealth: vi.fn().mockResolvedValue(true) }
    const manager = new BackendManager({ external, spawnProcess, authMode: 'capabilities' })
    await manager.start()
    await vi.waitFor(() => expect(getBackendEndpoint()).not.toBeNull())
    expect(spawnProcess).toHaveBeenCalledWith('rust', 'capabilities')
    expect(getBackendEndpoint()).toEqual({ port: 4300 })
    expect(daemonConnection()).toEqual({ port: 4300, authHeaders: { Authorization: 'Bearer host' } })
    manager.stop()
  })

  it('a daemon that answers in the other mode gets no connection', async () => {
    delete process.env['AGENT24_BACKEND']
    const external: ExternalDaemonSource = { read: () => null, checkHealth: vi.fn().mockResolvedValue(true) }
    const asksCapabilities = new BackendManager({
      external,
      authMode: 'capabilities',
      spawnProcess: () => childAnnouncing({ port: 4301, token: 'legacy-token' }),
    })
    await asksCapabilities.start()
    await vi.waitFor(() => expect(getBackendEndpoint()).not.toBeNull())
    expect(daemonConnection()).toBeNull()
    asksCapabilities.stop()

    const asksLegacy = new BackendManager({
      external,
      authMode: 'legacy',
      spawnProcess: () => childAnnouncing({ port: 4302, auth_mode: 'capabilities', product_host_token: 'host', token: 'x' }),
    })
    await asksLegacy.start()
    await vi.waitFor(() => expect(getBackendEndpoint()).not.toBeNull())
    expect(daemonConnection()).toBeNull()
    asksLegacy.stop()
  })

  it('never reuses an external daemon in capability mode; spawns its own instead', async () => {
    const checkHealth = vi.fn().mockResolvedValue(true)
    const spawnProcess = vi.fn(() =>
      childAnnouncing({ port: 4303, auth_mode: 'capabilities', product_host_token: 'mine' }),
    )
    const external: ExternalDaemonSource = {
      read: () => ({ port: 5557, token: '', pid: 4244, authMode: 'capabilities' }),
      checkHealth,
    }
    const manager = new BackendManager({ external, spawnProcess, authMode: 'capabilities' })
    await manager.start()
    expect(checkHealth).not.toHaveBeenCalled()
    expect(manager.isExternalDaemon()).toBe(false)
    expect(spawnProcess).toHaveBeenCalledTimes(1)
    await vi.waitFor(() => expect(daemonConnection()?.port).toBe(4303))
    manager.stop()
  })

  it('a ready line is never echoed to the log, even when malformed', async () => {
    const log = vi.spyOn(console, 'log').mockImplementation(() => {})
    const external: ExternalDaemonSource = { read: () => null, checkHealth: vi.fn().mockResolvedValue(true) }
    const manager = new BackendManager({
      external,
      authMode: 'capabilities',
      spawnProcess: () => childAnnouncing({ port: 'not-a-number', product_host_token: 'leaky' }),
    })
    await manager.start()
    await new Promise((r) => setTimeout(r, 50))
    expect(getBackendEndpoint()).toBeNull()
    expect(log.mock.calls.flat().join(' ')).not.toContain('leaky')
    manager.stop()
  })
})
