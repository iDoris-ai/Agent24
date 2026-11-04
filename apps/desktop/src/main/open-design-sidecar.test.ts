import { EventEmitter } from 'node:events'
import { PassThrough } from 'node:stream'
import type { ChildProcess } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  reapStaleLauncher,
  OpenDesignHealth,
  OpenDesignLauncher,
  PosixProcessGroupTree,
  openDesignSidecarSpec,
  type KillFn,
  type OpenDesignLaunchPlan,
} from './open-design-sidecar'
import { MemoryEndpointHandoff } from './sidecar-contract'
import { SidecarManager, validateReady } from './sidecar-manager'

const errno = (code: string) => Object.assign(new Error(code), { code })

class FakeChild extends EventEmitter {
  pid: number | undefined = 5000
  stdout = new PassThrough()
  stderr = new PassThrough()
  exitCode: number | null = null
  signalCode: NodeJS.Signals | null = null
}

const tempDirs: string[] = []
afterEach(() => {
  vi.useRealTimers()
  for (const dir of tempDirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true })
})

function plan(overrides: Partial<OpenDesignLaunchPlan> = {}): OpenDesignLaunchPlan {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-odsc-'))
  tempDirs.push(root)
  return {
    headlessEntry: '/res/app/prebundled/agent24-headless.cjs',
    resourcesPath: root,
    runtimeExecutable: '/Apps/Agent24.app/Contents/Frameworks/Agent24 Helper.app/Contents/MacOS/Agent24 Helper',
    configPath: path.join(root, 'state', 'agent24-headless.json'),
    config: { protocol: 2, pinVersion: '0.26.1' },
    dataRoot: path.join(root, 'state', 'data'),
    runtimeRoot: path.join(root, 'state', 'runtime'),
    pinVersion: '0.26.1',
    environment: { PATH: '/usr/bin' },
    agent24Cli: '/Apps/Agent24.app/Contents/Resources/backend/agent24',
    readyPhaseTimeoutMs: 200,
    ...overrides,
  }
}

const logger = { info: vi.fn(), warn: vi.fn() }
const readyLine = (pid: number, extra: Record<string, unknown> = {}) => `${JSON.stringify({
  type: 'ready', protocol: 2, instanceId: 'od-1', pinVersion: '0.26.1',
  webOrigin: 'http://127.0.0.1:17456', daemonOrigin: 'http://127.0.0.1:17457',
  ownership: { ownerPid: pid, kind: 'process-tree' }, ...extra,
})}\n`
const okFetch = () => vi.fn(async () => new Response('{}', { status: 200 })) as unknown as typeof fetch

function launcherFor(child: FakeChild, current: OpenDesignLaunchPlan, fetchFn = okFetch(), platform: NodeJS.Platform = 'darwin') {
  const spawnFn = vi.fn(() => child as unknown as ChildProcess)
  return { spawnFn, launcher: new OpenDesignLauncher(() => current, spawnFn as never, fetchFn, logger, platform, vi.fn()) }
}

describe('openDesignSidecarSpec', () => {
  it('uses the designed bounds: ready 30 s, health 10 s / 3 s / 3 failures, TERM grace 15 s, kill 5 s', () => {
    expect(openDesignSidecarSpec()).toEqual({
      sidecarId: 'open-design',
      readyTimeoutMs: 30_000,
      healthIntervalMs: 10_000,
      healthTimeoutMs: 3_000,
      maxHealthFailures: 3,
      shutdown: { termGraceMs: 15_000, killAfterMs: 5_000 },
    })
  })

  it('keeps the manager readiness bound above the launcher phases so OD reports its own error first', () => {
    const spec = openDesignSidecarSpec({ readyPhaseTimeoutMs: 500 })
    expect(spec.readyTimeoutMs).toBeGreaterThan(2 * 500)
  })
})

