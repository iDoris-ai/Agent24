import { EventEmitter } from 'node:events'
import { PassThrough } from 'node:stream'
import type { ChildProcess } from 'node:child_process'
import crypto from 'node:crypto'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import {
  CreativeServeWeb,
  CreativeViewRequestFence,
  OPEN_DESIGN_PIN_VERSION,
  shouldReloadCreativeView,
  loadCreativeView,
  stopWithin,
  canonicalCreativeOrigin,
  classifyCreativeUrl,
  creativeChildEnvironment,
  resolveBundledAgent24Cli,
  resolveNodeRuntimeExecutable,
  parseAgent24HeadlessReady,
  resolveAgent24HeadlessLauncher,
  resolveOpenDesignCheckout,
} from './creative-serve-web'
import { mapOpenDesignReport } from './open-design-report'

const allChildren: FakeChild[] = []

class FakeChild extends EventEmitter {
  /** Group members besides the launcher (daemon, web) are modelled as one flag. */
  groupAlive = true
  /** A launcher that ignores SIGTERM (stuck shutdown). */
  ignoreTerm = false
  /** Descendants that survive the launcher's own exit. */
  orphansOnTerm = false
  signals: Array<[number, string | number]> = []
  constructor() {
    super()
    allChildren.push(this)
  }

  exitNow(code = 0): void {
    if (this.exitCode !== null) return
    this.exitCode = code
    queueMicrotask(() => this.emit('exit', code, null))
  }

  pid = 4321
  stdout = new PassThrough()
  stderr = new PassThrough()
  stdin = new PassThrough()
  exitCode: number | null = null
  signalCode: NodeJS.Signals | null = null
  kill = vi.fn((_signal?: NodeJS.Signals | number) => {
    this.exitCode = 0
    queueMicrotask(() => this.emit('exit', 0, null))
    return true
  })
}

const tempDirs: string[] = []

function esrch(): Error {
  return Object.assign(new Error('kill ESRCH'), { code: 'ESRCH' })
}

// The sidecar path signals by pid and pgid through process.kill. Never let a fake pid reach a real
// process: a spy models the kernel for the registered FakeChild instances and rejects the rest.
beforeEach(() => {
  allChildren.length = 0
  vi.spyOn(process, 'kill').mockImplementation(((target: number, signal?: string | number) => {
    const pid = Math.abs(target)
    const child = allChildren.find((candidate) => candidate.pid === pid)
    if (!child) throw esrch()
    child.signals.push([target, signal ?? 'SIGTERM'])
    if (target > 0) {
      if (signal === 'SIGTERM' && !child.ignoreTerm) {
        child.exitNow()
        if (!child.orphansOnTerm) child.groupAlive = false
      }
      return true
    }
    if (!child.groupAlive) throw esrch()
    if (signal === 'SIGKILL') {
      child.groupAlive = false
      child.exitNow(137)
    }
    return true
  }) as typeof process.kill)
})

function materializeCheckout(root: string): string {
  fs.mkdirSync(path.join(root, 'apps/daemon/bin'), { recursive: true })
  fs.mkdirSync(path.join(root, 'apps/daemon/dist'), { recursive: true })
  fs.mkdirSync(path.join(root, 'apps/web/out'), { recursive: true })
  fs.writeFileSync(path.join(root, 'apps/daemon/bin/od.mjs'), '')
  fs.writeFileSync(path.join(root, 'apps/daemon/dist/cli.js'), '')
  fs.writeFileSync(path.join(root, 'apps/web/out/index.html'), '<!doctype html>')
  return root
}

function checkout(): string {
  const root = fs.mkdtempSync(path.join(process.cwd(), '.tmp-creative-'))
  tempDirs.push(root)
  return materializeCheckout(root)
}

// M4 (Opus re-review, 2026-10-06): the four launcher entry files and a
// matching integrity marker — mirrors exactly what
// OpenDesignComponentInstaller.installOnce actually writes at install
// time (see ENTRY_FILES_FOR_INTEGRITY_CHECK / verifyEntryFileIntegrity in
// open-design-component.ts), since creative-serve-web.ts's on-demand path
// now spot-checks this marker before every launch.
const ENTRY_RELATIVE_PATHS = [
  'app/prebundled/agent24-headless.cjs',
  'app/prebundled/daemon/daemon-cli.mjs',
  'app/prebundled/daemon/daemon-sidecar.mjs',
  'app/prebundled/web-sidecar.mjs',
] as const

function packagedHeadlessRuntime(): { resources: string; stateRoot: string; runtimeExecutable: string } {
  const root = fs.mkdtempSync(path.join(process.cwd(), '.tmp-creative-headless-'))
  tempDirs.push(root)
  const resources = path.join(root, 'resources')
  const stateRoot = path.join(root, 'state')
  const runtimeExecutable = path.join(root, 'Agent24')
  fs.mkdirSync(path.join(resources, 'app', 'prebundled', 'daemon'), { recursive: true })
  fs.mkdirSync(path.join(resources, 'open-design'), { recursive: true })
  const entrySha256: Record<string, string> = {}
  for (const relative of ENTRY_RELATIVE_PATHS) {
    const content = `// fixture: ${relative}\n`
    fs.writeFileSync(path.join(resources, relative), content)
    entrySha256[relative] = crypto.createHash('sha256').update(content).digest('hex')
  }
  fs.writeFileSync(
    path.join(resources, '.agent24-component.json'),
    JSON.stringify({ sha256: 'fixture-tarball-sha256', entrySha256 }),
  )
  fs.writeFileSync(runtimeExecutable, '')
  return { resources, stateRoot, runtimeExecutable }
}

function headlessReady(ownerPid: number, webPort: number, daemonPort: number, instanceId = 'creative-1') {
  return {
    type: 'ready', protocol: 2, instanceId, pinVersion: OPEN_DESIGN_PIN_VERSION,
    webOrigin: `http://127.0.0.1:${webPort}`, daemonOrigin: `http://127.0.0.1:${daemonPort}`,
    ownership: { ownerPid, kind: 'process-tree' },
  }
}

afterEach(() => {
  vi.restoreAllMocks()
  vi.useRealTimers()
  for (const dir of tempDirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true })
})

