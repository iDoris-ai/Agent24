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
  fetchDaemonHealth,
  readExternalDaemonState,
  BackendManager,
  selectedBackend,
  resolveNodeEntryFrom,
  resolveRustBinary,
  getBackendEndpoint,
  daemonCliEndpoint,
  daemonConnection,
  selectedAuthMode,
  type DaemonAuthMode,
  type ExternalDaemonSource,
  type ExternalDaemonState,
} from './backend-manager'
import { CLIENT_PROTOCOL_VERSION } from './protocol-compatibility'

describe('deriveStatus (F1b tray daemon status)', () => {
  it('a user-initiated stop pins `stopped`, even if a stale endpoint lingers', () => {
    expect(deriveStatus({ userStopped: true, hasEndpoint: false, healthy: false })).toBe('stopped')
    // must NOT read as running/starting — a manual stop is authoritative
    expect(deriveStatus({ userStopped: true, hasEndpoint: true, healthy: true })).toBe('stopped')
  })

  it('another host\'s daemon holding the data dir is `blocked`, below only a user stop', () => {
    expect(deriveStatus({ userStopped: false, hasEndpoint: false, healthy: false, foreignHost: true })).toBe('blocked')
    expect(deriveStatus({ userStopped: true, hasEndpoint: false, healthy: false, foreignHost: true })).toBe('stopped')
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

  it('keeps protocol_version when daemon.json has it, and leaves it off when the file is old', () => {
    writeState({ port: 4321, token: 'abc', pid: 999, protocol_version: 1 })
    expect(readExternalDaemonState()).toEqual({ port: 4321, token: 'abc', pid: 999, authMode: 'legacy', protocolVersion: 1 })
    writeState({ port: 4321, token: 'abc', pid: 999 })
    expect(readExternalDaemonState()?.protocolVersion).toBeUndefined()
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

/** A source whose reuse probe is its plain `checkHealth`, on this app's own protocol. */
function withProbe(src: Omit<ExternalDaemonSource, 'fetchHealth'>): ExternalDaemonSource {
  return {
    ...src,
    fetchHealth: async (endpoint) => ({
      ok: await src.checkHealth(endpoint),
      protocolVersion: CLIENT_PROTOCOL_VERSION,
    }),
  }
}

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

  it('退出发生在无 daemon 记录的启动 await 期间，不再 spawn 或启动健康轮询', async () => {
    vi.useFakeTimers()
    const spawnProcess = vi.fn(() => fakeChild(111))
    const external = withProbe({ read: () => null, checkHealth: vi.fn().mockResolvedValue(false) })
    const manager = new BackendManager({ external, spawnProcess })
    try {
      const starting = manager.start()
      manager.stop()
      await starting
      await vi.advanceTimersByTimeAsync(25_000)
      expect(spawnProcess).not.toHaveBeenCalled()
      expect(vi.getTimerCount()).toBe(0)
    } finally {
      manager.stop()
      vi.useRealTimers()
    }
  })

  it('daemon.json present and healthy → reuses it, never spawns', async () => {
    const spawnProcess = vi.fn(() => fakeChild(111))
    const external: ExternalDaemonSource = {
      read: () => ({ port: 5555, token: 'tok', pid: 4242, authMode: 'legacy', protocolVersion: CLIENT_PROTOCOL_VERSION }),
      checkHealth: vi.fn().mockResolvedValue(true),
      fetchHealth: vi.fn().mockResolvedValue({ ok: true, protocolVersion: CLIENT_PROTOCOL_VERSION }),
    }
    const manager = new BackendManager({ external, spawnProcess })

    await manager.start()

    expect(spawnProcess).not.toHaveBeenCalled()
    expect(manager.isExternalDaemon()).toBe(true)
    expect(manager.externalDaemonPid()).toBe(4242)
    expect(getBackendEndpoint()).toEqual({ port: 5555 })
    expect(daemonConnection()).toEqual({ port: 5555, authHeaders: { Authorization: 'Bearer tok' } })
    expect(external.fetchHealth).toHaveBeenCalledWith({ port: 5555 })
    expect(manager.status()).toBe('running')

    manager.stop()
  })

  it('daemon.json present but unhealthy → falls back to spawning its own', async () => {
    const child = fakeChild(222)
    const spawnProcess = vi.fn(() => child)
    const external: ExternalDaemonSource = {
      read: () => ({ port: 5555, token: 'tok', pid: 4242, authMode: 'legacy', protocolVersion: CLIENT_PROTOCOL_VERSION }),
      checkHealth: vi.fn().mockResolvedValue(false),
      fetchHealth: vi.fn().mockResolvedValue({ ok: false, protocolVersion: null }),
    }
    const manager = new BackendManager({ external, spawnProcess })

    await manager.start()

    expect(spawnProcess).toHaveBeenCalledTimes(1)
    expect(manager.isExternalDaemon()).toBe(false)

    manager.stop()
  })

  it('no daemon.json at all → falls back to spawning its own', async () => {
    const spawnProcess = vi.fn(() => fakeChild(333))
    const external: ExternalDaemonSource = {
      read: () => null,
      checkHealth: vi.fn(),
      fetchHealth: vi.fn(),
    }
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
      read: () => ({ port: 5555, token: 'tok', pid: 4242, authMode: 'legacy', protocolVersion: CLIENT_PROTOCOL_VERSION }),
      checkHealth: vi.fn().mockResolvedValue(true),
      fetchHealth: vi.fn().mockResolvedValue({ ok: true, protocolVersion: CLIENT_PROTOCOL_VERSION }),
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
      read: () => ({ port: 5555, token: 'tok', pid: 4242, authMode: 'legacy', protocolVersion: CLIENT_PROTOCOL_VERSION }),
      checkHealth: vi.fn().mockResolvedValue(true),
      fetchHealth: vi.fn().mockResolvedValue({ ok: true, protocolVersion: CLIENT_PROTOCOL_VERSION }),
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
      read: () => ({ port: 5555, token: 'tok', pid: 4242, authMode: 'legacy', protocolVersion: CLIENT_PROTOCOL_VERSION }),
      checkHealth: vi.fn().mockResolvedValue(true),
      fetchHealth: vi.fn().mockResolvedValue({ ok: true, protocolVersion: CLIENT_PROTOCOL_VERSION }),
    }
    const manager = new BackendManager({ external, spawnProcess })
    await manager.start()
    expect(manager.isExternalDaemon()).toBe(true)

    manager.restart()

    expect(spawnProcess).toHaveBeenCalledTimes(1)
    expect(manager.isExternalDaemon()).toBe(false)

    manager.stop()
  })

  it('does not reuse an external daemon whose /health protocol_version differs, and does not kill it', async () => {
    const fetchMock = vi.fn(async () => new Response(JSON.stringify({
      status: 'ok',
      version: '9.9.9',
      backend: 'rust',
      protocol_version: CLIENT_PROTOCOL_VERSION + 1,
      migrations: { store: 1, memory: 1 },
    }), { status: 200 }))
    vi.stubGlobal('fetch', fetchMock)
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    try {
      const spawnProcess = vi.fn(() => fakeChild(111))
      const external: ExternalDaemonSource = {
        read: () => ({ port: 5555, token: 'tok', pid: 4242, authMode: 'legacy', protocolVersion: CLIENT_PROTOCOL_VERSION }),
        checkHealth: vi.fn().mockResolvedValue(true),
        fetchHealth: fetchDaemonHealth,
      }
      const manager = new BackendManager({ external, spawnProcess })

      await manager.start()

      expect(fetchMock).toHaveBeenCalled()
      expect(manager.isExternalDaemon()).toBe(false)
      expect(getBackendEndpoint()).toBeNull()
      expect(spawnProcess).not.toHaveBeenCalled()
      expect(manager.status()).toBe('error')
      expect(manager.statusDetail()).toContain('请升级较旧的一方')
      expect(manager.statusDetail()).toContain('4242')
      const logged = warn.mock.calls.map((args) => args.join(' ')).join('\n')
      expect(logged).toContain('不复用外部 daemon')
      expect(logged).toContain(`daemon 协议版本 ${CLIENT_PROTOCOL_VERSION + 1}`)
      expect(logged).toContain(`客户端协议版本 ${CLIENT_PROTOCOL_VERSION}`)
      expect(logged).toContain(`daemon.json 记录的协议版本为 ${CLIENT_PROTOCOL_VERSION}`)
      expect(logged).toContain('4242')

      manager.stop()
    } finally {
      warn.mockRestore()
      vi.unstubAllGlobals()
    }
  })

  it('does not reuse a live old daemon whose /health omits protocol_version', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    try {
      const fetchHealth = vi.fn().mockResolvedValue({ ok: true, protocolVersion: null })
      const spawnProcess = vi.fn(() => fakeChild(111))
      const external: ExternalDaemonSource = {
        read: () => ({ port: 5555, token: 'tok', pid: 4242 }),
        checkHealth: vi.fn(),
        fetchHealth,
      }
      const manager = new BackendManager({ external, spawnProcess })

      await manager.start()

      expect(fetchHealth).toHaveBeenCalledTimes(1)
      expect(manager.isExternalDaemon()).toBe(false)
      expect(spawnProcess).not.toHaveBeenCalled()
      expect(manager.status()).toBe('error')
      expect(manager.statusDetail()).toContain('请升级较旧的一方')
      expect(manager.statusDetail()).toContain('daemon.json 没有记录协议版本')
      expect(manager.statusDetail()).toContain('4242')
      const logged = warn.mock.calls.map((args) => args.join(' ')).join('\n')
      expect(logged).toContain('daemon 版本过旧，请升级')

      manager.stop()
    } finally {
      warn.mockRestore()
    }
  })

  it('old daemon.json and an unreachable port spawns exactly once', async () => {
    const fetchHealth = vi.fn().mockResolvedValue({ ok: false, protocolVersion: null })
    const spawnProcess = vi.fn(() => fakeChild(111))
    const external: ExternalDaemonSource = {
      read: () => ({ port: 5555, token: 'tok', pid: 4242 }),
      checkHealth: vi.fn().mockResolvedValue(false),
      fetchHealth,
    }
    const manager = new BackendManager({ external, spawnProcess })

    await manager.start()

    expect(fetchHealth).toHaveBeenCalledTimes(1)
    expect(spawnProcess).toHaveBeenCalledTimes(1)
    expect(manager.isExternalDaemon()).toBe(false)
    expect(manager.status()).not.toBe('error')
    expect(manager.statusDetail()).toBeNull()

    manager.stop()
  })

  it('a 200 whose body is not JSON is incompatible and does not spawn', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response('not-json', { status: 200 })))
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    try {
      const spawnProcess = vi.fn(() => fakeChild(111))
      const external: ExternalDaemonSource = {
        read: () => ({ port: 5555, token: 'tok', pid: 4242, authMode: 'legacy', protocolVersion: CLIENT_PROTOCOL_VERSION }),
        checkHealth: vi.fn(),
        fetchHealth: fetchDaemonHealth,
      }
      const manager = new BackendManager({ external, spawnProcess })

      await manager.start()

      expect(spawnProcess).not.toHaveBeenCalled()
      expect(manager.isExternalDaemon()).toBe(false)
      expect(getBackendEndpoint()).toBeNull()
      expect(manager.status()).toBe('error')
      expect(manager.statusDetail()).toContain('daemon 版本过旧，请升级')
      const logged = warn.mock.calls.map((args) => args.join(' ')).join('\n')
      expect(logged).toContain('不复用外部 daemon')

      manager.stop()
    } finally {
      warn.mockRestore()
      vi.unstubAllGlobals()
    }
  })
})