describe('PosixProcessGroupTree', () => {
  function tree(kill: KillFn) { return new PosixProcessGroupTree(4321, kill) }

  it('sends the graceful signal to the launcher pid only, never to the group', async () => {
    const kill = vi.fn()
    await tree(kill).signal(false)
    expect(kill.mock.calls).toEqual([[-4321, 0], [4321, 'SIGTERM']])
  })

  it('sends the forced signal to the negative pgid', async () => {
    const kill = vi.fn()
    await tree(kill).signal(true)
    expect(kill).toHaveBeenLastCalledWith(-4321, 'SIGKILL')
    expect(kill).not.toHaveBeenCalledWith(4321, 'SIGKILL')
  })

  it('treats ESRCH as an empty group', async () => {
    const t = tree(() => { throw errno('ESRCH') })
    await expect(t.isEmpty()).resolves.toBe(true)
  })

  it('treats a live group as non-empty and EPERM as non-empty', async () => {
    await expect(tree(() => true).isEmpty()).resolves.toBe(false)
    await expect(tree(() => { throw errno('EPERM') }).isEmpty()).resolves.toBe(false)
  })

  it('surfaces unexpected errors instead of calling the group empty', async () => {
    await expect(tree(() => { throw errno('EINVAL') }).isEmpty()).rejects.toThrow('EINVAL')
    await expect(tree((_pid, signal) => { if (signal === 'SIGTERM') throw errno('EPERM'); return true }).signal(false)).rejects.toThrow('EPERM')
  })

  it('ignores ESRCH from the graceful and forced signals themselves', async () => {
    await expect(tree(() => { throw errno('ESRCH') }).signal(false)).resolves.toBeUndefined()
    const kill = vi.fn((_pid: number, signal: NodeJS.Signals | 0) => { if (signal === 'SIGKILL') throw errno('ESRCH'); return true })
    await expect(tree(kill).signal(true)).resolves.toBeUndefined()
  })

  it('retires permanently after SIGKILL ESRCH even when the pgid is reused', async () => {
    const kill = vi.fn((_pid: number, signal: NodeJS.Signals | 0) => {
      if (signal === 'SIGKILL') throw errno('ESRCH')
      return true
    })
    const t = tree(kill)
    await t.signal(true)
    expect(kill).toHaveBeenLastCalledWith(-4321, 'SIGKILL')
    kill.mockImplementation(() => true)
    kill.mockClear()
    await t.signal(true)
    await t.signal(false)
    await expect(t.isEmpty()).resolves.toBe(true)
    expect(kill).not.toHaveBeenCalled()
  })

  it('never SIGKILLs a group that is already gone', async () => {
    const kill = vi.fn((): boolean => { throw errno('ESRCH') })
    await tree(kill).signal(true)
    expect(kill.mock.calls.map((call) => call.slice(1))).toEqual([[0]])
  })

  it('retires after observing an empty group: no further signal of any kind is sent (pgid reuse)', async () => {
    const kill = vi.fn((): boolean => { throw errno('ESRCH') })
    const t = tree(kill)
    await expect(t.isEmpty()).resolves.toBe(true)
    kill.mockClear()
    await t.signal(true)
    await t.signal(false)
    await expect(t.isEmpty()).resolves.toBe(true)
    expect(kill).not.toHaveBeenCalled()
  })

  it('stays live while the group exists, so a later force still reaches it', async () => {
    const kill = vi.fn(() => true)
    const t = tree(kill)
    await expect(t.isEmpty()).resolves.toBe(false)
    await t.signal(true)
    expect(kill).toHaveBeenLastCalledWith(-4321, 'SIGKILL')
  })

  it('refuses pids that could address everything (0, 1, negative)', () => {
    for (const pid of [0, 1, -5, 1.5]) expect(() => new PosixProcessGroupTree(pid, vi.fn())).toThrow()
  })
})

