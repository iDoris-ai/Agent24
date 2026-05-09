import { fork, type ChildProcess } from 'node:child_process'
import path from 'node:path'
import http from 'node:http'

const BACKEND_PORT = 8765
const HEALTH_INTERVAL_MS = 5_000
const HEALTH_TIMEOUT_MS = 3_000
const MAX_HEALTH_FAILURES = 3

const isDev = process.env['NODE_ENV'] === 'development'

function resolveServerEntry(): string {
  if (isDev) {
    // rootDir=src, outDir=dist → src/backend/server.ts → dist/backend/server.js
    // __dirname here is dist/main/, so go up one level to dist/
    return path.join(__dirname, '..', 'backend', 'server.js')
  }
  // In production: packed into resources/backend/server.js via electron-builder extraResources.
  return path.join(process.resourcesPath, 'backend', 'server.js')
}

function checkHealth(): Promise<boolean> {
  return new Promise((resolve) => {
    const req = http.get(
      { host: '127.0.0.1', port: BACKEND_PORT, path: '/health', timeout: HEALTH_TIMEOUT_MS },
      (res) => {
        resolve(res.statusCode === 200)
        res.resume()
      },
    )
    req.on('error', () => resolve(false))
    req.on('timeout', () => { req.destroy(); resolve(false) })
  })
}

export class BackendManager {
  private child: ChildProcess | null = null
  private healthTimer: NodeJS.Timeout | null = null
  private failureCount = 0

  start(): void {
    this.spawn()
    this.healthTimer = setInterval(() => { void this.tick() }, HEALTH_INTERVAL_MS)
  }

  private spawn(): void {
    const entry = resolveServerEntry()
    this.child = fork(entry, [], {
      stdio: 'inherit',
      env: { ...process.env, NODE_ENV: process.env['NODE_ENV'] ?? 'production' },
    })

    this.child.on('exit', (code) => {
      console.warn(`[backend] exited with code ${String(code)}`)
      this.child = null
    })

    this.child.on('error', (err) => {
      console.error('[backend] process error', err)
    })

    console.log('[backend] spawned pid', this.child.pid)
  }

  private async tick(): Promise<void> {
    const alive = await checkHealth()
    if (alive) {
      this.failureCount = 0
      return
    }

    this.failureCount += 1
    console.warn(`[backend] health check failed (${this.failureCount}/${MAX_HEALTH_FAILURES})`)

    if (this.failureCount >= MAX_HEALTH_FAILURES) {
      console.warn('[backend] restarting after consecutive failures')
      this.killChild()
      this.failureCount = 0
      this.spawn()
    }
  }

  private killChild(): void {
    if (!this.child) return
    try {
      this.child.kill('SIGTERM')
    } catch {
      // process may already be gone
    }
    this.child = null
  }

  stop(): void {
    if (this.healthTimer) {
      clearInterval(this.healthTimer)
      this.healthTimer = null
    }
    this.killChild()
  }
}