describe('fetchDaemonHealth', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('treats HTTP 200 with a non-JSON body as a live peer with no protocol version', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response('not-json', {
      status: 200,
      headers: { 'content-type': 'text/plain' },
    })))
    await expect(fetchDaemonHealth({ port: 5555, token: 'tok' })).resolves.toEqual({
      ok: true,
      protocolVersion: null,
    })
  })

  it('treats a network failure as absent', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => {
      throw new Error('connect ECONNREFUSED')
    }))
    await expect(fetchDaemonHealth({ port: 5555, token: 'tok' })).resolves.toEqual({
      ok: false,
      protocolVersion: null,
    })
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
      read: () => (externalAvailable
        ? { port: 5555, token: 'tok', pid: 4242, authMode: 'legacy', protocolVersion: CLIENT_PROTOCOL_VERSION }
        : null),
      checkHealth: vi.fn((endpoint: { port: number } | null) => {
        // The external daemon's own endpoint is only healthy once it
        // becomes available; a self-spawned child's endpoint (currentEndpoint
        // stays null here since stdout is null / no ready line ever arrives,
        // exactly like the real lost-singleton-lock failure) never is.
        if (endpoint && endpoint.port === 5555) return Promise.resolve(externalAvailable)
        return Promise.resolve(false)
      }),
      fetchHealth: vi.fn((endpoint: { port: number }) => {
        if (endpoint.port === 5555 && externalAvailable) {
          return Promise.resolve({ ok: true, protocolVersion: CLIENT_PROTOCOL_VERSION })
        }
        return Promise.resolve({ ok: false, protocolVersion: null })
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

  it('does not spawn over an incompatible external daemon, and the health loop does not retry', async () => {
    vi.useFakeTimers()
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const spawnProcess = vi.fn(() => fakeChild(111))
    const external: ExternalDaemonSource = {
      read: () => ({ port: 5555, token: 'tok', pid: 4242, authMode: 'legacy', protocolVersion: CLIENT_PROTOCOL_VERSION }),
      checkHealth: vi.fn().mockResolvedValue(false),
      fetchHealth: vi.fn().mockResolvedValue({
        ok: true,
        protocolVersion: CLIENT_PROTOCOL_VERSION + 1,
      }),
    }
    const manager = new BackendManager({ external, spawnProcess })
    try {
      await manager.start()

      // The external daemon holds the singleton lock. Spawning (now, or on a
      // later health tick) fails and retries forever.
      expect(spawnProcess).not.toHaveBeenCalled()
      expect(manager.isExternalDaemon()).toBe(false)
      expect(getBackendEndpoint()).toBeNull()
      expect(manager.status()).toBe('error')
      const detail = manager.statusDetail()
      expect(detail).toContain('请升级较旧的一方')
      expect(detail).toContain('4242')
      expect(detail).toContain(`daemon 协议版本 ${CLIENT_PROTOCOL_VERSION + 1}`)
      expect(detail).toContain(`客户端协议版本 ${CLIENT_PROTOCOL_VERSION}`)

      const warnsAfterStart = warn.mock.calls.length
      expect(warnsAfterStart).toBeGreaterThan(0)

      for (let i = 0; i < MAX_HEALTH_FAILURES + 2; i++) {
        await vi.advanceTimersByTimeAsync(HEALTH_INTERVAL_MS)
      }

      expect(spawnProcess).not.toHaveBeenCalled()
      expect(warn.mock.calls.length).toBe(warnsAfterStart)
      expect(manager.status()).toBe('error')
      expect(external.checkHealth).not.toHaveBeenCalled()

      manager.restart()
      manager.startDaemon()
      expect(spawnProcess).not.toHaveBeenCalled()
      expect(manager.isExternalDaemon()).toBe(false)
      expect(getBackendEndpoint()).toBeNull()
    } finally {
      manager.stop()
      warn.mockRestore()
    }
  })

  it('blocked, then the other side becomes compatible → a later tick reuses it', async () => {
    vi.useFakeTimers()
    let live = CLIENT_PROTOCOL_VERSION + 1
    const fetchHealth = vi.fn(async () => ({ ok: true, protocolVersion: live }))
    const spawnProcess = vi.fn(() => fakeChild(111))
    const external: ExternalDaemonSource = {
      read: () => ({ port: 5555, token: 'tok', pid: 4242, authMode: 'legacy', protocolVersion: CLIENT_PROTOCOL_VERSION }),
      checkHealth: vi.fn().mockResolvedValue(true),
      fetchHealth,
    }
    const manager = new BackendManager({ external, spawnProcess })
    try {
      await manager.start()
      expect(spawnProcess).not.toHaveBeenCalled()
      expect(manager.status()).toBe('error')
      expect(manager.isExternalDaemon()).toBe(false)

      live = CLIENT_PROTOCOL_VERSION
      await vi.advanceTimersByTimeAsync(HEALTH_INTERVAL_MS)

      expect(manager.status()).toBe('running')
      expect(fetchHealth.mock.calls.length).toBeGreaterThanOrEqual(2)
      expect(spawnProcess).not.toHaveBeenCalled()
      expect(manager.isExternalDaemon()).toBe(true)
      expect(manager.externalDaemonPid()).toBe(4242)
      expect(getBackendEndpoint()).toEqual({ port: 5555 })
      expect(manager.statusDetail()).toBeNull()
    } finally {
      manager.stop()
    }
  })

  it('blocked, then the occupant stops answering → a later tick clears the block and spawns once', async () => {
    vi.useFakeTimers()
    let health: { ok: boolean; protocolVersion: number | null } = {
      ok: true,
      protocolVersion: CLIENT_PROTOCOL_VERSION + 1,
    }
    const fetchHealth = vi.fn(async () => health)
    const spawnProcess = vi.fn(() => fakeChild(111))
    const external: ExternalDaemonSource = {
      read: () => ({ port: 5555, token: 'tok', pid: 4242, authMode: 'legacy', protocolVersion: CLIENT_PROTOCOL_VERSION }),
      checkHealth: vi.fn().mockResolvedValue(false),
      fetchHealth,
    }
    const manager = new BackendManager({ external, spawnProcess })
    try {
      await manager.start()
      expect(manager.status()).toBe('error')
      expect(spawnProcess).not.toHaveBeenCalled()

      health = { ok: false, protocolVersion: null }
      await vi.advanceTimersByTimeAsync(HEALTH_INTERVAL_MS)

      expect(spawnProcess).toHaveBeenCalledTimes(1)
      expect(manager.status()).not.toBe('error')
      expect(manager.statusDetail()).toBeNull()
      expect(manager.isExternalDaemon()).toBe(false)
      expect(fetchHealth.mock.calls.length).toBeGreaterThanOrEqual(2)
    } finally {
      manager.stop()
    }
  })
})