describe('OpenDesignHealth', () => {
  const endpoint = { origin: 'http://127.0.0.1:17456', auth: 'none' as const }

  it('is healthy on 200 from /api/ready through the web origin', async () => {
    const fetchFn = vi.fn(async () => new Response('{}', { status: 200 }))
    await expect(new OpenDesignHealth(fetchFn as never).check(endpoint, 50)).resolves.toBe(true)
    expect(fetchFn).toHaveBeenCalledWith('http://127.0.0.1:17456/api/ready', expect.objectContaining({ signal: expect.any(AbortSignal) }))
  })

  it('is unhealthy on 503 (daemon closing), on network errors, and when the request outlives the timeout', async () => {
    await expect(new OpenDesignHealth((async () => new Response('', { status: 503 })) as never).check(endpoint, 50)).resolves.toBe(false)
    await expect(new OpenDesignHealth((async () => { throw new Error('ECONNREFUSED') }) as never).check(endpoint, 50)).resolves.toBe(false)
    const hang = ((_url: string, init?: RequestInit) => new Promise((_resolve, reject) => {
      init?.signal?.addEventListener('abort', () => reject(new Error('aborted')))
    })) as never
    await expect(new OpenDesignHealth(hang).check(endpoint, 20)).resolves.toBe(false)
  })
})

