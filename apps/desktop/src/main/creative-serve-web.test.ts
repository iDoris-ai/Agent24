import { EventEmitter } from 'node:events'
import { PassThrough } from 'node:stream'
import type { ChildProcess } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  CreativeServeWeb,
  CreativeViewRequestFence,
  OPEN_DESIGN_PIN_VERSION,
  canonicalCreativeOrigin,
  classifyCreativeUrl,
  creativeChildEnvironment,
  parseAgent24HeadlessReady,
  resolveAgent24HeadlessLauncher,
  resolveOpenDesignCheckout,
} from './creative-serve-web'

class FakeChild extends EventEmitter {
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

function packagedHeadlessRuntime(): { resources: string; stateRoot: string; runtimeExecutable: string } {
  const root = fs.mkdtempSync(path.join(process.cwd(), '.tmp-creative-headless-'))
  tempDirs.push(root)
  const resources = path.join(root, 'resources')
  const stateRoot = path.join(root, 'state')
  const runtimeExecutable = path.join(root, 'Agent24')
  fs.mkdirSync(path.join(resources, 'app', 'prebundled'), { recursive: true })
  fs.mkdirSync(path.join(resources, 'open-design'), { recursive: true })
  fs.writeFileSync(path.join(resources, 'app', 'prebundled', 'agent24-headless.cjs'), '')
  fs.writeFileSync(runtimeExecutable, '')
  return { resources, stateRoot, runtimeExecutable }
}

function headlessReady(ownerPid: number, webPort: number, daemonPort: number, instanceId = 'creative-1') {
  return {
    type: 'ready', protocol: 1, instanceId, pinVersion: OPEN_DESIGN_PIN_VERSION,
    webOrigin: `http://127.0.0.1:${webPort}`, daemonOrigin: `http://127.0.0.1:${daemonPort}`,
    ownership: { ownerPid, kind: 'process-tree' },
  }
}

afterEach(() => {
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

    await expect(starting).resolves.toEqual({ state: 'ready', origin: 'http://127.0.0.1:17456' })
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
      }),
    )
    expect(JSON.parse(fs.readFileSync(configPath, 'utf8'))).toEqual({
      protocol: 1,
      pinVersion: OPEN_DESIGN_PIN_VERSION,
      resourceRoot: path.join(resources, 'open-design'),
      dataRoot: path.join(stateRoot, 'data'),
      runtimeRoot: path.join(stateRoot, 'runtime'),
      runtimeExecutable,
    })
    expect(fetchFn).toHaveBeenCalledWith('http://127.0.0.1:17456/api/ready')
    await service.stop()
    expect(child.kill).toHaveBeenCalledWith('SIGTERM')
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
    expect(child.kill).toHaveBeenCalledWith('SIGTERM')
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
    const restarted = service.start()
    second.stdout.write(`${JSON.stringify(headlessReady(second.pid, 17458, 17459, 'creative-2'))}\n`)
    await expect(restarted).resolves.toEqual({ state: 'ready', origin: 'http://127.0.0.1:17458' })
    expect(spawnFn).toHaveBeenCalledTimes(2)
    await service.stop()
  })

  it('serializes starts behind a slow packaged stop and never force-kills only the owner', async () => {
    const { resources, stateRoot, runtimeExecutable } = packagedHeadlessRuntime()
    const child = new FakeChild()
    child.kill = vi.fn(() => true)
    const spawnFn = vi.fn(() => child as unknown as ChildProcess)
    const service = new CreativeServeWeb(
      { resourcesPath: resources, stateRoot, runtimeExecutable, readyTimeoutMs: 500 },
      spawnFn as never,
      vi.fn(async () => new Response('{}', { status: 200 })),
    )
    const starting = service.start()
    child.stdout.write(`${JSON.stringify(headlessReady(child.pid, 17460, 17461, 'creative-slow'))}\n`)
    await expect(starting).resolves.toEqual({ state: 'ready', origin: 'http://127.0.0.1:17460' })

    vi.useFakeTimers()
    try {
      const stopping = service.stop()
      const concurrentStart = service.start()
      expect(spawnFn).toHaveBeenCalledTimes(1)
      await vi.advanceTimersByTimeAsync(60_000)
      const failed = {
        state: 'failed',
        error: 'Open Design headless launcher did not stop within 60000ms; refusing an owner-only force kill',
      }
      await expect(stopping).resolves.toEqual(failed)
      await expect(concurrentStart).resolves.toEqual(failed)
      expect(spawnFn).toHaveBeenCalledTimes(1)
      expect(child.kill).toHaveBeenCalledWith('SIGTERM')
      expect(child.kill).not.toHaveBeenCalledWith('SIGKILL')

      child.exitCode = 0
      child.emit('exit', 0, null)
      expect(service.status()).toEqual({ state: 'stopped' })
    } finally {
      vi.useRealTimers()
    }
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
})