describe('resolveNodeEntryFrom (REV-01: path depth, no injection)', () => {
  const want = path.resolve('/R/packages/node-daemon/dist/server.js')
  it.each(['/R/apps/desktop/dist/main', '/R/apps/desktop/src/main'])('%s → <repo>/packages/node-daemon/dist/server.js', (dir) => {
    expect(resolveNodeEntryFrom(dir, () => true)).toBe(want)
  })
  it('throws with a build hint when the file is absent', () => {
    expect(() => resolveNodeEntryFrom('/R/apps/desktop/dist/main', () => false)).toThrow(/node-daemon build/)
  })
  it('the real __dirname (src/main) resolves to this checkout\'s packages/node-daemon', () => {
    expect(resolveNodeEntryFrom(__dirname, () => true)).toBe(path.resolve(__dirname, '../../../../packages/node-daemon/dist/server.js'))
    expect(fs.existsSync(path.resolve(__dirname, '../../../../packages/node-daemon/package.json'))).toBe(true)
  })
})

describe('selectedBackend (ADR-037 / CORE-8: node-daemon not shipped)', () => {
  const prevB = process.env['AGENT24_BACKEND']
  const prevN = process.env['NODE_ENV']
  const restore = (k: string, v: string | undefined) => {
    if (v === undefined) delete process.env[k]
    else process.env[k] = v
  }
  afterEach(() => {
    restore('AGENT24_BACKEND', prevB)
    restore('NODE_ENV', prevN)
    vi.restoreAllMocks()
  })
  const ok = () => '/fake/server.js'
  const noWarnSpy = () => vi.spyOn(console, 'warn').mockImplementation(() => {})

  it('packaged + AGENT24_BACKEND=node → rust, with a warning', () => {
    process.env['AGENT24_BACKEND'] = 'node'
    const warn = noWarnSpy()
    expect(selectedBackend(true, ok)).toBe('rust')
    expect(warn).toHaveBeenCalledOnce()
  })

  it('dev + AGENT24_BACKEND=node → node', () => {
    process.env['AGENT24_BACKEND'] = 'node'
    expect(selectedBackend(false, ok)).toBe('node')
  })

  it('unset → rust in both modes', () => {
    delete process.env['AGENT24_BACKEND']
    expect(selectedBackend(true, ok)).toBe('rust')
    expect(selectedBackend(false, ok)).toBe('rust')
  })

  it('dev + node but node-daemon unresolvable → warn and fall back to rust (no throw)', () => {
    process.env['AGENT24_BACKEND'] = 'node'
    const warn = noWarnSpy()
    const bad = () => {
      throw new Error('Cannot find module')
    }
    expect(selectedBackend(false, bad)).toBe('rust')
    expect(warn).toHaveBeenCalledOnce()
  })

  it('BackendManager: injected isPackaged=true wins over inherited NODE_ENV=development', () => {
    process.env['AGENT24_BACKEND'] = 'node'
    process.env['NODE_ENV'] = 'development'
    noWarnSpy()
    expect(new BackendManager({ isPackaged: true, resolveNode: ok }).backendKind()).toBe('rust')
  })

  it('BackendManager: kind follows the injected isPackaged (false → node)', () => {
    process.env['AGENT24_BACKEND'] = 'node'
    process.env['NODE_ENV'] = 'production'
    expect(new BackendManager({ isPackaged: false, resolveNode: ok }).backendKind()).toBe('node')
  })

  it('BackendManager: no injection fails closed (rust)', () => {
    process.env['AGENT24_BACKEND'] = 'node'
    noWarnSpy()
    expect(new BackendManager({ resolveNode: ok }).backendKind()).toBe('rust')
  })
})