describe('OpenDesignLauncher', () => {
  it('spawns the Helper runtime detached, under ELECTRON_RUN_AS_NODE, in the resources cwd', () => {
    const child = new FakeChild()
    const current = plan()
    const { launcher, spawnFn } = launcherFor(child, current)
    const launch = launcher.launch()
    void launch.ready.catch(() => {})
    expect(spawnFn).toHaveBeenCalledWith(
      current.runtimeExecutable,
      [current.headlessEntry, '--config', current.configPath],
      expect.objectContaining({ cwd: current.resourcesPath, detached: true, stdio: ['ignore', 'pipe', 'pipe'] }),
    )
    const env = (spawnFn.mock.calls[0] as unknown as [string, string[], { env: NodeJS.ProcessEnv }])[2].env
    expect(env.ELECTRON_RUN_AS_NODE).toBe('1')
    expect(launch.dedicatedProcessGroup).toBe(true)
    expect(launch.processGroupId).toBe(5000)
  })

  it('strips A24_* from the child environment and keeps everything else', () => {
    const child = new FakeChild()
    const { launcher, spawnFn } = launcherFor(child, plan({
      environment: { PATH: '/usr/bin', OPENAI_API_KEY: 'k', A24_HANDSHAKE_TOKEN: 'host-secret', a24_bootstrap: 'x' },
    }))
    void launcher.launch().ready.catch(() => {})
    const env = (spawnFn.mock.calls[0] as unknown as [string, string[], { env: NodeJS.ProcessEnv }])[2].env
    expect({ ...env }).toEqual({ PATH: '/usr/bin', OPENAI_API_KEY: 'k', ELECTRON_RUN_AS_NODE: '1', AGENT24_BIN: '/Apps/Agent24.app/Contents/Resources/backend/agent24' })
  })

  it('sets AGENT24_BIN to the bundled CLI but keeps a user override', () => {
    for (const [userBin, expected] of [[undefined, '/bundled/agent24'], ['/opt/mine/agent24', '/opt/mine/agent24']] as const) {
      const child = new FakeChild()
      const { launcher, spawnFn } = launcherFor(child, plan({
        environment: { PATH: '/usr/bin', ...(userBin ? { AGENT24_BIN: userBin } : {}) },
        agent24Cli: '/bundled/agent24',
      }))
      void launcher.launch().ready.catch(() => {})
      const env = (spawnFn.mock.calls[0] as unknown as [string, string[], { env: NodeJS.ProcessEnv }])[2].env
      expect(env.AGENT24_BIN).toBe(expected)
    }
  })

  it('leaves AGENT24_BIN unset when there is no bundled CLI', () => {
    const child = new FakeChild()
    const { launcher, spawnFn } = launcherFor(child, plan({ agent24Cli: null }))
    void launcher.launch().ready.catch(() => {})
    const env = (spawnFn.mock.calls[0] as unknown as [string, string[], { env: NodeJS.ProcessEnv }])[2].env
    expect(env.AGENT24_BIN).toBeUndefined()
  })

  it('rewrites the 0600 managed config on every launch', () => {
    const current = plan({ config: { protocol: 2, generation: 1 } })
    const child = new FakeChild()
    const { launcher } = launcherFor(child, current)
    void launcher.launch().ready.catch(() => {})
    expect(JSON.parse(fs.readFileSync(current.configPath, 'utf8'))).toEqual({ protocol: 2, generation: 1 })
    expect(fs.statSync(current.configPath).mode & 0o777).toBe(0o600)
    expect(fs.existsSync(current.dataRoot) && fs.existsSync(current.runtimeRoot)).toBe(true)
    fs.writeFileSync(current.configPath, 'stale')
    const second = new FakeChild()
    void launcherFor(second, current).launcher.launch().ready.catch(() => {})
    expect(JSON.parse(fs.readFileSync(current.configPath, 'utf8'))).toEqual({ protocol: 2, generation: 1 })
  })

  it('tightens a pre-existing config file to 0600 (writeFile mode only applies to new files)', () => {
    const current = plan()
    fs.mkdirSync(path.dirname(current.configPath), { recursive: true })
    fs.writeFileSync(current.configPath, 'old', { mode: 0o644 })
    fs.chmodSync(current.configPath, 0o644)
    void launcherFor(new FakeChild(), current).launcher.launch().ready.catch(() => {})
    expect(fs.statSync(current.configPath).mode & 0o777).toBe(0o600)
  })

  it('refuses win32 before touching the filesystem or spawning', () => {
    const child = new FakeChild()
    const current = plan()
    const { launcher, spawnFn } = launcherFor(child, current, okFetch(), 'win32')
    expect(() => launcher.launch()).toThrow('unsupported platform')
    expect(spawnFn).not.toHaveBeenCalled()
    expect(fs.existsSync(current.configPath)).toBe(false)
  })

  it('resolves ready with the web origin and no secret (the port is unauthenticated)', async () => {
    const child = new FakeChild()
    const { launcher } = launcherFor(child, plan())
    const launch = launcher.launch()
    child.stdout.write(readyLine(5000))
    await expect(launch.ready).resolves.toEqual({ endpoint: { origin: 'http://127.0.0.1:17456', auth: 'none' } })
    expect(() => validateReady({ endpoint: { origin: 'http://127.0.0.1:17456', auth: 'none' } })).not.toThrow()
  })

  it('polls /api/ready until the web origin answers, and does not give up early', async () => {
    const child = new FakeChild()
    let calls = 0
    const fetchFn = vi.fn(async () => (++calls < 3 ? new Response('', { status: 503 }) : new Response('{}', { status: 200 }))) as unknown as typeof fetch
    const { launcher } = launcherFor(child, plan({ readyPhaseTimeoutMs: 2_000 }), fetchFn)
    const launch = launcher.launch()
    child.stdout.write(readyLine(5000))
    await expect(launch.ready).resolves.toMatchObject({ endpoint: { origin: 'http://127.0.0.1:17456' } })
    expect(calls).toBe(3)
  })

  it('rejects an ownerPid that is not the spawned launcher, before probing', async () => {
    const child = new FakeChild()
    const fetchFn = okFetch()
    const { launcher } = launcherFor(child, plan(), fetchFn)
    const launch = launcher.launch()
    child.stdout.write(readyLine(9999))
    await expect(launch.ready).rejects.toThrow('ownership does not match')
    expect(fetchFn).not.toHaveBeenCalled()
  })

  it('rejects malformed, empty, oversized, unsupported and wrong-pin ready lines', async () => {
    const cases: Array<[string, string]> = [
      ['not json\n', 'JSON'],
      ['\n', 'empty'],
      [`${'x'.repeat(17_000)}`, 'exceeded'],
      [readyLine(5000, { secret: 's' }), 'unsupported field'],
      [readyLine(5000, { pinVersion: '0.0.1' }), 'pin version'],
      [readyLine(5000, { webOrigin: 'http://example.com:17456' }), '127.0.0.1'],
    ]
    for (const [line, message] of cases) {
      const child = new FakeChild()
      const launch = launcherFor(child, plan()).launcher.launch()
      child.stdout.write(line)
      await expect(launch.ready, message).rejects.toThrow(message)
    }
  })

  it('times out waiting for the ready line and for /api/ready, each with its own message', async () => {
    const silent = new FakeChild()
    await expect(launcherFor(silent, plan({ readyPhaseTimeoutMs: 30 })).launcher.launch().ready).rejects.toThrow('readiness output timed out')
    const child = new FakeChild()
    const notReady = vi.fn(async () => new Response('', { status: 503 })) as unknown as typeof fetch
    const launch = launcherFor(child, plan({ readyPhaseTimeoutMs: 150 }), notReady).launcher.launch()
    child.stdout.write(readyLine(5000))
    await expect(launch.ready).rejects.toThrow('/api/ready timed out: HTTP 503')
  })

  it('fails readiness with the stderr tail when the launcher exits first, and stops polling', async () => {
    const child = new FakeChild()
    const { launcher } = launcherFor(child, plan())
    const launch = launcher.launch()
    child.stderr.write('boom: cannot bind')
    await new Promise((resolve) => setImmediate(resolve))
    child.emit('exit', 3, null)
    await expect(launch.ready).rejects.toThrow('exited before ready (3): boom: cannot bind')
    expect(logger.warn).toHaveBeenCalledWith('open-design.exited', expect.objectContaining({ code: 3, stderr: 'boom: cannot bind' }))

    const polling = new FakeChild()
    const fetchFn = vi.fn(async () => new Response('', { status: 503 })) as unknown as typeof fetch
    const second = launcherFor(polling, plan({ readyPhaseTimeoutMs: 5_000 }), fetchFn).launcher.launch()
    polling.stdout.write(readyLine(5000))
    await vi.waitFor(() => expect(fetchFn).toHaveBeenCalled())
    polling.emit('exit', 1, null)
    await expect(second.ready).rejects.toThrow('exited before ready')
  })

  it('keeps only a 2 KB stderr tail', async () => {
    const child = new FakeChild()
    const launch = launcherFor(child, plan()).launcher.launch()
    child.stderr.write(`${'a'.repeat(5_000)}END`)
    await new Promise((resolve) => setImmediate(resolve))
    child.emit('exit', 1, null)
    await expect(launch.ready).rejects.toSatisfy((error: Error) => error.message.endsWith('END') && error.message.length < 2_200)
  })

  it('survives a spawn failure: the pid is missing, no unhandled error event, and the group claim is withdrawn', async () => {
    const child = new FakeChild()
    child.pid = undefined
    const launch = launcherFor(child, plan()).launcher.launch()
    expect(launch.dedicatedProcessGroup).toBe(false)
    expect(launch.processGroupId).toBeUndefined()
    expect(() => child.emit('error', new Error('spawn ENOENT'))).not.toThrow()
    await expect(launch.ready).rejects.toThrow('did not provide a pid')
  })

  it('drains stdout after the ready line so a chatty launcher cannot block on a full pipe', async () => {
    const child = new FakeChild()
    const launch = launcherFor(child, plan()).launcher.launch()
    child.stdout.write(readyLine(5000))
    await launch.ready
    expect(child.stdout.listenerCount('data')).toBeGreaterThan(0)
  })
})

