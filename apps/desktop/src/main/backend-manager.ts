// Backend process manager — supervises the daemon child process.
// AGENT24_BACKEND=node|rust selects the implementation (default: rust since
// v0.1.0 — the Rust agent24d is the shipping backend; `node` stays available
// as an explicit opt-in for the legacy mock).
// Both backends speak the same v1 protocol and announce themselves via the
// SPEC-002 §4 ready line: the manager scans child stdout for the first
// {"type":"ready","port":…,"token":…} JSON line and exposes it to the IPC
// proxy layer, so the renderer never cares which backend is running.

import { fork, spawn, type ChildProcess } from 'node:child_process'
import fs from 'node:fs'
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

// ── DEP-A5: reuse an already-running `agent24 daemon start` instead of always
// spawning a second one (that second one would fail: non-ephemeral agent24d
// holds a lifetime singleton lock — see `server.rs`'s `try_acquire_singleton`
// — so the CLI-then-desktop ordering used to leave the desktop app stuck in a
// spawn/health-fail/respawn loop, and desktop-then-CLI made `daemon start`
// fail outright). ────────────────────────────────────────────────────────

/** What `readExternalDaemonState` recovers from the daemon's discovery file. */
export interface ExternalDaemonState {
  port: number
  token: string
  pid: number
}

/** Mirrors `agent24_protocol::state_file::state_path()` (Rust):
 * `$HOME/.agent24/daemon.json`. Reads `HOME` directly (not `os.homedir()`) so
 * this stays in lockstep with where the daemon itself looks, and so tests can
 * point it at an isolated HOME without touching the real one. */
function daemonStatePath(): string | null {
  const home = process.env['HOME']
  if (!home) return null
  return path.join(home, '.agent24', 'daemon.json')
}

/** Read + parse `daemon.json`. Absent, unreadable, or malformed all mean the
 * same thing here — "nothing to reuse" — never a hard error; the caller falls
 * back to spawning its own daemon exactly as before this file's dep on it. */
export function readExternalDaemonState(): ExternalDaemonState | null {
  const statePath = daemonStatePath()
  if (!statePath) return null
  try {
    const raw = fs.readFileSync(statePath, 'utf8')
    const parsed = JSON.parse(raw) as { port?: unknown; token?: unknown; pid?: unknown }
    if (typeof parsed.port !== 'number' || typeof parsed.pid !== 'number') return null
    const token = typeof parsed.token === 'string' ? parsed.token : ''
    return { port: parsed.port, token, pid: parsed.pid }
  } catch {
    return null
  }
}

/** Discovery + health-probing, factored out so tests can inject both without
 * touching the real filesystem or making a real HTTP call. */
export interface ExternalDaemonSource {
  read(): ExternalDaemonState | null
  checkHealth(endpoint: BackendEndpoint | null): Promise<boolean>
}

/** Constructor-injectable seams. Both default to the real implementations —
 * only tests pass overrides. */
export interface BackendManagerDeps {
  external?: ExternalDaemonSource
  /** Spawns/forks the backend child process. Defaulted to the real
   * spawn/fork call below; tests inject a fake so `spawnChild()` never
   * touches a real binary. */
  spawnProcess?: (kind: BackendKind) => ChildProcess
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

function defaultSpawnProcess(kind: BackendKind): ChildProcess {
  if (kind === 'rust') {
    const bin = resolveRustBinary()
    return spawn(bin, ['serve', '--port', '0'], {
      stdio: ['ignore', 'pipe', 'pipe'],
      env: { ...process.env },
    })
  }
  const entry = resolveNodeEntry()
  return fork(entry, [], {
    // piped (not inherited) so the ready line can be scanned
    silent: true,
    env: { ...process.env, NODE_ENV: process.env['NODE_ENV'] ?? 'production' },
  })
}

const defaultExternalDaemonSource: ExternalDaemonSource = {
  read: readExternalDaemonState,
  checkHealth,
}

export class BackendManager {
  private child: ChildProcess | null = null
  private healthTimer: NodeJS.Timeout | null = null
  private failureCount = 0
  private readonly kind: BackendKind = selectedBackend()
  private readonly external: ExternalDaemonSource
  private readonly spawnProcess: (kind: BackendKind) => ChildProcess
  // F1b: last observed health (drives the tray status), and whether the user
  // deliberately stopped the daemon — while true, the health loop must NOT
  // auto-respawn it (that would defeat a manual stop).
  private lastHealthy = false
  private userStopped = false
  // DEP-A5: true while `currentEndpoint` points at a daemon this manager
  // discovered (via daemon.json) rather than spawned itself. Gates every
  // lifecycle action that would otherwise kill a process we don't own.
  private isExternal = false
  private externalPid: number | null = null

