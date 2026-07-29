// Backend process manager — supervises the daemon child process.
// AGENT24_BACKEND=node|rust selects the implementation (default: rust since
// v0.1.0 — the Rust agent24d is the shipping backend; `node` stays available
// as an explicit opt-in for the legacy mock).
// Both backends speak the same v1 protocol and announce themselves via the
// SPEC-002 §4 ready line: the manager scans child stdout for the first
// {"type":"ready","port":…,"token":…} JSON line and exposes it to the IPC
// proxy layer, so the renderer never cares which backend is running.

import { fork, spawn, type ChildProcess } from 'node:child_process'
import path from 'node:path'
import http from 'node:http'
import readline from 'node:readline'

const HEALTH_INTERVAL_MS = 5_000
const HEALTH_TIMEOUT_MS = 3_000
const MAX_HEALTH_FAILURES = 3

const isDev = process.env['NODE_ENV'] === 'development'

export type BackendKind = 'node' | 'rust'

export interface BackendEndpoint {
  port: number
  token: string
}

// Module-level so the IPC proxy can read it without holding the manager.
// Null until the CURRENT child's ready line arrives — never a guessed default:
// a stale fallback port could silently route to the wrong backend (review B5).
let currentEndpoint: BackendEndpoint | null = null

export function getBackendEndpoint(): BackendEndpoint | null {
  return currentEndpoint
}

export function selectedBackend(): BackendKind {
  // Default rust (v0.1.0); only an explicit `node` opts into the legacy mock.
  return process.env['AGENT24_BACKEND'] === 'node' ? 'node' : 'rust'
}

/** Daemon lifecycle status surfaced to the tray (F1b). */
export type BackendStatus = 'running' | 'starting' | 'stopped'

/** Pure derivation of the tray status from the manager's observable state.
 * Exported so the state machine is unit-testable without spawning a process:
 * - a user-initiated stop pins `stopped` (the health loop won't auto-respawn);
 * - a healthy endpoint is `running`;
 * - anything else (spawned but not yet ready, or briefly between respawns) is
 *   `starting`. */
export function deriveStatus(s: {
  userStopped: boolean
  hasEndpoint: boolean
  healthy: boolean
}): BackendStatus {
  if (s.userStopped) return 'stopped'
  if (s.hasEndpoint && s.healthy) return 'running'
  return 'starting'
}

function repoRootDev(): string {
  // __dirname = apps/desktop/dist/main → repo root is four levels up
  return path.join(__dirname, '..', '..', '..', '..')
}

function resolveNodeEntry(): string {
  if (isDev) {
    // Workspace dep: @agent24/node-daemon main → packages/node-daemon/dist/server.js
    return require.resolve('@agent24/node-daemon')
  }
  return path.join(process.resourcesPath, 'backend', 'server.js')
}

function resolveRustBinary(): string {
  const override = process.env['AGENT24D_BIN']
  if (override) return override
  const exe = process.platform === 'win32' ? 'agent24d.exe' : 'agent24d'
  if (isDev) {
    return path.join(repoRootDev(), 'rust', 'target', 'debug', exe)
  }
  // Packaged: electron-builder copies the platform-native release binary into
  // Resources/backend/<exe> (per-platform extraResources in package.json).
  return path.join(process.resourcesPath, 'backend', exe)
}

