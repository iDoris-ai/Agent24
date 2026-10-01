import { EventEmitter } from 'node:events'
import { PassThrough } from 'node:stream'
import type { ChildProcess } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  CreativeServeWeb,
  CreativeViewRequestFence,
  canonicalCreativeOrigin,
  classifyCreativeUrl,
  creativeChildEnvironment,
  resolveOpenDesignCheckout,
} from './creative-serve-web'

class FakeChild extends EventEmitter {
  stdout = new PassThrough()
  stderr = new PassThrough()
  stdin = new PassThrough()
  exitCode: number | null = null
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

  it('removes Agent24 host variables from the Creative child environment', () => {
    expect(creativeChildEnvironment({
      PATH: '/usr/bin',
      OPENAI_API_KEY: 'provider-secret',
      A24_HANDSHAKE_TOKEN: 'host-secret',
      a24_host_bootstrap: 'also-secret',
    })).toEqual({
      PATH: '/usr/bin',
      OPENAI_API_KEY: 'provider-secret',
    })
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
