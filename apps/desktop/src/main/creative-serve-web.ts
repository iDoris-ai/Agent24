import { spawn, type ChildProcess } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import type { Readable } from 'node:stream'

export type CreativeServeWebState = 'stopped' | 'starting' | 'ready' | 'failed'

export interface CreativeServeWebStatus {
  state: CreativeServeWebState
  origin?: string
  error?: string
}

export interface CreativeServeWebOptions {
  checkoutDir?: string
  nodeBinary?: string
  port?: number
  readyTimeoutMs?: number
}

type SpawnFn = typeof spawn
type FetchFn = typeof fetch
type CreativeChild = ChildProcess & { stdout: Readable; stderr: Readable }

const DEFAULT_PORT = 7456
const DEFAULT_READY_TIMEOUT_MS = 15_000

export function resolveOpenDesignCheckout(
  cwd = process.cwd(),
  env: NodeJS.ProcessEnv = process.env,
): string | null {
  const explicit = env.A24_OPEN_DESIGN_DIR?.trim()
  const candidates = [
    explicit || null,
    path.resolve(cwd, '../../../open-design-agent24'),
    path.resolve(cwd, '../open-design-agent24'),
  ].filter((candidate): candidate is string => Boolean(candidate))

  return candidates.find((candidate) =>
    fs.existsSync(path.join(candidate, 'apps/daemon/bin/od.mjs')),
  ) ?? null
}

export class CreativeServeWeb {
  private child: CreativeChild | null = null
  private current: CreativeServeWebStatus = { state: 'stopped' }
  private starting: Promise<CreativeServeWebStatus> | null = null

  constructor(
    private readonly options: CreativeServeWebOptions = {},
    private readonly spawnFn: SpawnFn = spawn,
    private readonly fetchFn: FetchFn = fetch,
  ) {}

  status(): CreativeServeWebStatus {
    return { ...this.current }
  }

  start(): Promise<CreativeServeWebStatus> {
    if (this.current.state === 'ready' && this.child) return Promise.resolve(this.status())
    if (this.starting) return this.starting

    const pending = this.startOnce()
    this.starting = pending
    pending.then(
      () => { if (this.starting === pending) this.starting = null },
      () => { if (this.starting === pending) this.starting = null },
    )
    return pending
  }

  private async startOnce(): Promise<CreativeServeWebStatus> {

    const checkoutDir = this.options.checkoutDir ?? resolveOpenDesignCheckout()
    if (!checkoutDir) {
      this.current = {
        state: 'failed',
        error: 'Open Design checkout not found; set A24_OPEN_DESIGN_DIR',
      }
      return this.status()
    }

    const entry = path.join(checkoutDir, 'apps/daemon/bin/od.mjs')
    const distEntry = path.join(checkoutDir, 'apps/daemon/dist/cli.js')
    const webEntry = path.join(checkoutDir, 'apps/web/out/index.html')
    if (!fs.existsSync(entry) || !fs.existsSync(distEntry)) {
      this.current = {
        state: 'failed',
        error: 'Open Design daemon is not built; run pnpm --filter @open-design/daemon build',
      }
      return this.status()
    }
    if (!fs.existsSync(webEntry)) {
      this.current = {
        state: 'failed',
        error: 'Open Design web bundle is not built; run pnpm --filter @open-design/web build',
      }
      return this.status()
    }

    const envPort = Number(process.env.A24_OPEN_DESIGN_PORT)
    const port = this.options.port ?? (Number.isInteger(envPort) && envPort > 0 ? envPort : DEFAULT_PORT)
    const nodeBinary = this.options.nodeBinary ?? (process.env.A24_OPEN_DESIGN_NODE?.trim() || 'node')
    const readyTimeoutMs = this.options.readyTimeoutMs ?? DEFAULT_READY_TIMEOUT_MS
    this.current = { state: 'starting' }

    const child = this.spawnFn(
      nodeBinary,
      [entry, 'daemon', 'start', '--serve-web', '--no-open', '--host', '127.0.0.1', '--port', String(port)],
      { cwd: checkoutDir, env: process.env, stdio: ['ignore', 'pipe', 'pipe'] },
    ) as CreativeChild
    this.child = child

    try {
      const origin = await this.waitForOrigin(child, readyTimeoutMs)
      await this.waitUntilReady(origin, readyTimeoutMs)
      if (this.child !== child) throw new Error('Open Design launch was superseded')
      this.current = { state: 'ready', origin }
      child.once('exit', () => {
        if (this.child !== child) return
        this.child = null
        this.current = { state: 'stopped' }
      })
      return this.status()
    } catch (error) {
      if (this.child === child) {
        child.kill('SIGTERM')
        this.child = null
      }
      this.current = {
        state: 'failed',
        error: error instanceof Error ? error.message : String(error),
      }
      return this.status()
    }
  }

  async stop(): Promise<CreativeServeWebStatus> {
    const child = this.child
    this.child = null
    if (!child) {
      this.current = { state: 'stopped' }
      return this.status()
    }

    await new Promise<void>((resolve) => {
      if (child.exitCode != null) return resolve()
      const timer = setTimeout(() => {
        child.kill('SIGKILL')
        resolve()
      }, 5_000)
      timer.unref?.()
      child.once('exit', () => {
        clearTimeout(timer)
        resolve()
      })
      child.kill('SIGTERM')
    })
    this.current = { state: 'stopped' }
    return this.status()
  }

  private waitForOrigin(child: CreativeChild, timeoutMs: number): Promise<string> {
    return new Promise((resolve, reject) => {
      let stdout = ''
      let stderr = ''
      const timeout = setTimeout(() => done(new Error('Open Design readiness output timed out')), timeoutMs)
      timeout.unref?.()

      const done = (value: Error | string): void => {
        clearTimeout(timeout)
        child.stdout.off('data', onStdout)
        child.stderr.off('data', onStderr)
        child.off('error', onError)
        child.off('exit', onExit)
        value instanceof Error ? reject(value) : resolve(value)
      }
      const inspect = (): void => {
        const match = stdout.match(/\[od\] listening on (https?:\/\/[^\s]+)(?: \([^)]*\))?\s*$/m)
        if (match?.[1]) done(match[1])
      }
      const onStdout = (chunk: Buffer | string): void => {
        stdout += chunk.toString()
        inspect()
      }
      const onStderr = (chunk: Buffer | string): void => {
        stderr = (stderr + chunk.toString()).slice(-2_000)
      }
      const onError = (error: Error): void => done(error)
      const onExit = (code: number | null): void =>
        done(new Error(`Open Design exited before ready (${code ?? 'signal'})${stderr ? `: ${stderr.trim()}` : ''}`))

      child.stdout.on('data', onStdout)
      child.stderr.on('data', onStderr)
      child.once('error', onError)
      child.once('exit', onExit)
    })
  }

  private async waitUntilReady(origin: string, timeoutMs: number): Promise<void> {
    const deadline = Date.now() + timeoutMs
    let lastError = 'not ready'
    while (Date.now() < deadline) {
      try {
        const response = await this.fetchFn(`${origin}/api/ready`)
        if (response.ok) return
        lastError = `HTTP ${response.status}`
      } catch (error) {
        lastError = error instanceof Error ? error.message : String(error)
      }
      await new Promise((resolve) => setTimeout(resolve, 100))
    }
    throw new Error(`Open Design /api/ready timed out: ${lastError}`)
  }
}