describe('CreativeServeWeb', () => {
  it('invalidates an in-flight view request when the view is hidden', () => {
    const fence = new CreativeViewRequestFence()
    const first = fence.begin()
    expect(fence.isCurrent(first)).toBe(true)

    fence.invalidate()
    expect(fence.isCurrent(first)).toBe(false)
    expect(fence.wantsVisible()).toBe(false)

    const second = fence.begin()
    expect(fence.isCurrent(second)).toBe(true)
    expect(fence.isCurrent(first)).toBe(false)
  })

  it('canonicalizes only the launched loopback origin', () => {
    expect(canonicalCreativeOrigin('http://127.0.0.1:17456/', 17456)).toBe('http://127.0.0.1:17456')
    expect(canonicalCreativeOrigin('https://127.0.0.1:17456', 17456)).toBe('https://127.0.0.1:17456')

    for (const raw of [
      'http://example.com:17456',
      'http://localhost:17456',
      'http://user@127.0.0.1:17456',
      'http://127.0.0.1:17456/path',
      'http://127.0.0.1:17456/?query=1',
      'http://127.0.0.1:17456/#fragment',
      'http://127.0.0.1:17457',
      'file:///tmp/open-design',
    ]) {
      expect(() => canonicalCreativeOrigin(raw, 17456), raw).toThrow()
    }
  })

  it('classifies exact-origin navigation without prefix trust', () => {
    const origin = 'http://127.0.0.1:17456'
    expect(classifyCreativeUrl(`${origin}/project/1?tab=files#active`, origin)).toBe('same-origin')
    expect(classifyCreativeUrl('https://example.com/docs', origin)).toBe('external-http')
    expect(classifyCreativeUrl('http://127.0.0.1:17457/project', origin)).toBe('external-http')
    expect(classifyCreativeUrl('https://127.0.0.1:17456/project', origin)).toBe('external-http')
    expect(classifyCreativeUrl('http://127.0.0.1.evil.example:17456/project', origin)).toBe('external-http')
    expect(classifyCreativeUrl('blob:http://127.0.0.1:17456/preview', origin)).toBe('blocked')
    expect(classifyCreativeUrl('javascript:alert(1)', origin)).toBe('blocked')
    expect(classifyCreativeUrl('file:///tmp/secret', origin)).toBe('blocked')
    expect(classifyCreativeUrl('not a url', origin)).toBe('blocked')
  })

  it('uses the explicit Open Design checkout when it contains the od entry', () => {
    const root = checkout()
    expect(resolveOpenDesignCheckout('/unused', { A24_OPEN_DESIGN_DIR: root })).toBe(root)
  })

  it('discovers the packaged Open Design runtime under Electron resources', () => {
    const resources = fs.mkdtempSync(path.join(process.cwd(), '.tmp-creative-resources-'))
    tempDirs.push(resources)
    const packaged = materializeCheckout(path.join(resources, 'open-design'))

    expect(resolveOpenDesignCheckout('/unused', {}, resources)).toBe(packaged)
  })

  it('keeps the explicit checkout override ahead of packaged resources', () => {
    const explicit = checkout()
    const resources = fs.mkdtempSync(path.join(process.cwd(), '.tmp-creative-resources-'))
    tempDirs.push(resources)
    materializeCheckout(path.join(resources, 'open-design'))

    expect(resolveOpenDesignCheckout('/unused', { A24_OPEN_DESIGN_DIR: explicit }, resources)).toBe(explicit)
  })

  it('discovers only the stable packaged Agent24 headless launcher path', () => {
    const { resources } = packagedHeadlessRuntime()
    expect(resolveAgent24HeadlessLauncher(resources)).toBe(
      path.join(resources, 'app', 'prebundled', 'agent24-headless.cjs'),
    )
    expect(resolveAgent24HeadlessLauncher(path.join(resources, 'missing'))).toBeNull()
  })

  it('validates the exact headless readiness contract', () => {
    const ready = headlessReady(4321, 17456, 17457)
    expect(parseAgent24HeadlessReady(ready, OPEN_DESIGN_PIN_VERSION, 4321)).toEqual(ready)
    expect(() => parseAgent24HeadlessReady({ ...ready, pinVersion: 'other' }, OPEN_DESIGN_PIN_VERSION, 4321)).toThrow('pin version')
    expect(() => parseAgent24HeadlessReady({ ...ready, ownership: { ...ready.ownership, ownerPid: 999 } }, OPEN_DESIGN_PIN_VERSION, 4321)).toThrow('ownership')
    expect(() => parseAgent24HeadlessReady({ ...ready, webOrigin: 'http://example.com:17456' }, OPEN_DESIGN_PIN_VERSION, 4321)).toThrow('127.0.0.1')
    expect(() => parseAgent24HeadlessReady({ ...ready, secret: 'must-not-exist' }, OPEN_DESIGN_PIN_VERSION, 4321)).toThrow('unsupported field')
  })

  it('removes Agent24 host variables from the Creative child environment', () => {
    const childEnv = creativeChildEnvironment({
      PATH: '/usr/bin',
      OPENAI_API_KEY: 'provider-secret',
      A24_HANDSHAKE_TOKEN: 'host-secret',
      a24_host_bootstrap: 'also-secret',
    })
    expect(childEnv).toEqual({
      PATH: '/usr/bin',
      OPENAI_API_KEY: 'provider-secret',
    })
    expect(Object.getPrototypeOf(childEnv)).toBeNull()
  })

  it('does not reintroduce inherited A24 variables when spawn enumerates the env', () => {
    Object.defineProperty(Object.prototype, 'a24_polluted_host_secret', {
      configurable: true,
      enumerable: true,
      value: 'leaked-if-enumerated',
      writable: true,
    })
    try {
      const childEnv = creativeChildEnvironment({ PATH: '/usr/bin' })
      const enumerated = [] as string[]
      for (const key in childEnv) enumerated.push(key)
      expect(enumerated).toEqual(['PATH'])
    } finally {
      delete (Object.prototype as Record<string, unknown>).a24_polluted_host_secret
    }
  })

  it('launches serve-web, waits for /api/ready, and stops with SIGTERM', async () => {
    const root = checkout()
    const child = new FakeChild()
    const spawnFn = vi.fn(() => child as unknown as ChildProcess)
    const fetchFn = vi.fn(async () => new Response(JSON.stringify({ ok: true }), { status: 200 }))
    const service = new CreativeServeWeb(
      {
        checkoutDir: root,
        environment: {
          PATH: '/usr/bin',
          OPENAI_API_KEY: 'provider-secret',
          A24_HANDSHAKE_TOKEN: 'host-secret',
        },
        port: 17456,
        readyTimeoutMs: 500,
      },
      spawnFn as never,
      fetchFn,
    )

    const starting = service.start()
    child.stdout.write('[od] listening on http://127.0.0.1:17456 (headless)\n')

    await expect(starting).resolves.toEqual({ state: 'ready', origin: 'http://127.0.0.1:17456' })
    expect(spawnFn).toHaveBeenCalledWith(
      'node',
      expect.arrayContaining(['daemon', 'start', '--serve-web', '--no-open', '--port', '17456']),
      expect.objectContaining({
        cwd: root,
        env: {
          PATH: '/usr/bin',
          OPENAI_API_KEY: 'provider-secret',
        },
      }),
    )
    expect(fetchFn).toHaveBeenCalledWith('http://127.0.0.1:17456/api/ready')

    await expect(service.stop()).resolves.toEqual({ state: 'stopped' })
    expect(child.kill).toHaveBeenCalledWith('SIGTERM')
  })

  it('launches the packaged headless adapter with host-managed roots and a sanitized environment', async () => {
    const { resources, stateRoot, runtimeExecutable } = packagedHeadlessRuntime()
    const child = new FakeChild()
    const spawnFn = vi.fn(() => child as unknown as ChildProcess)
    const fetchFn = vi.fn(async () => new Response('{}', { status: 200 }))
    const service = new CreativeServeWeb(
      {
        resourcesPath: resources,
        stateRoot,
        runtimeExecutable,
        environment: {
          PATH: '/usr/bin',
          OPENAI_API_KEY: 'provider-secret',
          A24_HANDSHAKE_TOKEN: 'host-secret',
        },
        readyTimeoutMs: 500,
      },
      spawnFn as never,
      fetchFn,
    )

    const starting = service.start()
    child.stdout.write(`${JSON.stringify(headlessReady(child.pid, 17456, 17457))}\n`)

    await expect(starting).resolves.toEqual({ state: 'ready', origin: 'http://127.0.0.1:17456', instanceId: expect.any(String) })
    const configPath = path.join(stateRoot, 'agent24-headless.json')
    expect(spawnFn).toHaveBeenCalledWith(
      runtimeExecutable,
      [path.join(resources, 'app', 'prebundled', 'agent24-headless.cjs'), '--config', configPath],
      expect.objectContaining({
        cwd: resources,
        env: {
          PATH: '/usr/bin',
          OPENAI_API_KEY: 'provider-secret',
          ELECTRON_RUN_AS_NODE: '1',
        },
        // The launcher leads its own process group so the tree can be reaped without touching ours.
        detached: true,
      }),
    )
    expect(JSON.parse(fs.readFileSync(configPath, 'utf8'))).toEqual({
      protocol: 2,
      pinVersion: OPEN_DESIGN_PIN_VERSION,
      resourceRoot: path.join(resources, 'open-design'),
      dataRoot: path.join(stateRoot, 'data'),
      runtimeRoot: path.join(stateRoot, 'runtime'),
      runtimeExecutable,
      // Legacy bundled-resources path: no resourceSafeBase (the fork's v1
      // runtimeExecutable-derived default still applies).
    })
    expect(fetchFn).toHaveBeenCalledWith('http://127.0.0.1:17456/api/ready', expect.objectContaining({ signal: expect.any(AbortSignal) }))
    await service.stop()
    // Graceful stop signals the launcher pid only, never the group.
    expect(child.signals).toEqual([[-4321, 0], [4321, 'SIGTERM'], [-4321, 0]])
    expect(child.kill).not.toHaveBeenCalled()
  })

  it('fails closed on an invalid packaged ready line before probing the web origin', async () => {
    const { resources, stateRoot, runtimeExecutable } = packagedHeadlessRuntime()
    const child = new FakeChild()
    const fetchFn = vi.fn(async () => new Response('{}', { status: 200 }))
    const service = new CreativeServeWeb(
      { resourcesPath: resources, stateRoot, runtimeExecutable, readyTimeoutMs: 500 },
      vi.fn(() => child as unknown as ChildProcess) as never,
      fetchFn,
    )
    const starting = service.start()
    child.stdout.write(`${JSON.stringify({ ...headlessReady(child.pid, 17456, 17457), webOrigin: 'http://example.com:17456' })}\n`)
    await expect(starting).resolves.toMatchObject({ state: 'failed', error: expect.stringContaining('127.0.0.1') })
    expect(fetchFn).not.toHaveBeenCalled()
    expect(child.signals.filter(([, signal]) => signal !== 0)[0]).toEqual([4321, 'SIGTERM'])
    expect(service.headlessSidecarState()).toBe('failed')
  })

  it('allows restart after the packaged owner exits by signal during readiness', async () => {
    const { resources, stateRoot, runtimeExecutable } = packagedHeadlessRuntime()
    const first = new FakeChild()
    const second = new FakeChild()
    second.pid = 4322
    const spawnFn = vi.fn()
      .mockReturnValueOnce(first as unknown as ChildProcess)
      .mockReturnValueOnce(second as unknown as ChildProcess)
    const fetchFn = vi.fn(async () => new Response('{}', { status: 200 }))
    const service = new CreativeServeWeb(
      { resourcesPath: resources, stateRoot, runtimeExecutable, readyTimeoutMs: 500 },
      spawnFn as never,
      fetchFn,
    )

    const staleStart = service.start()
    first.signalCode = 'SIGTERM'
    first.emit('exit', null, 'SIGTERM')
    await expect(staleStart).resolves.toMatchObject({ state: 'failed', error: expect.stringContaining('exited before ready') })
    expect(first.kill).not.toHaveBeenCalled()
    // The launcher died by itself; whatever it left in its group is reaped by pgid.
    expect(first.signals).toContainEqual([-4321, 'SIGKILL'])
    const restarted = service.start()
    second.stdout.write(`${JSON.stringify(headlessReady(second.pid, 17458, 17459, 'creative-2'))}\n`)
    await expect(restarted).resolves.toEqual({ state: 'ready', origin: 'http://127.0.0.1:17458', instanceId: expect.any(String) })
    expect(spawnFn).toHaveBeenCalledTimes(2)
    await service.stop()
  })

  it('serializes starts behind a slow packaged stop, then SIGKILLs the whole group after the grace period', async () => {
    const { resources, stateRoot, runtimeExecutable } = packagedHeadlessRuntime()
    const child = new FakeChild()
    child.ignoreTerm = true
    const next = new FakeChild()
    next.pid = 4322
    const spawnFn = vi.fn().mockReturnValueOnce(child as unknown as ChildProcess).mockReturnValueOnce(next as unknown as ChildProcess)
    const service = new CreativeServeWeb(
      { resourcesPath: resources, stateRoot, runtimeExecutable, readyTimeoutMs: 500 },
      spawnFn as never,
      vi.fn(async () => new Response('{}', { status: 200 })),
    )
    const starting = service.start()
    child.stdout.write(`${JSON.stringify(headlessReady(child.pid, 17460, 17461, 'creative-slow'))}\n`)
    await expect(starting).resolves.toMatchObject({ state: 'ready', origin: 'http://127.0.0.1:17460' })

    vi.useFakeTimers()
    const stopping = service.stop()
    const concurrentStart = service.start()
    expect(spawnFn).toHaveBeenCalledTimes(1)
    expect(service.headlessSidecarState()).toBe('stopping')
    // Inside the 15 s grace nothing but the launcher's SIGTERM has been sent.
    await vi.advanceTimersByTimeAsync(14_000)
    expect(child.signals.filter(([, signal]) => signal === 'SIGKILL')).toEqual([])
    await vi.advanceTimersByTimeAsync(1_000)
    await expect(stopping).resolves.toEqual({ state: 'stopped' })
    expect(child.signals.filter(([, signal]) => signal !== 0)).toEqual([[4321, 'SIGTERM'], [-4321, 'SIGKILL']])
    expect(child.kill).not.toHaveBeenCalled()
    // The queued start proceeds only after the stop settled, and gets a fresh launch.
    expect(spawnFn).toHaveBeenCalledTimes(2)
    expect(service.headlessSidecarState()).toBe('starting')
    vi.useRealTimers()
    next.stdout.write(`${JSON.stringify(headlessReady(next.pid, 17462, 17463, 'creative-next'))}\n`)
    await expect(concurrentStart).resolves.toMatchObject({ state: 'ready', origin: 'http://127.0.0.1:17462' })
    await service.stop()
  })

  it('reports failed and refuses to start over an instance whose group cannot be emptied', async () => {
    const { resources, stateRoot, runtimeExecutable } = packagedHeadlessRuntime()
    const child = new FakeChild()
    child.ignoreTerm = true
    // SIGKILL "succeeds" but the group never empties (an uninterruptible member).
    vi.mocked(process.kill).mockImplementation(((target: number, signal?: string | number) => {
      child.signals.push([target, signal ?? 'SIGTERM'])
      if (target < 0 && signal === 0) return true
      return true
    }) as typeof process.kill)
    const spawnFn = vi.fn(() => child as unknown as ChildProcess)
    const service = new CreativeServeWeb(
      { resourcesPath: resources, stateRoot, runtimeExecutable, readyTimeoutMs: 500, sidecarTimings: { termGraceMs: 20, killAfterMs: 20 } },
      spawnFn as never,
      vi.fn(async () => new Response('{}', { status: 200 })),
    )
    const starting = service.start()
    child.stdout.write(`${JSON.stringify(headlessReady(child.pid, 17460, 17461))}\n`)
    await starting

    await expect(service.stop()).resolves.toMatchObject({ state: 'failed', error: expect.stringContaining('did not exit') })
    expect(service.headlessSidecarState()).toBe('failed')
    expect(service.hasLiveProcess()).toBe(true)
    // The retained owner blocks a replacement: no second launch while the old tree may still run.
    await expect(service.start()).resolves.toMatchObject({ state: 'failed', error: expect.stringContaining('has not stopped') })
    expect(spawnFn).toHaveBeenCalledTimes(1)
  })

  it('shares one in-flight launch across concurrent start calls', async () => {
    const root = checkout()
    const child = new FakeChild()
    const spawnFn = vi.fn(() => child as unknown as ChildProcess)
    const fetchFn = vi.fn(async () => new Response('{}', { status: 200 }))
    const service = new CreativeServeWeb(
      { checkoutDir: root, port: 17456, readyTimeoutMs: 500 },
      spawnFn as never,
      fetchFn,
    )

    const first = service.start()
    const second = service.start()
    expect(spawnFn).toHaveBeenCalledTimes(1)

    child.stdout.write('[od] listening on http://127.0.0.1:17456 (headless)\n')
    await expect(Promise.all([first, second])).resolves.toEqual([
      { state: 'ready', origin: 'http://127.0.0.1:17456' },
      { state: 'ready', origin: 'http://127.0.0.1:17456' },
    ])
    expect(spawnFn).toHaveBeenCalledTimes(1)
    expect(service.headlessSidecarState()).toBeNull()
    await service.stop()
  })

  it('rejects an untrusted readiness origin before any host fetch', async () => {
    const root = checkout()
    const child = new FakeChild()
    const spawnFn = vi.fn(() => child as unknown as ChildProcess)
    const fetchFn = vi.fn(async () => new Response('{}', { status: 200 }))
    const service = new CreativeServeWeb(
      { checkoutDir: root, port: 17456, readyTimeoutMs: 500 },
      spawnFn as never,
      fetchFn,
    )

    const starting = service.start()
    child.stdout.write('[od] listening on https://example.com:17456 (headless)\n')

    await expect(starting).resolves.toEqual({
      state: 'failed',
      error: 'Open Design origin must use 127.0.0.1',
    })
    expect(fetchFn).not.toHaveBeenCalled()
    expect(child.kill).toHaveBeenCalledWith('SIGTERM')
  })

  it('reports needs-download with the manifest size when the on-demand component is not installed', async () => {
    const emptyResources = fs.mkdtempSync(path.join(process.cwd(), '.tmp-creative-empty-resources-'))
    tempDirs.push(emptyResources)
    const service = new CreativeServeWeb({
      resourcesPath: emptyResources,
      component: { status: () => ({ state: 'not-installed', size: 123_456_789 }) },
    })

    await expect(service.start()).resolves.toEqual({ state: 'needs-download', size: 123_456_789 })
  })

  it('C1 (Opus review of #670, 2026-10-05): uses the installed component dir directly, with its realpath as resourceSafeBase — no symlink, no write into resourcesPath', async () => {
    // The previous fix wrote a symlink into the app's own resourcesPath at
    // runtime (EROFS on a read-only AppImage mount, EACCES on a root-owned
    // deb /opt install, breaks a signed mac bundle). The real fix is
    // fork-side (iDoris-ai/open-design-agent24#7, protocol v2's explicit
    // resourceSafeBase) — this process never writes into resourcesPath for
    // the on-demand case at all now.
    const { resources: componentDir, stateRoot, runtimeExecutable } = packagedHeadlessRuntime()
    const child = new FakeChild()
    const spawnFn = vi.fn(() => child as unknown as ChildProcess)
    const fetchFn = vi.fn(async () => new Response('{}', { status: 200 }))
    const service = new CreativeServeWeb(
      {
        stateRoot,
        runtimeExecutable,
        readyTimeoutMs: 500,
        component: { status: () => ({ state: 'installed', dir: componentDir }) },
      },
      spawnFn as never,
      fetchFn,
    )

    const starting = service.start()
    child.stdout.write(`${JSON.stringify(headlessReady(child.pid, 17456, 17457))}\n`)
    await expect(starting).resolves.toEqual({ state: 'ready', origin: 'http://127.0.0.1:17456', instanceId: expect.any(String) })
    expect(spawnFn).toHaveBeenCalledWith(
      runtimeExecutable,
      [path.join(componentDir, 'app', 'prebundled', 'agent24-headless.cjs'), '--config', path.join(stateRoot, 'agent24-headless.json')],
      expect.objectContaining({ cwd: componentDir }),
    )
    const configPath = path.join(stateRoot, 'agent24-headless.json')
    expect(JSON.parse(fs.readFileSync(configPath, 'utf8'))).toMatchObject({
      protocol: 2,
      resourceSafeBase: fs.realpathSync(componentDir),
    })
    await service.stop()
  })

  it('hands the bundled agent24 CLI to Open Design as AGENT24_BIN, keeping a user override', async () => {
    for (const [userBin, expected] of [[undefined, '/Apps/Agent24.app/Contents/Resources/backend/agent24'], ['/opt/mine/agent24', '/opt/mine/agent24']] as const) {
      const { resources, stateRoot, runtimeExecutable } = packagedHeadlessRuntime()
      const child = new FakeChild()
      const spawnFn = vi.fn(() => child as unknown as ChildProcess)
      const service = new CreativeServeWeb(
        {
          resourcesPath: resources,
          stateRoot,
          runtimeExecutable,
          environment: { PATH: '/usr/bin', ...(userBin ? { AGENT24_BIN: userBin } : {}) },
          agent24Cli: '/Apps/Agent24.app/Contents/Resources/backend/agent24',
          readyTimeoutMs: 500,
        },
        spawnFn as never,
        vi.fn(async () => new Response('{}', { status: 200 })),
      )
      const starting = service.start()
      child.stdout.write(`${JSON.stringify(headlessReady(child.pid, 17456, 17457))}\n`)
      await starting
      const env = (spawnFn.mock.calls[0] as unknown as [string, string[], { env: NodeJS.ProcessEnv }])[2].env
      expect(env.AGENT24_BIN).toBe(expected)
      await service.stop()
    }
  })

  it('runs Open Design children under the LSUIElement Helper on macOS so none of them gets a Dock tile', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-helper-'))
    try {
      const execPath = path.join(root, 'Agent24.app', 'Contents', 'MacOS', 'Agent24')
      const helper = path.join(root, 'Agent24.app', 'Contents', 'Frameworks', 'Agent24 Helper.app', 'Contents', 'MacOS', 'Agent24 Helper')
      expect(resolveNodeRuntimeExecutable(execPath, 'darwin')).toBe(execPath)
      fs.mkdirSync(path.dirname(helper), { recursive: true })
      fs.writeFileSync(helper, '')
      expect(resolveNodeRuntimeExecutable(execPath, 'darwin')).toBe(helper)
      expect(resolveNodeRuntimeExecutable(execPath, 'linux')).toBe(execPath)
      expect(resolveNodeRuntimeExecutable(execPath, 'win32')).toBe(execPath)
    } finally {
      fs.rmSync(root, { recursive: true, force: true })
    }
  })

  it('finds the CLI bundled in Agent24.app and nothing when it is absent', () => {
    const resources = fs.mkdtempSync(path.join(os.tmpdir(), 'a24-res-'))
    try {
      expect(resolveBundledAgent24Cli(resources)).toBeNull()
      fs.mkdirSync(path.join(resources, 'backend'))
      fs.writeFileSync(path.join(resources, 'backend', 'agent24'), '')
      expect(resolveBundledAgent24Cli(resources)).toBe(path.join(resources, 'backend', 'agent24'))
      expect(resolveBundledAgent24Cli(undefined)).toBeNull()
    } finally {
      fs.rmSync(resources, { recursive: true, force: true })
    }
  })

  it('reports headless sidecar phases and clears them after a clean stop', async () => {
    const { resources: componentDir, stateRoot, runtimeExecutable } = packagedHeadlessRuntime()
    const child = new FakeChild()
    const service = new CreativeServeWeb(
      {
        stateRoot,
        runtimeExecutable,
        readyTimeoutMs: 500,
        component: { status: () => ({ state: 'installed', dir: componentDir }) },
      },
      vi.fn(() => child as unknown as ChildProcess) as never,
      vi.fn(async () => new Response('{}', { status: 200 })),
    )
    const starting = service.start()
    expect(service.headlessSidecarState()).toBe('starting')
    child.stdout.write(`${JSON.stringify(headlessReady(child.pid, 17456, 17457))}\n`)
    await expect(starting).resolves.toEqual({ state: 'ready', origin: 'http://127.0.0.1:17456', instanceId: expect.any(String) })
    expect(service.headlessSidecarState()).toBe('healthy')
    child.emit('exit', 1, null)
    await Promise.resolve()
    expect(service.headlessSidecarState()).toBe('failed')
    expect(service.status()).toEqual({ state: 'stopped' })
  })

  it('reports stopped after a clean headless stop, then starting on the next launch', async () => {
    const { resources: componentDir, stateRoot, runtimeExecutable } = packagedHeadlessRuntime()
    const first = new FakeChild()
    const second = new FakeChild()
    second.pid = 4322
    const children = [first, second]
    let spawned = 0
    const service = new CreativeServeWeb(
      {
        stateRoot,
        runtimeExecutable,
        readyTimeoutMs: 500,
        component: { status: () => ({ state: 'installed', dir: componentDir }) },
      },
      vi.fn(() => children[spawned++] as unknown as ChildProcess) as never,
      vi.fn(async () => new Response('{}', { status: 200 })),
    )
    const phases: Array<string | null> = []
    service.onStatus(() => { phases.push(service.headlessSidecarState()) })
    const starting = service.start()
    first.stdout.write(`${JSON.stringify(headlessReady(first.pid, 17456, 17457))}\n`)
    await starting
    const stopping = service.stop()
    expect(service.headlessSidecarState()).toBe('stopping')
    // Stop clears `this.child` before the process is gone. An upgrade snapshot
    // in that window must not drop `stopping`.
    expect(service.headlessPhaseForReport('downloading')).toBe('stopping')
    expect(service.status()).toEqual({ state: 'stopped' })
    await stopping
    expect(service.headlessSidecarState()).toBe('stopped')
    expect(phases.at(-1)).toBe('stopped')
    expect(phases).not.toContain(null)

    const again = service.start()
    expect(service.headlessSidecarState()).toBe('starting')
    second.stdout.write(`${JSON.stringify(headlessReady(second.pid, 17458, 17459, 'creative-2'))}\n`)
    await expect(again).resolves.toEqual({ state: 'ready', origin: 'http://127.0.0.1:17458', instanceId: expect.any(String) })
    expect(service.headlessSidecarState()).toBe('healthy')
    await service.stop()
    expect(service.headlessSidecarState()).toBe('stopped')
  })

  it('clears a leftover stopped phase when the component leaves installed, so reinstall reports installed', async () => {
    const { resources: componentDir, stateRoot, runtimeExecutable } = packagedHeadlessRuntime()
    const first = new FakeChild()
    const second = new FakeChild()
    second.pid = 4322
    const children = [first, second]
    let spawned = 0
    const service = new CreativeServeWeb(
      {
        stateRoot,
        runtimeExecutable,
        readyTimeoutMs: 500,
        component: { status: () => ({ state: 'installed', dir: componentDir }) },
      },
      vi.fn(() => children[spawned++] as unknown as ChildProcess) as never,
      vi.fn(async () => new Response('{}', { status: 200 })),
    )
    const starting = service.start()
    first.stdout.write(`${JSON.stringify(headlessReady(first.pid, 17456, 17457))}\n`)
    await starting
    await service.stop()
    expect(service.headlessSidecarState()).toBe('stopped')
    // Still installed: a clean stop stays stopped (D5 `installed` + sidecar).
    expect(service.headlessPhaseForReport('installed')).toBe('stopped')
    expect(service.headlessSidecarState()).toBe('stopped')

    // Uninstall clears the leftover. The next installed report is `installed`,
    // which would be `stopped` if the phase were left in place.
    expect(service.headlessPhaseForReport('not-installed')).toBeNull()
    expect(service.headlessSidecarState()).toBeNull()
    expect(mapOpenDesignReport({
      installState: 'installed',
      version: OPEN_DESIGN_PIN_VERSION,
      sidecar: service.headlessSidecarState(),
    })).toEqual({ state: 'installed', version: OPEN_DESIGN_PIN_VERSION })

    // A later run can stop again. A download that skips `not-installed` clears too.
    const again = service.start()
    second.stdout.write(`${JSON.stringify(headlessReady(second.pid, 17458, 17459, 'creative-2'))}\n`)
    await again
    await service.stop()
    expect(service.headlessSidecarState()).toBe('stopped')
    expect(service.headlessPhaseForReport('downloading')).toBeNull()
    expect(mapOpenDesignReport({
      installState: 'downloading',
      version: OPEN_DESIGN_PIN_VERSION,
      sidecar: service.headlessSidecarState(),
    })).toEqual({ state: 'installing' })
    expect(mapOpenDesignReport({
      installState: 'installed',
      version: OPEN_DESIGN_PIN_VERSION,
      sidecar: service.headlessSidecarState(),
    })).toEqual({ state: 'installed', version: OPEN_DESIGN_PIN_VERSION })
  })

  it('keeps a live ready phase through an upgrade, and records a crash during that upgrade as failed', async () => {
    const { resources: componentDir, stateRoot, runtimeExecutable } = packagedHeadlessRuntime()
    const child = new FakeChild()
    const service = new CreativeServeWeb(
      {
        stateRoot,
        runtimeExecutable,
        readyTimeoutMs: 500,
        component: { status: () => ({ state: 'installed', dir: componentDir }) },
      },
      vi.fn(() => child as unknown as ChildProcess) as never,
      vi.fn(async () => new Response('{}', { status: 200 })),
    )
    // main.ts reports on every status assignment. While an upgrade is in
    // progress that snapshot still sees `downloading`, not `installed`.
    let installState = 'installed'
    service.onStatus(() => { service.headlessPhaseForReport(installState) })

    const starting = service.start()
    child.stdout.write(`${JSON.stringify(headlessReady(child.pid, 17456, 17457))}\n`)
    await starting
    expect(service.headlessSidecarState()).toBe('healthy')

    installState = 'downloading'
    expect(service.headlessPhaseForReport('downloading')).toBe('healthy')
    expect(service.headlessPhaseForReport('verifying')).toBe('healthy')
    expect(service.headlessSidecarState()).toBe('healthy')

    child.emit('exit', 1, null)
    await Promise.resolve()
    expect(service.headlessSidecarState()).toBe('failed')
    // The crash's own status event already called headlessPhaseForReport('downloading').
    // A later progress tick must still see the failure, and so must the
    // snapshot once the install is back.
    expect(service.headlessPhaseForReport('downloading')).toBe('failed')
    expect(service.headlessPhaseForReport('paused')).toBe('failed')
    expect(service.headlessPhaseForReport('installed')).toBe('failed')
    expect(mapOpenDesignReport({
      installState: 'installed',
      version: OPEN_DESIGN_PIN_VERSION,
      sidecar: service.headlessSidecarState(),
    })).toEqual({ state: 'failed', version: OPEN_DESIGN_PIN_VERSION })

    // Uninstall drops a failed phase only once the manager no longer owns the dead instance.
    expect(service.headlessPhaseForReport('not-installed')).toBe('failed')
    await vi.waitFor(() => expect(service.hasLiveProcess()).toBe(false))
    expect(service.headlessPhaseForReport('not-installed')).toBeNull()
    expect(service.headlessSidecarState()).toBeNull()
    expect(service.headlessPhaseForReport('unavailable')).toBeNull()
  })

  it('C1: Design still starts when resourcesPath is unset entirely (proves no dependency on a writable — or any — app resources dir for the on-demand path)', async () => {
    // Stands in for "resourcesPath is read-only" (a real-only-readable
    // AppImage FUSE mount, a root-owned deb /opt install, a signed mac
    // bundle): if the code never touches resourcesPath, it doesn't matter
    // whether that path exists, is writable, or is set at all.
    const { resources: componentDir, stateRoot, runtimeExecutable } = packagedHeadlessRuntime()
    const child = new FakeChild()
    const spawnFn = vi.fn(() => child as unknown as ChildProcess)
    const fetchFn = vi.fn(async () => new Response('{}', { status: 200 }))
    const service = new CreativeServeWeb(
      {
        stateRoot,
        runtimeExecutable,
        readyTimeoutMs: 500,
        component: { status: () => ({ state: 'installed', dir: componentDir }) },
        // resourcesPath intentionally omitted.
      },
      spawnFn as never,
      fetchFn,
    )
    const starting = service.start()
    child.stdout.write(`${JSON.stringify(headlessReady(child.pid, 17456, 17457))}\n`)
    await expect(starting).resolves.toEqual({ state: 'ready', origin: 'http://127.0.0.1:17456', instanceId: expect.any(String) })
    await service.stop()
  })

  it('M2 closed-loop finding (2026-10-05): resourceRoot and resourceSafeBase are anchored to the same base even when the installed component dir is reached through a symlink', async () => {
    // Caught by actually running the real packaged Electron binary against
    // the real installer (apps/desktop/scripts/ci-on-demand-smoke.mjs) on
    // macOS, where tmp dirs route through /tmp -> /private/tmp: with
    // resourceRoot built from the raw (symlinked) dir while
    // resourceSafeBase used fs.realpathSync, the fork's own v2 check
    // (plain path.relative — see iDoris-ai/open-design-agent24#7) saw the
    // two disagree and rejected every on-demand launch. Reproduced here
    // without touching the real filesystem's /tmp — a plain in-tree
    // symlink hits the same code path.
    const { resources: componentDir, stateRoot, runtimeExecutable } = packagedHeadlessRuntime()
    const componentDirLink = path.join(path.dirname(componentDir), 'component-via-symlink')
    fs.symlinkSync(componentDir, componentDirLink)
    tempDirs.push(componentDirLink)
    const child = new FakeChild()
    const spawnFn = vi.fn(() => child as unknown as ChildProcess)
    const fetchFn = vi.fn(async () => new Response('{}', { status: 200 }))
    const service = new CreativeServeWeb(
      {
        stateRoot,
        runtimeExecutable,
        readyTimeoutMs: 500,
        component: { status: () => ({ state: 'installed', dir: componentDirLink }) },
      },
      spawnFn as never,
      fetchFn,
    )

    const starting = service.start()
    child.stdout.write(`${JSON.stringify(headlessReady(child.pid, 17456, 17457))}\n`)
    await expect(starting).resolves.toEqual({ state: 'ready', origin: 'http://127.0.0.1:17456', instanceId: expect.any(String) })
    const realBase = fs.realpathSync(componentDir)
    const configPath = path.join(stateRoot, 'agent24-headless.json')
    expect(JSON.parse(fs.readFileSync(configPath, 'utf8'))).toMatchObject({
      resourceRoot: path.join(realBase, 'open-design'),
      resourceSafeBase: realBase,
    })
    await service.stop()
  })

  it('does not consult the on-demand component accessor when an explicit dev checkout is set', async () => {
    const root = checkout()
    const statusFn = vi.fn(() => ({ state: 'not-installed' as const, size: 1 }))
    const child = new FakeChild()
    const spawnFn = vi.fn(() => child as unknown as ChildProcess)
    const fetchFn = vi.fn(async () => new Response('{}', { status: 200 }))
    const service = new CreativeServeWeb(
      { checkoutDir: root, port: 17456, readyTimeoutMs: 500, component: { status: statusFn } },
      spawnFn as never,
      fetchFn,
    )

    const starting = service.start()
    child.stdout.write('[od] listening on http://127.0.0.1:17456 (headless)\n')
    await expect(starting).resolves.toEqual({ state: 'ready', origin: 'http://127.0.0.1:17456' })
    expect(statusFn).not.toHaveBeenCalled()
    await service.stop()
  })

  it('H1: falls back to the packaged dev checkout (resourcesPath/open-design) when the component is unavailable', async () => {
    // Before the fix, `component.status().state === 'unavailable'` (no
    // baked manifest — a dev/unreleased build) short-circuited straight to
    // `needs-download` and never even tried resolveOpenDesignCheckout, so a
    // dev build with a real packaged `resources/open-design` checkout (no
    // env var needed) regressed to "needs download" instead of just working.
    const resources = fs.mkdtempSync(path.join(process.cwd(), '.tmp-creative-resources-'))
    tempDirs.push(resources)
    materializeCheckout(path.join(resources, 'open-design'))
    const child = new FakeChild()
    const spawnFn = vi.fn(() => child as unknown as ChildProcess)
    const fetchFn = vi.fn(async () => new Response('{}', { status: 200 }))
    const service = new CreativeServeWeb(
      {
        resourcesPath: resources,
        port: 17456,
        readyTimeoutMs: 500,
        component: { status: () => ({ state: 'unavailable', reason: 'no manifest baked' }) },
      },
      spawnFn as never,
      fetchFn,
    )

    const starting = service.start()
    child.stdout.write('[od] listening on http://127.0.0.1:17456 (headless)\n')
    await expect(starting).resolves.toEqual({ state: 'ready', origin: 'http://127.0.0.1:17456' })
    expect(spawnFn).toHaveBeenCalledWith(
      'node',
      expect.arrayContaining(['daemon', 'start', '--serve-web']),
      expect.objectContaining({ cwd: path.join(resources, 'open-design') }),
    )
    await service.stop()
  })

  it('H1: unavailable with no dev checkout available either still fails clearly (does not become needs-download)', async () => {
    // Same `unavailable` component status as above, but with no packaged
    // dev checkout to fall back to — must reproduce the exact pre-existing
    // "Open Design runtime not found" failure, not a download prompt for a
    // component this build never baked a manifest for.
    const emptyResources = fs.mkdtempSync(path.join(process.cwd(), '.tmp-creative-empty-resources-'))
    tempDirs.push(emptyResources)
    const service = new CreativeServeWeb({
      resourcesPath: emptyResources,
      component: { status: () => ({ state: 'unavailable', reason: 'no manifest baked' }) },
    })

    await expect(service.start()).resolves.toEqual({
      state: 'failed',
      error: 'Open Design runtime not found; set A24_OPEN_DESIGN_DIR or package resources/open-design',
      notInstalled: true,
    })
  })

  it('H1: needs-download (not the dev fallback) when the component truly is not-installed/downloading/failed', async () => {
    for (const state of ['not-installed', 'downloading', 'verifying', 'failed'] as const) {
      const emptyResources = fs.mkdtempSync(path.join(process.cwd(), '.tmp-creative-empty-resources-'))
      tempDirs.push(emptyResources)
      const service = new CreativeServeWeb({
        resourcesPath: emptyResources,
        component: { status: () => ({ state, size: 42 }) },
      })
      await expect(service.start(), state).resolves.toEqual({ state: 'needs-download', size: 42 })
    }
  })

  it('H1: needs-download when the installed dir exists per the accessor but the launcher file is actually missing', async () => {
    const emptyResources = fs.mkdtempSync(path.join(process.cwd(), '.tmp-creative-empty-resources-'))
    tempDirs.push(emptyResources)
    const deletedComponentDir = fs.mkdtempSync(path.join(process.cwd(), '.tmp-creative-deleted-component-'))
    fs.rmSync(deletedComponentDir, { recursive: true, force: true }) // the accessor still claims this dir is "installed"
    const service = new CreativeServeWeb({
      resourcesPath: emptyResources,
      component: { status: () => ({ state: 'installed', dir: deletedComponentDir }) },
    })
    await expect(service.start()).resolves.toEqual({ state: 'needs-download', size: undefined })
  })

  it('fails clearly when the daemon build is missing', async () => {
    const root = fs.mkdtempSync(path.join(process.cwd(), '.tmp-creative-'))
    tempDirs.push(root)
    fs.mkdirSync(path.join(root, 'apps/daemon/bin'), { recursive: true })
    fs.writeFileSync(path.join(root, 'apps/daemon/bin/od.mjs'), '')
    const service = new CreativeServeWeb({ checkoutDir: root })

    await expect(service.start()).resolves.toEqual({
      state: 'failed',
      error: 'Open Design daemon is not built; run pnpm --filter @open-design/daemon build',
    })
  })

  it('fails clearly when the web bundle is missing', async () => {
    const root = checkout()
    fs.rmSync(path.join(root, 'apps/web/out/index.html'))
    const service = new CreativeServeWeb({ checkoutDir: root })

    await expect(service.start()).resolves.toEqual({
      state: 'failed',
      error: 'Open Design web bundle is not built; run pnpm --filter @open-design/web build',
    })
  })

  describe('Open Design under SidecarManager (C8)', () => {
    function managed(options: Partial<ConstructorParameters<typeof CreativeServeWeb>[0]> = {}, fetchFn?: typeof fetch, children: FakeChild[] = [new FakeChild()]) {
      const { resources, stateRoot, runtimeExecutable } = packagedHeadlessRuntime()
      children.forEach((child, index) => { child.pid = 4321 + index })
      let spawned = 0
      const spawnFn = vi.fn(() => children[spawned++] as unknown as ChildProcess)
      const service = new CreativeServeWeb(
        { resourcesPath: resources, stateRoot, runtimeExecutable, readyTimeoutMs: 500, ...options },
        spawnFn as never,
        fetchFn ?? vi.fn(async () => new Response('{}', { status: 200 })),
      )
      const launch = async (index: number, instance = `creative-${index}`) => {
        const starting = service.start()
        const child = children[index]
        child.stdout.write(`${JSON.stringify(headlessReady(child.pid, 17456 + index * 2, 17457 + index * 2, instance))}\n`)
        return starting
      }
      return { service, spawnFn, children, launch }
    }

    it('gives every launch a new instanceId, including after a crash that reuses the same port', async () => {
      const { service, launch, children } = managed({}, undefined, [new FakeChild(), new FakeChild()])
      const first = await launch(0)
      expect(first).toMatchObject({ state: 'ready', instanceId: expect.any(String) })
      children[0].exitNow(1)
      await vi.waitFor(() => expect(service.hasLiveProcess()).toBe(false))
      expect(service.status()).toEqual({ state: 'stopped' })
      // The crash tree was reaped by group, not just the launcher pid.
      expect(children[0].signals).toContainEqual([-4321, 'SIGKILL'])

      const second = await launch(1)
      expect(second.state).toBe('ready')
      expect(second.instanceId).not.toBe(first.instanceId)
      await service.stop()
    })

    it('publishes a crash through onStatus without waiting for a heartbeat', async () => {
      const { service, launch, children } = managed()
      await launch(0)
      const seen: string[] = []
      service.onStatus((status) => seen.push(`${status.state}:${service.headlessSidecarState()}`))
      children[0].exitNow(1)
      await vi.waitFor(() => expect(seen).toContain('stopped:failed'))
      expect(service.headlessSidecarState()).toBe('failed')
    })

    it('reports degraded after repeated health failures, never restarts, and recovers on the next success', async () => {
      let healthy = true
      const fetchFn = vi.fn(async () => (healthy ? new Response('{}', { status: 200 }) : new Response('', { status: 503 })))
      const { service, launch, spawnFn, children } = managed({ sidecarTimings: { healthIntervalMs: 20, healthTimeoutMs: 15 } }, fetchFn as never)
      await launch(0)
      expect(service.status()).not.toHaveProperty('degraded')
      healthy = false
      await vi.waitFor(() => expect(service.status()).toMatchObject({ state: 'ready', degraded: true }), { timeout: 2_000 })
      expect(service.headlessSidecarState()).toBe('degraded')
      // Still the same instance, same process: no restart on degraded.
      expect(spawnFn).toHaveBeenCalledTimes(1)
      expect(children[0].signals.filter(([, signal]) => signal !== 0)).toEqual([])
      healthy = true
      await vi.waitFor(() => expect(service.status()).not.toHaveProperty('degraded'), { timeout: 2_000 })
      expect(service.status().state).toBe('ready')
      await service.stop()
    })

    it.each([false, true])('show restarts degraded once (concurrent shows: %s)', async (concurrent) => {
      let healthy = true
      const fetchFn = vi.fn(async () => new Response('{}', { status: healthy ? 200 : 502 }))
      const { service, launch, spawnFn, children } = managed({
        sidecarTimings: { healthIntervalMs: 20, healthTimeoutMs: 15, termGraceMs: 100 },
      }, fetchFn, [new FakeChild(), new FakeChild()])
      const first = await launch(0)
      healthy = false
      await vi.waitFor(() => expect(service.headlessSidecarState()).toBe('degraded'))
      children[0].ignoreTerm = true
      const showing = service.start()
      const secondShow = concurrent ? service.start() : showing
      await vi.waitFor(() => expect(spawnFn).toHaveBeenCalledTimes(2))
      healthy = true
      children[1].stdout.write(`${JSON.stringify(headlessReady(children[1].pid, 17458, 17459, 'replacement'))}\n`)
      const [second, repeated] = await Promise.all([showing, secondShow])
      expect(second).toMatchObject({ state: 'ready', instanceId: expect.any(String) })
      expect(second.instanceId).not.toBe(first.instanceId)
      expect(repeated.instanceId).toBe(second.instanceId)
      expect(children[0].signals.filter(([, signal]) => signal !== 0)).toEqual([[4321, 'SIGTERM'], [-4321, 'SIGKILL']])
      expect(spawnFn).toHaveBeenCalledTimes(2)
      await service.stop()
    })

    it('an explicit stop during a degraded restart cancels the relaunch', async () => {
      let healthy = true
      const fetchFn = vi.fn(async () => new Response('{}', { status: healthy ? 200 : 502 }))
      const { service, launch, spawnFn } = managed({
        sidecarTimings: { healthIntervalMs: 20, healthTimeoutMs: 15, termGraceMs: 100 },
      }, fetchFn, [new FakeChild(), new FakeChild()])
      await launch(0)
      healthy = false
      await vi.waitFor(() => expect(service.headlessSidecarState()).toBe('degraded'))
      const showing = service.start()
      await service.stop()
      await showing
      expect(spawnFn).toHaveBeenCalledOnce()
    })

    it('an explicit stop also cancels a show queued behind the restart', async () => {
      let healthy = true
      const fetchFn = vi.fn(async () => new Response('{}', { status: healthy ? 200 : 502 }))
      const { service, launch, spawnFn } = managed({
        sidecarTimings: { healthIntervalMs: 20, healthTimeoutMs: 15, termGraceMs: 100 },
      }, fetchFn, [new FakeChild(), new FakeChild()])
      await launch(0)
      healthy = false
      await vi.waitFor(() => expect(service.headlessSidecarState()).toBe('degraded'))
      const first = service.start()
      const queued = service.start()
      await service.stop()
      await Promise.all([first, queued])
      expect(spawnFn).toHaveBeenCalledOnce()
    })

    it('quit during a degraded restart does not relaunch', async () => {
      let healthy = true
      const fetchFn = vi.fn(async () => new Response('{}', { status: healthy ? 200 : 502 }))
      const { service, launch, spawnFn } = managed({
        sidecarTimings: { healthIntervalMs: 20, healthTimeoutMs: 15, termGraceMs: 100 },
      }, fetchFn, [new FakeChild(), new FakeChild()])
      await launch(0)
      healthy = false
      await vi.waitFor(() => expect(service.headlessSidecarState()).toBe('degraded'))
      const showing = service.start()
      await service.shutdown()
      await showing
      expect(spawnFn).toHaveBeenCalledOnce()
      expect((await service.start()).instanceId).toBeUndefined()
      expect(spawnFn).toHaveBeenCalledOnce()
    })

    it.each(['ready', 'healthy'] as const)('show reuses a %s instance', async (state) => {
      const fetchFn = vi.fn(async () => new Response('{}', { status: state === 'healthy' ? 200 : 502 }))
        .mockResolvedValueOnce(new Response('{}', { status: 200 }))
      const { service, launch, spawnFn } = managed({ sidecarTimings: { healthIntervalMs: 1_000 } }, fetchFn)
      const first = await launch(0)
      if (state === 'healthy') await vi.waitFor(() => expect(service.headlessSidecarState()).toBe('healthy'))
      expect(service.headlessSidecarState()).toBe(state)
      expect((await service.start()).instanceId).toBe(first.instanceId)
      expect(spawnFn).toHaveBeenCalledOnce()
      await service.stop()
    })

    it('probes /api/ready through the web origin for health, with a bounded request', async () => {
      const fetchFn = vi.fn(async (_url: string | URL | Request, _init?: RequestInit) => new Response('{}', { status: 200 }))
      const { service, launch } = managed({ sidecarTimings: { healthIntervalMs: 20 } }, fetchFn as never)
      await launch(0)
      await vi.waitFor(() => expect(fetchFn.mock.calls.some(([, init]) => init?.signal instanceof AbortSignal)).toBe(true))
      expect(fetchFn.mock.calls.every(([url]) => url === 'http://127.0.0.1:17456/api/ready')).toBe(true)
      await service.stop()
    })

    it('fails with "unsupported platform" on win32 without spawning or writing the launcher config', async () => {
      const { resources, stateRoot, runtimeExecutable } = packagedHeadlessRuntime()
      const spawnFn = vi.fn()
      const service = new CreativeServeWeb(
        { resourcesPath: resources, stateRoot, runtimeExecutable },
        spawnFn as never,
        vi.fn(),
        { platform: 'win32' },
      )
      await expect(service.start()).resolves.toEqual({ state: 'failed', error: 'unsupported platform' })
      expect(spawnFn).not.toHaveBeenCalled()
      expect(fs.existsSync(path.join(stateRoot, 'agent24-headless.json'))).toBe(false)
    })

    it('a stop that arrives during launch cancels it instead of reporting failed', async () => {
      const { service, children } = managed()
      const starting = service.start()
      const stopped = service.stop()
      await expect(stopped).resolves.toEqual({ state: 'stopped' })
      await expect(starting).resolves.toEqual({ state: 'stopped' })
      expect(service.headlessSidecarState()).toBe('stopped')
      expect(children[0].signals.some(([, signal]) => signal === 'SIGTERM')).toBe(true)
    })

    it('keeps all three states stopped when stop arrives during failed-start cleanup', async () => {
      const { service, children } = managed({ sidecarTimings: { termGraceMs: 1_000 } })
      const child = children[0]
      child.ignoreTerm = true
      const starting = service.start()
      child.stdout.write(`${JSON.stringify({ ...headlessReady(child.pid, 17456, 17457), pinVersion: 'other' })}\n`)
      await vi.waitFor(() => expect(child.signals).toContainEqual([child.pid, 'SIGTERM']))
      expect(service.headlessSidecarState()).toBe('failed')
      const stopping = service.stop()
      child.groupAlive = false
      child.exitNow()
      await stopping
      await expect(starting).resolves.toEqual({ state: 'stopped' })
      expect(service.status()).toEqual({ state: 'stopped' })
      expect(service.headlessSidecarState()).toBe('stopped')
      expect(mapOpenDesignReport({ installState: 'installed', sidecar: service.headlessPhaseForReport('installed') }).state).toBe('stopped')
      expect(service.hasLiveProcess()).toBe(false)
    })

    it('a normal stop never passes through failed', async () => {
      const { service, launch } = managed()
      await launch(0)
      const phases: Array<string | null> = []
      const pairs: string[] = []
      service.onStatus((status) => { phases.push(service.headlessSidecarState()); pairs.push(`${status.state}:${service.headlessSidecarState()}`) })
      await service.stop()
      // A reporter never sees a live phase next to a stopped status.
      expect(pairs.filter((pair) => pair.startsWith('stopped:') && pair !== 'stopped:stopping' && pair !== 'stopped:stopped')).toEqual([])
      expect(phases).not.toContain('failed')
      expect(phases).toContain('stopping')
      expect(phases.at(-1)).toBe('stopped')
    })

    it('stopping a launch that failed and left nothing keeps the failed phase (no unplanned failed -> stopped)', async () => {
      const { service, children } = managed()
      const starting = service.start()
      children[0].stdout.write(`${JSON.stringify({ ...headlessReady(children[0].pid, 17456, 17457), pinVersion: 'other' })}\n`)
      await expect(starting).resolves.toMatchObject({ state: 'failed', error: expect.stringContaining('pin version') })
      expect(service.hasLiveProcess()).toBe(false)
      await service.stop()
      expect(service.headlessSidecarState()).toBe('failed')
    })

    it('hasLiveProcess is false for a clean stop and for a failure that left nothing, true while a tree remains', async () => {
      const { service, launch } = managed()
      expect(service.hasLiveProcess()).toBe(false)
      await launch(0)
      expect(service.hasLiveProcess()).toBe(true)
      await service.stop()
      expect(service.hasLiveProcess()).toBe(false)
    })
  })

  describe('stopWithin', () => {
    it('resolves true when the stop finishes inside the budget', async () => {
      await expect(stopWithin({ stop: async () => ({ state: 'stopped' }) }, 1_000)).resolves.toBe(true)
    })

    it('gives up after the budget when the stop hangs, and does not reject when it throws', async () => {
      vi.useFakeTimers()
      const hanging = stopWithin({ stop: () => new Promise(() => {}) }, 22_000)
      await vi.advanceTimersByTimeAsync(22_000)
      await expect(hanging).resolves.toBe(false)
      vi.useRealTimers()
      await expect(stopWithin({ stop: async () => { throw new Error('boom') } }, 1_000)).resolves.toBe(false)
    })
  })

  describe('shouldReloadCreativeView', () => {
    const base = { forceReload: false, currentUrl: 'http://127.0.0.1:17456/', origin: 'http://127.0.0.1:17456', loadedInstanceId: 'a', instanceId: 'a' as string | undefined }

    it('retries loading on the next show after navigation fails', async () => {
      let loadedInstanceId: string | null = 'a'
      const loadURL = vi.fn().mockRejectedValueOnce(new Error('navigation failed')).mockResolvedValue(undefined)
      const show = () => loadCreativeView({
        ...base, instanceId: 'b', loadedInstanceId, loadURL,
        isCurrent: () => true,
        rememberInstance: (id) => { loadedInstanceId = id },
      })
      await expect(show()).rejects.toThrow('navigation failed')
      expect(loadedInstanceId).toBe('a')
      await show()
      expect(loadURL).toHaveBeenCalledTimes(2)
      expect(loadedInstanceId).toBe('b')
    })

    it('does not remember a successful navigation from a superseded view request', async () => {
      const rememberInstance = vi.fn()
      await loadCreativeView({ ...base, instanceId: 'b', loadURL: async () => {}, isCurrent: () => false, rememberInstance })
      expect(rememberInstance).not.toHaveBeenCalled()
    })

    it('keeps the page for the same instance on the same origin', () => {
      expect(shouldReloadCreativeView(base)).toBe(false)
    })

    it('reloads when a new instance reuses the old port', () => {
      expect(shouldReloadCreativeView({ ...base, instanceId: 'b' })).toBe(true)
    })

    it('still reloads on force, on a different origin, and ignores instance for a checkout (no instanceId)', () => {
      expect(shouldReloadCreativeView({ ...base, forceReload: true })).toBe(true)
      expect(shouldReloadCreativeView({ ...base, currentUrl: 'about:blank' })).toBe(true)
      expect(shouldReloadCreativeView({ ...base, instanceId: undefined, loadedInstanceId: null })).toBe(false)
    })
  })
})