describe('Open Design launcher under a real SidecarManager', () => {
  it('goes ready -> healthy and publishes an origin-only endpoint', async () => {
    const child = new FakeChild()
    const kill = vi.fn(() => { throw errno('ESRCH') })
    const spawnFn = vi.fn(() => child as unknown as ChildProcess)
    const handoff = new MemoryEndpointHandoff()
    const manager = new SidecarManager(
      openDesignSidecarSpec({ readyPhaseTimeoutMs: 200, healthIntervalMs: 1_000 }),
      new OpenDesignLauncher(() => plan(), spawnFn as never, okFetch(), logger, 'linux', kill as never),
      new OpenDesignHealth(okFetch()),
      undefined,
      handoff,
    )
    const starting = manager.start()
    child.stdout.write(readyLine(5000))
    await expect(starting).resolves.toMatchObject({ state: 'healthy', sidecarId: 'open-design' })
    expect(handoff.current()?.endpoint).toEqual({ origin: 'http://127.0.0.1:17456', auth: 'none' })
    await manager.stop()
  })
})

describe('PR1580 launcher regressions', () => {
  it('F6 retires an empty group before graceful TERM even if its pid is reused', async () => {
    const kill = vi.fn(() => { throw errno('ESRCH') })
    const tree = new PosixProcessGroupTree(4321, kill)
    await tree.signal(false)
    expect(kill).toHaveBeenCalledExactlyOnceWith(-4321, 0)
    kill.mockClear(); kill.mockImplementation(() => undefined as never)
    await tree.signal(false)
    expect(kill).not.toHaveBeenCalled()
  })
  it.each(['timeout', 'stop', 'exit'] as const)('F5 aborts hanging readiness fetch on %s within the phase budget', async (cause) => {
    vi.useFakeTimers()
    const child = new FakeChild()
    let signal: AbortSignal | null | undefined
    const fetchFn: typeof fetch = vi.fn((_url, init) => { signal = init?.signal; return new Promise(() => {}) })
    const launch = launcherFor(child, plan({ readyPhaseTimeoutMs: 200 }), fetchFn).launcher.launch()
    const result = expect(launch.ready).rejects.toThrow(cause === 'timeout' ? 'timed out' : 'cancelled')
    child.stdout.write(readyLine(5000))
    await vi.advanceTimersByTimeAsync(0)
    expect(signal).toBeInstanceOf(AbortSignal)
    if (cause === 'stop') await launch.tree.signal(false)
    if (cause === 'exit') child.emit('exit', 0, null)
    await vi.advanceTimersByTimeAsync(200)
    await result
    expect(signal?.aborted).toBe(true)
  })
  it('F2 records the group and removes its own pid file on exit', async () => {
    const current = plan(); const child = new FakeChild()
    const launch = launcherFor(child, current).launcher.launch()
    void launch.ready.catch(() => {})
    const file = path.join(current.runtimeRoot, 'open-design.pgid')
    expect(fs.readFileSync(file, 'utf8')).toBe('5000')
    child.emit('exit', 0, null)
    expect(fs.existsSync(file)).toBe(false)
  })
  it('F2 only reports a live leftover launcher and never signals it', () => {
    // A pid file and a ps snapshot cannot prove the id still names our group when the
    // signal lands, so a leftover launcher from an earlier run is logged, not killed.
    const current = plan()
    fs.mkdirSync(current.runtimeRoot, { recursive: true })
    const file = path.join(current.runtimeRoot, 'open-design.pgid')
    fs.writeFileSync(file, '4321')
    const kill = vi.fn()
    const warn = vi.fn()
    const ps = vi.fn(() => ` 4321 ${current.runtimeExecutable} ${current.headlessEntry} --config ${current.configPath}\n`)
    const launcher = new OpenDesignLauncher(() => current, vi.fn(() => new FakeChild() as unknown as ChildProcess) as never, okFetch(), { info: vi.fn(), warn }, 'linux', kill, ps)
    void launcher.launch().ready.catch(() => {})
    expect(ps).toHaveBeenCalledExactlyOnceWith(4321)
    // Diagnostics only probe (signal 0); any real signal on this path is a regression.
    expect(kill.mock.calls.every(([, signal]) => signal === 0)).toBe(true)
    expect(warn).toHaveBeenCalledWith('open-design.stale-launcher-running', { pid: 4321 })
  })
  it.each(['missing', 'invalid', 'dead', 'permission', 'wrong-command', 'wrong-group', 'ps-failed'])('F2 refuses stale reap when %s', (cause) => {
    const current = plan()
    fs.mkdirSync(current.runtimeRoot, { recursive: true })
    const file = path.join(current.runtimeRoot, 'open-design.pgid')
    if (cause !== 'missing') fs.writeFileSync(file, cause === 'invalid' ? '1' : '4321')
    const kill = vi.fn((_pid: number, signal: NodeJS.Signals | 0) => {
      if (signal === 0 && (cause === 'dead' || cause === 'permission')) throw errno(cause === 'dead' ? 'ESRCH' : 'EPERM')
    })
    const ps = vi.fn(() => {
      if (cause === 'ps-failed') throw new Error('ps denied')
      return `${cause === 'wrong-group' ? 9999 : 4321} ${cause === 'wrong-command' ? '/unrelated' : current.runtimeExecutable} ${current.headlessEntry} --config ${current.configPath}`
    })
    const launcher = new OpenDesignLauncher(() => current, vi.fn(() => new FakeChild()) as never, okFetch(), logger, 'linux', kill, ps)
    void launcher.launch().ready.catch(() => {})
    expect(kill.mock.calls.filter(([, signal]) => signal !== 0)).toEqual([])
  })
})


