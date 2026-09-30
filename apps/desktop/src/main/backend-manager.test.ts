import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { EventEmitter } from 'node:events'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import type { ChildProcess } from 'node:child_process'
import {
  deriveStatus,
  readExternalDaemonState,
  BackendManager,
  getBackendEndpoint,
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

  beforeEach(() => {
    originalHome = process.env['HOME']
    tmpHome = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-daemon-state-'))
    process.env['HOME'] = tmpHome
  })

  afterEach(() => {
    if (originalHome === undefined) delete process.env['HOME']
    else process.env['HOME'] = originalHome
    fs.rmSync(tmpHome, { recursive: true, force: true })
  })

  function writeState(json: unknown): void {
    const dir = path.join(tmpHome, '.agent24')
    fs.mkdirSync(dir, { recursive: true })
    fs.writeFileSync(path.join(dir, 'daemon.json'), JSON.stringify(json))
  }

  it('parses a well-formed daemon.json (mirrors the Rust DaemonState shape)', () => {
    writeState({ port: 4321, token: 'abc', pid: 999, version: '0.5.0', generation: 'g1', auth_mode: 'legacy_single_token' })
    expect(readExternalDaemonState()).toEqual({ port: 4321, token: 'abc', pid: 999 })
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

  it('treats a capability-mode record (empty token) as reusable too', () => {
    writeState({ port: 1, pid: 2, auth_mode: 'capabilities', generation: 'g1' })
    expect(readExternalDaemonState()).toEqual({ port: 1, token: '', pid: 2 })
  })

  it('returns null when HOME is unset — mirrors the Rust state_dir() contract', () => {
    delete process.env['HOME']
    expect(readExternalDaemonState()).toBeNull()
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
      read: () => ({ port: 5555, token: 'tok', pid: 4242 }),
      checkHealth: vi.fn().mockResolvedValue(true),
    }
    const manager = new BackendManager({ external, spawnProcess })

    await manager.start()

    expect(spawnProcess).not.toHaveBeenCalled()
    expect(manager.isExternalDaemon()).toBe(true)
    expect(manager.externalDaemonPid()).toBe(4242)
    expect(getBackendEndpoint()).toEqual({ port: 5555, token: 'tok' })
    expect(manager.status()).toBe('running')

    manager.stop()
  })

  it('daemon.json present but unhealthy → falls back to spawning its own', async () => {
    const child = fakeChild(222)
    const spawnProcess = vi.fn(() => child)
    const external: ExternalDaemonSource = {
      read: () => ({ port: 5555, token: 'tok', pid: 4242 }),
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
      read: () => ({ port: 5555, token: 'tok', pid: 4242 }),
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
      read: () => ({ port: 5555, token: 'tok', pid: 4242 }),
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
      read: () => ({ port: 5555, token: 'tok', pid: 4242 }),
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
      read: () => (externalAvailable ? { port: 5555, token: 'tok', pid: 4242 } : null),
      checkHealth: vi.fn((endpoint: { port: number; token: string } | null) => {
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
    expect(getBackendEndpoint()).toEqual({ port: 5555, token: 'tok' })
    expect(spawnProcess).toHaveBeenCalledTimes(2) // no third spawn
    expect(children[0].kill).toHaveBeenCalledWith('SIGTERM')
    expect(children[1].kill).toHaveBeenCalledWith('SIGTERM')

    manager.stop()
  })
})
