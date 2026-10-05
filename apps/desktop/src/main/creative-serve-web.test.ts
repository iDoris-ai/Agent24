import { EventEmitter } from 'node:events'
import { PassThrough } from 'node:stream'
import type { ChildProcess } from 'node:child_process'
import crypto from 'node:crypto'
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
      protocol: 2,
      pinVersion: OPEN_DESIGN_PIN_VERSION,
      resourceRoot: path.join(resources, 'open-design'),
      dataRoot: path.join(stateRoot, 'data'),
      runtimeRoot: path.join(stateRoot, 'runtime'),
      runtimeExecutable,
      // Legacy bundled-resources path: no resourceSafeBase (the fork's v1
      // runtimeExecutable-derived default still applies).
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
    await expect(starting).resolves.toEqual({ state: 'ready', origin: 'http://127.0.0.1:17456' })
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
    await expect(starting).resolves.toEqual({ state: 'ready', origin: 'http://127.0.0.1:17456' })
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
    await expect(starting).resolves.toEqual({ state: 'ready', origin: 'http://127.0.0.1:17456' })
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
})