describe('resolveRustBinary (ADR-037: packaged decided by injected isPackaged)', () => {
  const prevN = process.env['NODE_ENV']
  const prevBin = process.env['AGENT24D_BIN']
  const prevRes = (process as { resourcesPath?: string }).resourcesPath
  afterEach(() => {
    if (prevN === undefined) delete process.env['NODE_ENV']
    else process.env['NODE_ENV'] = prevN
    if (prevBin === undefined) delete process.env['AGENT24D_BIN']
    else process.env['AGENT24D_BIN'] = prevBin
    ;(process as { resourcesPath?: string }).resourcesPath = prevRes
  })
  const exe = process.platform === 'win32' ? 'agent24d.exe' : 'agent24d'

  it('isPackaged=true + inherited NODE_ENV=development → resourcesPath/backend binary', () => {
    delete process.env['AGENT24D_BIN']
    process.env['NODE_ENV'] = 'development'
    ;(process as { resourcesPath?: string }).resourcesPath = '/app/Resources'
    expect(resolveRustBinary(true)).toBe(path.join('/app/Resources', 'backend', exe))
  })

  it('isPackaged=false + NODE_ENV=production → rust/target/debug', () => {
    delete process.env['AGENT24D_BIN']
    process.env['NODE_ENV'] = 'production'
    expect(resolveRustBinary(false)).toBe(
      path.join(path.resolve(__dirname, '..', '..', '..', '..'), 'rust', 'target', 'debug', exe),
    )
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
    const external: ExternalDaemonSource = withProbe({ read: () => null, checkHealth: vi.fn().mockResolvedValue(true) })
    const manager = new BackendManager({
      external,
      authMode,
      // dev build: the node reference daemon is only honoured outside a packaged app
      isPackaged: false,
      resolveNode: () => '/x/server.js',
      spawnProcess: () => childAnnouncing(ready),
    })
    await manager.start()
    await vi.waitFor(() => expect(getBackendEndpoint()).not.toBeNull())
    return manager
  }

  it('the rust ready token becomes a bearer header, never part of the endpoint', async () => {
    delete process.env['AGENT24_BACKEND']
    const manager = await startWith({ port: 4100, token: 'secret' })
    expect(getBackendEndpoint()).toEqual({ port: 4100 })
    expect(daemonConnection()).toEqual({ port: 4100, authHeaders: { Authorization: 'Bearer secret' } })
    expect(daemonCliEndpoint()).toEqual({ port: 4100, token: 'secret' })
    manager.stop()
  })

  it('a rust daemon that announced no token gets no connection: nothing is sent without a credential', async () => {
    delete process.env['AGENT24_BACKEND']
    const manager = await startWith({ port: 4101 })
    expect(getBackendEndpoint()).toEqual({ port: 4101 })
    expect(daemonConnection()).toBeNull()
    expect(daemonCliEndpoint()).toBeNull()
    manager.stop()
  })

  it('the health tick probes the current port', async () => {
    delete process.env['AGENT24_BACKEND']
    const checkHealth = vi.fn().mockResolvedValue(true)
    const external: ExternalDaemonSource = withProbe({ read: () => null, checkHealth })
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
    const external: ExternalDaemonSource = withProbe({ read: () => null, checkHealth: vi.fn().mockResolvedValue(true) })
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
    const external: ExternalDaemonSource = withProbe({
      read: () => ({ port: 5558, token: 'external', pid: 4245, authMode: 'legacy' }),
      checkHealth: vi.fn(() => Promise.resolve(healthy)),
    })
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
    expect(daemonCliEndpoint()).toEqual({ port: 8765, token: '' })
    manager.stop()
  })

  it('a reused legacy record without a token gets no connection', async () => {
    const external: ExternalDaemonSource = withProbe({
      read: () => ({ port: 5556, token: '', pid: 4243, authMode: 'legacy' }),
      checkHealth: vi.fn().mockResolvedValue(true),
    })
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
    const external: ExternalDaemonSource = withProbe({ read: () => null, checkHealth: vi.fn().mockResolvedValue(true) })
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
    const external: ExternalDaemonSource = withProbe({ read: () => null, checkHealth: vi.fn().mockResolvedValue(true) })
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

    // A host token under any other mode, or one that is not a string, is no
    // credential either (PR-Daemon #1184 Low 1 / Low A).
    for (const ready of [
      { port: 4313, auth_mode: 'legacy_single_token', product_host_token: 'x' },
      { port: 4314, auth_mode: 'capabilities', product_host_token: 42 },
    ]) {
      const error = vi.spyOn(console, 'error').mockImplementation(() => {})
      const manager = new BackendManager({ external, authMode: 'capabilities', spawnProcess: () => childAnnouncing(ready) })
      await manager.start()
      await vi.waitFor(() => expect(getBackendEndpoint()).not.toBeNull())
      expect(daemonConnection()).toBeNull()
      expect(error).toHaveBeenCalledWith(expect.stringContaining('carries no credential'))
      manager.stop()
      error.mockRestore()
    }
  })

  it('another host\'s live capability daemon blocks: nothing reused, nothing spawned, until it is gone', async () => {
    let foreignAlive = true
    const spawnProcess = vi.fn(() =>
      childAnnouncing({ port: 4303, auth_mode: 'capabilities', product_host_token: 'mine' }),
    )
    const external: ExternalDaemonSource = withProbe({
      read: () => (foreignAlive ? { port: 5557, token: '', pid: 4244, authMode: 'capabilities' } : null),
      checkHealth: vi.fn().mockResolvedValue(true),
    })
    const manager = new BackendManager({ external, spawnProcess, authMode: 'capabilities' })
    await manager.start()
    const tick = (manager as unknown as { tick(): Promise<void> }).tick.bind(manager)

    expect(spawnProcess).not.toHaveBeenCalled()
    expect(manager.isExternalDaemon()).toBe(false)
    expect(daemonConnection()).toBeNull()
    expect(manager.status()).toBe('blocked')
    expect(manager.foreignDaemonPid()).toBe(4244)

    for (let i = 0; i < 5; i++) await tick()
    expect(spawnProcess).not.toHaveBeenCalled()
    expect(manager.status()).toBe('blocked')

    foreignAlive = false
    await tick()
    expect(spawnProcess).toHaveBeenCalledTimes(1)
    expect(manager.foreignDaemonPid()).toBeNull()
    await vi.waitFor(() => expect(daemonConnection()?.port).toBe(4303))
    manager.stop()
  })

  it('a manual restart while blocked starts exactly one child and leaves the blocked state', async () => {
    let foreignAlive = true
    const children: ChildProcess[] = []
    const spawnProcess = vi.fn(() => {
      const child = childAnnouncing({ port: 4306 + children.length, auth_mode: 'capabilities', product_host_token: 'mine' })
      children.push(child)
      return child
    })
    const external: ExternalDaemonSource = withProbe({
      read: () => (foreignAlive ? { port: 5560, token: '', pid: 4247, authMode: 'capabilities' } : null),
      checkHealth: vi.fn().mockResolvedValue(true),
    })
    const manager = new BackendManager({ external, spawnProcess, authMode: 'capabilities' })
    await manager.start()
    expect(manager.status()).toBe('blocked')

    manager.restart()
    expect(spawnProcess).toHaveBeenCalledTimes(1)
    expect(manager.foreignDaemonPid()).toBeNull()
    foreignAlive = false
    await (manager as unknown as { tick(): Promise<void> }).tick()
    expect(spawnProcess).toHaveBeenCalledTimes(1)
    manager.stop()
  })

  it('a reused legacy daemon that dies while another host takes the data dir leaves no connection behind', async () => {
    let healthyLegacy = true
    let record: ExternalDaemonState = { port: 5561, token: 'legacy', pid: 4248, authMode: 'legacy' }
    const spawnProcess = vi.fn(() => childAnnouncing({ port: 4310, auth_mode: 'capabilities', product_host_token: 'mine' }))
    const external: ExternalDaemonSource = withProbe({
      read: () => record,
      // The legacy daemon answers until it dies; the other host's always does.
      checkHealth: ({ port }) => Promise.resolve(port === 5562 || (port === 5561 && healthyLegacy)),
    })
    const manager = new BackendManager({ external, spawnProcess, authMode: 'capabilities' })
    await manager.start()
    expect(manager.isExternalDaemon()).toBe(true)

    healthyLegacy = false
    record = { port: 5562, token: '', pid: 4249, authMode: 'capabilities' }
    const tick = (manager as unknown as { tick(): Promise<void> }).tick.bind(manager)
    for (let i = 0; i < 3; i++) await tick() // MAX_HEALTH_FAILURES
    expect(manager.status()).toBe('blocked')
    expect(manager.foreignDaemonPid()).toBe(4249)
    expect(manager.isExternalDaemon()).toBe(false)
    expect(manager.externalDaemonPid()).toBeNull()
    expect(daemonConnection()).toBeNull()
    expect(spawnProcess).not.toHaveBeenCalled()
    manager.stop()
  })

  describe('a tray action during a pending probe wins over its stale result', () => {
    function blockedManager() {
      let probe: ((alive: boolean) => void) | null = null
      const spawnProcess = vi.fn(() => childAnnouncing({ port: 4311, auth_mode: 'capabilities', product_host_token: 'mine' }))
      const external: ExternalDaemonSource = withProbe({
        read: () => ({ port: 5563, token: '', pid: 4250, authMode: 'capabilities' }),
        checkHealth: vi.fn().mockResolvedValue(true),
      })
      const manager = new BackendManager({ external, spawnProcess, authMode: 'capabilities' })
      const holdNextProbe = () => {
        vi.mocked(external.checkHealth).mockImplementationOnce(() => new Promise((resolve) => { probe = resolve }))
      }
      const tick = (manager as unknown as { tick(): Promise<void> }).tick.bind(manager)
      return { manager, spawnProcess, holdNextProbe, tick, answer: (alive: boolean) => probe!(alive) }
    }

    it('stop or app quit: the foreign daemon going away meanwhile starts nothing', async () => {
      for (const action of ['stopDaemon', 'stop'] as const) {
        const m = blockedManager()
        await m.manager.start()
        expect(m.manager.status()).toBe('blocked')
        m.holdNextProbe()
        const pending = m.tick()
        m.manager[action]()
        m.answer(false)
        await pending
        expect(m.spawnProcess).not.toHaveBeenCalled()
        m.manager.stop()
      }
    })

    it('restart or start: exactly one child, and a late `foreign` answer does not re-block it', async () => {
      for (const action of ['restart', 'startDaemon'] as const) for (const alive of [false, true]) {
        const m = blockedManager()
        await m.manager.start()
        m.holdNextProbe()
        const pending = m.tick()
        m.manager[action]()
        m.answer(alive)
        await pending
        expect(m.spawnProcess).toHaveBeenCalledTimes(1)
        expect(m.manager.foreignDaemonPid()).toBeNull()
        expect(m.manager.status()).not.toBe('blocked')
        m.manager.stop()
      }
    })

    it('app quit during the first probe of start(): no child and no health loop afterwards', async () => {
      vi.useFakeTimers()
      try {
        const m = blockedManager()
        m.holdNextProbe()
        const starting = m.manager.start()
        m.manager.stop()
        m.answer(false)
        await starting
        await vi.advanceTimersByTimeAsync(25_000) // five health intervals
        expect(m.spawnProcess).not.toHaveBeenCalled()
        expect(vi.getTimerCount()).toBe(0)
      } finally {
        vi.useRealTimers()
      }
    })

    it('stop during a failing health tick of our own child starts no replacement', async () => {
      const spawnProcess = vi.fn(() => childAnnouncing({ port: 4312, auth_mode: 'capabilities', product_host_token: 'mine' }))
      let probe: ((alive: boolean) => void) | null = null
      const external: ExternalDaemonSource = withProbe({ read: () => null, checkHealth: vi.fn().mockResolvedValue(false) })
      const manager = new BackendManager({ external, spawnProcess, authMode: 'capabilities' })
      await manager.start()
      const tick = (manager as unknown as { tick(): Promise<void> }).tick.bind(manager)
      for (let i = 0; i < 2; i++) await tick() // one short of MAX_HEALTH_FAILURES
      vi.mocked(external.checkHealth).mockImplementationOnce(() => new Promise((resolve) => { probe = resolve }))
      const pending = tick()
      manager.stopDaemon()
      probe!(false)
      await pending
      expect(spawnProcess).toHaveBeenCalledTimes(1)
      expect(manager.status()).toBe('stopped')
      manager.stop()
    })
  })

  it('a stale capability record (daemon not answering) does not block: spawns its own', async () => {
    const spawnProcess = vi.fn(() =>
      childAnnouncing({ port: 4304, auth_mode: 'capabilities', product_host_token: 'mine' }),
    )
    const external: ExternalDaemonSource = withProbe({
      read: () => ({ port: 5559, token: '', pid: 4246, authMode: 'capabilities' }),
      checkHealth: vi.fn().mockResolvedValue(false),
    })
    const manager = new BackendManager({ external, spawnProcess, authMode: 'capabilities' })
    await manager.start()
    expect(spawnProcess).toHaveBeenCalledTimes(1)
    expect(manager.status()).not.toBe('blocked')
    await vi.waitFor(() => expect(daemonConnection()?.port).toBe(4304))
    manager.stop()
  })

  it('asking for legacy accepts only a legacy ready line, not an unknown mode', async () => {
    const external: ExternalDaemonSource = withProbe({ read: () => null, checkHealth: vi.fn().mockResolvedValue(true) })
    const manager = new BackendManager({
      external,
      authMode: 'legacy',
      spawnProcess: () => childAnnouncing({ port: 4305, auth_mode: 'something_new', token: 'x' }),
    })
    await manager.start()
    await vi.waitFor(() => expect(getBackendEndpoint()).not.toBeNull())
    expect(daemonConnection()).toBeNull()
    manager.stop()
  })

  it('a ready line is never echoed to the log, even when malformed', async () => {
    const log = vi.spyOn(console, 'log').mockImplementation(() => {})
    const external: ExternalDaemonSource = withProbe({ read: () => null, checkHealth: vi.fn().mockResolvedValue(true) })
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