  constructor(deps: BackendManagerDeps = {}) {
    this.external = deps.external ?? defaultExternalDaemonSource
    this.spawnProcess = deps.spawnProcess ?? defaultSpawnProcess
  }

  async start(): Promise<void> {
    const reused = await this.tryReuseExternal()
    if (!reused) this.spawnChild()
    this.healthTimer = setInterval(() => { void this.tick() }, HEALTH_INTERVAL_MS)
  }

  /** DEP-A5 step 1: before spawning anything, check whether a daemon started
   * outside this app (typically `agent24 daemon start`) is already up and
   * healthy — if so, point at it instead of racing it for the singleton lock. */
  private async tryReuseExternal(): Promise<boolean> {
    const state = this.external.read()
    if (!state) return false
    const endpoint: BackendEndpoint = { port: state.port, token: state.token }
    const healthy = await this.external.checkHealth(endpoint)
    if (!healthy) return false
    this.isExternal = true
    this.externalPid = state.pid
    currentEndpoint = endpoint
    this.lastHealthy = true
    console.log(
      `[backend:${this.kind}] reusing external daemon (pid ${state.pid}, port ${state.port}) — not spawning our own`,
    )
    return true
  }

  /** True while the current endpoint is a daemon this manager discovered
   * rather than spawned (surfaced by the tray so the UI can say so). */
  isExternalDaemon(): boolean {
    return this.isExternal
  }

  /** pid of the reused external daemon, for tray labels; null when not external. */
  externalDaemonPid(): number | null {
    return this.externalPid
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
    if (!this.child && !this.isExternal) this.spawnChild()
  }

  /** Tray "停止 daemon": for a daemon this manager spawned, kill the child and
   * keep it down (no auto-respawn) — unchanged from before DEP-A5.
   * For a reused *external* daemon (DEP-A5 requirement #2), "stop" must never
   * kill it — it belongs to `agent24 daemon start` (or another owner), not to
   * this app. Instead this only forgets the connection: `userStopped` still
   * pins the tray to "stopped" and suspends the health loop, exactly as a
   * real stop would, but the external process itself is left untouched. */
  stopDaemon(): void {
    this.userStopped = true
    this.lastHealthy = false
    if (this.isExternal) {
      this.disconnectExternal()
      return
    }
    this.killChild()
  }

  /** Tray "重启 daemon": for a spawned child, kill + respawn as before.
   * For a reused external daemon, killing it is off the table for the same
   * reason as stopDaemon() above — "restart" here means "stop reusing it and
   * bring up a desktop-managed one instead". */
  restart(): void {
    this.userStopped = false
    this.failureCount = 0
    this.lastHealthy = false
    if (this.isExternal) {
      this.disconnectExternal()
    } else {
      this.killChild()
    }
    this.spawnChild()
  }

  /** Forget a reused external daemon without touching the process itself. */
  private disconnectExternal(): void {
    this.isExternal = false
    this.externalPid = null
    currentEndpoint = null
  }

  private spawnChild(): void {
    currentEndpoint = null
    // Any spawn (initial, respawn-after-failure, or an explicit restart)
    // always produces a desktop-owned child — never external past this point.
    this.isExternal = false
    this.externalPid = null

    this.child = this.spawnProcess(this.kind)
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

    const alive = await this.external.checkHealth(currentEndpoint)
    this.lastHealthy = alive
    if (alive) {
      this.failureCount = 0
      return
    }

    this.failureCount += 1
    console.warn(`[backend:${this.kind}] health check failed (${this.failureCount}/${MAX_HEALTH_FAILURES})`)

    if (this.failureCount >= MAX_HEALTH_FAILURES) {
      // DEP-A5 decision — what happens when a *reused external* daemon dies:
      // chose "re-detect, then fall back to spawning our own" over "prompt the
      // user". Rationale: this branch already exists (it's the same
      // failure-threshold respawn path used for a daemon we spawned
      // ourselves), so reusing it for the external case is the smallest
      // change — no new state machine, no new UI surface for a blocking
      // prompt from a background timer tick. The cost is a silent handoff:
      // the user isn't told their external daemon died. That's judged
      // acceptable because the tray's status/tooltip (main.ts's
      // refreshTray()) already flips from "运行中（外部）" to a plain
      // spawned-daemon state within one health-poll interval, so the demotion
      // is visible on next glance, just not interrupt-driven.
      if (this.isExternal) {
        console.warn(
          `[backend:${this.kind}] external daemon (pid ${this.externalPid ?? '?'}) stopped responding — ` +
          'falling back to a self-managed daemon',
        )
      }
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
    // App exit must never kill a daemon this manager didn't spawn (DEP-A5
    // requirement #2) — just stop watching it and let it keep running.
    if (this.isExternal) {
      this.disconnectExternal()
      return
    }
    this.killChild()
  }
}