function checkHealth(endpoint: BackendEndpoint | null): Promise<boolean> {
  if (!endpoint) return Promise.resolve(false)
  return new Promise((resolve) => {
    const headers: Record<string, string> = {}
    if (endpoint.token) headers['Authorization'] = `Bearer ${endpoint.token}`
    const req = http.get(
      {
        host: '127.0.0.1',
        port: endpoint.port,
        path: '/api/v1/health',
        headers,
        timeout: HEALTH_TIMEOUT_MS,
      },
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
  private readonly kind: BackendKind = selectedBackend()
  // F1b: last observed health (drives the tray status), and whether the user
  // deliberately stopped the daemon — while true, the health loop must NOT
  // auto-respawn it (that would defeat a manual stop).
  private lastHealthy = false
  private userStopped = false

  start(): void {
    this.spawnChild()
    this.healthTimer = setInterval(() => { void this.tick() }, HEALTH_INTERVAL_MS)
  }

  /** Current lifecycle status for the tray (F1b). */
  status(): BackendStatus {
    return deriveStatus({
      userStopped: this.userStopped,
      hasEndpoint: currentEndpoint !== null,
      healthy: this.lastHealthy,
    })
  }

  /** Which backend implementation is running (shown in the tray tooltip). */
  backendKind(): BackendKind {
    return this.kind
  }

  /** Tray "启动 daemon": clear a prior manual stop and (re)spawn if needed. */
  startDaemon(): void {
    this.userStopped = false
    this.failureCount = 0
    if (!this.child) this.spawnChild()
  }

  /** Tray "停止 daemon": kill the child and keep it down (no auto-respawn). */
  stopDaemon(): void {
    this.userStopped = true
    this.lastHealthy = false
    this.killChild()
  }

  /** Tray "重启 daemon": kill + respawn regardless of prior state. */
  restart(): void {
    this.userStopped = false
    this.failureCount = 0
    this.lastHealthy = false
    this.killChild()
    this.spawnChild()
  }

  private spawnChild(): void {
    currentEndpoint = null

    if (this.kind === 'rust') {
      const bin = resolveRustBinary()
      this.child = spawn(bin, ['serve', '--port', '0'], {
        stdio: ['ignore', 'pipe', 'pipe'],
        env: { ...process.env },
      })
    } else {
      const entry = resolveNodeEntry()
      this.child = fork(entry, [], {
        // piped (not inherited) so the ready line can be scanned
        silent: true,
        env: { ...process.env, NODE_ENV: process.env['NODE_ENV'] ?? 'production' },
      })
    }

    this.wireChild(this.child)
    console.log(`[backend:${this.kind}] spawned pid`, this.child.pid)
  }

  private wireChild(child: ChildProcess): void {
    // Every callback guards on identity: a stale child's late ready line or
    // exit event must never clobber the current child's state (review B5)
    const rls: readline.Interface[] = []
    if (child.stdout) {
      const rl = readline.createInterface({ input: child.stdout })
      rls.push(rl)
      rl.on('line', (line) => {
        // SPEC-002 §4: scan for the first type=="ready" JSON line — it is not
        // necessarily the first stdout line (init logs may precede it)
        try {
          const parsed = JSON.parse(line) as { type?: string; port?: number; token?: string }
          if (parsed.type === 'ready' && typeof parsed.port === 'number') {
            if (this.child === child) {
              currentEndpoint = { port: parsed.port, token: parsed.token ?? '' }
              console.log(`[backend:${this.kind}] ready on port ${parsed.port}`)
            }
            return
          }
        } catch { /* not JSON — plain log line */ }
        console.log(`[backend:${this.kind}]`, line)
      })
    }
    if (child.stderr) {
      const rl = readline.createInterface({ input: child.stderr })
      rls.push(rl)
      rl.on('line', (line) => console.error(`[backend:${this.kind}]`, line))
    }
    child.on('close', () => {
      for (const rl of rls) rl.close()
    })
    child.on('exit', (code) => {
      console.warn(`[backend:${this.kind}] exited with code ${String(code)}`)
      if (this.child === child) {
        this.child = null
        currentEndpoint = null
      }
    })
    child.on('error', (err) => {
      // spawn failures (e.g. ENOENT for a missing agent24d binary) emit
      // 'error' without a guaranteed 'exit' — clear state so the health loop
      // performs a plain (bounded-interval) respawn rather than leaking a ref
      console.error(`[backend:${this.kind}] process error`, err)
      if (this.child === child) {
        this.child = null
        currentEndpoint = null
      }
    })
  }

  private async tick(): Promise<void> {
    // A deliberate stop suspends supervision entirely — don't probe, don't
    // count failures, and above all don't auto-respawn (F1b).
    if (this.userStopped) {
      this.lastHealthy = false
      return
    }

    const alive = await checkHealth(currentEndpoint)
    this.lastHealthy = alive
    if (alive) {
      this.failureCount = 0
      return
    }

    this.failureCount += 1
    console.warn(`[backend:${this.kind}] health check failed (${this.failureCount}/${MAX_HEALTH_FAILURES})`)

    if (this.failureCount >= MAX_HEALTH_FAILURES) {
      console.warn(`[backend:${this.kind}] restarting after consecutive failures`)
      this.killChild()
      this.failureCount = 0
      this.spawnChild()
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
    // Clear immediately: with child already nulled, the exit handler's
    // identity guard will (correctly) skip its own cleanup — without this,
    // stop() would leave a stale dead endpoint exposed forever (review B5)
    currentEndpoint = null
  }

  stop(): void {
    if (this.healthTimer) {
      clearInterval(this.healthTimer)
      this.healthTimer = null
    }
    this.killChild()
  }
}