describe('N1–N3 review guards', () => {
  it.each(['ENOSPC', 'EROFS'])('N1 kills the spawned group if pid recording fails: %s', (code) => {
    const current = plan()
    const child = new FakeChild()
    const kill = vi.fn()
    const files: typeof fs = { ...fs, writeFileSync: (file, ...args) => {
      if (String(file).endsWith('open-design.pgid')) throw errno(code)
      fs.writeFileSync(file, ...args)
    } }
    const launcher = new OpenDesignLauncher(() => current, vi.fn(() => child) as never, okFetch(), logger, 'linux', kill, vi.fn(), files)
    expect(() => launcher.launch()).toThrow('Open Design startup failed')
    expect(kill).toHaveBeenCalledExactlyOnceWith(-5000, 'SIGKILL')
  })
  function stale(record = '4321') {
    const current = plan()
    fs.mkdirSync(current.runtimeRoot, { recursive: true })
    const file = path.join(current.runtimeRoot, 'open-design.pgid')
    fs.writeFileSync(file, record)
    return { current, file }
  }
  it.each(['leader-gone', 'tampered', 'leader-permission'])('N2 never reaps a leaderless or unprovable group: %s', (cause) => {
    // A group whose leader is gone has no ownership proof: member command lines that merely
    // mention the app's directories are not evidence, so nothing is signalled.
    const { current, file } = stale(cause === 'tampered' ? '9999' : '4321')
    const kill = vi.fn((pid: number, _signal: NodeJS.Signals | 0) => { if (pid > 0) throw errno(cause === 'leader-permission' ? 'EPERM' : 'ESRCH') })
    const ps = vi.fn(); const warn = vi.fn()
    reapStaleLauncher(file, current, kill, ps, { info: vi.fn(), warn })
    expect(kill.mock.calls.filter(([, signal]) => signal === 'SIGKILL')).toEqual([])
    expect(ps).not.toHaveBeenCalled()
    expect(fs.existsSync(file)).toBe(true)
    if (cause !== 'leader-permission') expect(warn).toHaveBeenCalled()
  })
  it.each(['1', '0', '1e3', '01', '-5'])('N3 rejects pid file %s before any kill or process inspection', (record) => {
    const { current, file } = stale(record)
    const kill = vi.fn(); const ps = vi.fn()
    reapStaleLauncher(file, current, kill, ps, logger)
    expect(kill).not.toHaveBeenCalled()
    expect(ps).not.toHaveBeenCalled()
  })
  it('N3 late old launcher exit preserves the new pid file', () => {
    const current = plan(); const old = new FakeChild()
    void launcherFor(old, current).launcher.launch().ready.catch(() => {})
    const next = new FakeChild(); next.pid = 5001
    void launcherFor(next, current).launcher.launch().ready.catch(() => {})
    old.emit('exit', 0, null)
    expect(fs.readFileSync(path.join(current.runtimeRoot, 'open-design.pgid'), 'utf8')).toBe('5001')
    next.emit('exit', 0, null)
  })
})
