import { EventEmitter } from 'node:events'
import { PassThrough } from 'node:stream'
import type { ChildProcess } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { CreativeServeWeb, resolveOpenDesignCheckout } from './creative-serve-web'

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

function checkout(): string {
  const root = fs.mkdtempSync(path.join(process.cwd(), '.tmp-creative-'))
  tempDirs.push(root)
  fs.mkdirSync(path.join(root, 'apps/daemon/bin'), { recursive: true })
  fs.mkdirSync(path.join(root, 'apps/daemon/dist'), { recursive: true })
  fs.writeFileSync(path.join(root, 'apps/daemon/bin/od.mjs'), '')
  fs.writeFileSync(path.join(root, 'apps/daemon/dist/cli.js'), '')
  return root
}

afterEach(() => {
  for (const dir of tempDirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true })
})

describe('CreativeServeWeb', () => {
  it('uses the explicit Open Design checkout when it contains the od entry', () => {
    const root = checkout()
    expect(resolveOpenDesignCheckout('/unused', { A24_OPEN_DESIGN_DIR: root })).toBe(root)
  })

  it('launches serve-web, waits for /api/ready, and stops with SIGTERM', async () => {
    const root = checkout()
    const child = new FakeChild()
    const spawnFn = vi.fn(() => child as unknown as ChildProcess)
    const fetchFn = vi.fn(async () => new Response(JSON.stringify({ ok: true }), { status: 200 }))
    const service = new CreativeServeWeb(
      { checkoutDir: root, port: 17456, readyTimeoutMs: 500 },
      spawnFn as never,
      fetchFn,
    )

    const starting = service.start()
    child.stdout.write('[od] listening on http://127.0.0.1:17456 (headless)\n')

    await expect(starting).resolves.toEqual({ state: 'ready', origin: 'http://127.0.0.1:17456' })
    expect(spawnFn).toHaveBeenCalledWith(
      'node',
      expect.arrayContaining(['daemon', 'start', '--serve-web', '--no-open', '--port', '17456']),
      expect.objectContaining({ cwd: root }),
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
})
