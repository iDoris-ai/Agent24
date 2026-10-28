// Backend process manager — supervises the daemon child process.
// AGENT24_BACKEND=node|rust selects the implementation (default: rust since
// v0.1.0 — the Rust agent24d is the shipping backend; `node` is a dev-only
// opt-in for the reference mock, ignored in packaged builds per ADR-037).
// Both backends speak the same v1 protocol and announce themselves via the
// SPEC-002 §4 ready line: the manager scans child stdout for the first
// {"type":"ready","port":…} JSON line and exposes the port to the IPC proxy
// layer, so the renderer never cares which backend is running. The credential
// on that line (`token`, or `product_host_token` in capability mode) stays in
// this module.

import { fork, spawn, type ChildProcess } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import http from 'node:http'
import readline from 'node:readline'
import {
  CLIENT_PROTOCOL_VERSION,
  protocolCompatibility,
  protocolVersionOf,
} from './protocol-compatibility'
import { joinLiteral, tryDataDir } from './data-dir'

const HEALTH_INTERVAL_MS = 5_000
const HEALTH_TIMEOUT_MS = 3_000
const MAX_HEALTH_FAILURES = 3


export type BackendKind = 'node' | 'rust'

export interface BackendEndpoint {
  port: number
}

/** How main authenticates to the daemon. Held apart from `BackendEndpoint` so
 * the credential never travels with the port toward the renderer. `none` is
 * only the node mock, which takes no credential. */
type DaemonAuth = { kind: 'bearer'; token: string } | { kind: 'none' }

/** The daemon's port and the headers that authenticate main to it. */
export interface DaemonConnection {
  port: number
  authHeaders: Record<string, string>
}

// Module-level so the IPC proxy can read it without holding the manager.
// Null until the CURRENT child's ready line arrives — never a guessed default:
// a stale fallback port could silently route to the wrong backend (review B5).
let current: { port: number; auth: DaemonAuth } | null = null

export function getBackendEndpoint(): BackendEndpoint | null {
  return current ? { port: current.port } : null
}

/** Null until the daemon is ready, and also when the rust daemon left no
 * usable credential: callers then refuse to send instead of sending without
 * one. */
export function daemonConnection(): DaemonConnection | null {
  if (!current) return null
  const { port, auth } = current
  if (auth.kind === 'none') return { port, authHeaders: {} }
  if (!auth.token) return null
  return { port, authHeaders: { Authorization: `Bearer ${auth.token}` } }
}

/** The daemon's port and main's credential, for the `agent24` CLI that main
 * runs and hands them to on stdin (documents-cli.ts). Null in the same cases
 * as `daemonConnection`; the node mock takes no credential, so it is empty. */
export interface CliEndpoint {
  port: number
  token: string
}

export function daemonCliEndpoint(): CliEndpoint | null {
  if (!current) return null
  const { port, auth } = current
  if (auth.kind === 'none') return { port, token: '' }
  return auth.token ? { port, token: auth.token } : null
}

/** How the desktop asks agent24d to authenticate. Capability mode needs the
 * daemon to verify that stdin/stdout are private pipes, which it can do only
 * on Unix; Windows and the node mock keep the single legacy token. */
export type DaemonAuthMode = 'legacy' | 'capabilities'

export function selectedAuthMode(kind: BackendKind, platform: NodeJS.Platform = process.platform): DaemonAuthMode {
  return kind === 'rust' && platform !== 'win32' ? 'capabilities' : 'legacy'
}

/** Dev-only, explicit opt-in (`AGENT24_BACKEND=node`): the contract-test
 * reference daemon at packages/node-daemon/dist/server.js. REV-01: desktop no
 * longer depends on or builds @agent24/node-daemon, so this resolves the
 * workspace path directly and the developer builds it by hand
 * (`pnpm --filter @agent24/node-daemon build`). Throws if absent, which
 * selectedBackend turns into a fall-back to rust. */
export function resolveNodeEntryFrom(dir: string, exists: (p: string) => boolean = fs.existsSync): string {
  // <repo>/apps/desktop/{dist,src}/main → repo root
  const entry = path.resolve(dir, '../../../../packages/node-daemon/dist/server.js')
  if (!exists(entry)) {
    throw new Error(`${entry} not found — run: pnpm --filter @agent24/node-daemon build`)
  }
  return entry
}

function resolveNodeEntry(): string {
  return resolveNodeEntryFrom(__dirname)
}

/** `packaged` MUST come from Electron's `app.isPackaged` (injected by main.ts).
 * It is deliberately NOT derived from NODE_ENV: an installed app can inherit
 * NODE_ENV=development from its launching shell, and that must not unlock the
 * node backend. Default `true` = fail closed (never node unless told dev).
 *
 * ADR-037 decision 1 (CORE-8): node-daemon is not shipped in installers, so:
 * - packaged + AGENT24_BACKEND=node → warn, use rust;
 * - dev + node but the dependency cannot be resolved → warn, use rust (never
 *   throw: a stray env var must not brick the app). */
export function selectedBackend(
  packaged: boolean = true,
  resolveNode: () => string = resolveNodeEntry,
): BackendKind {
  if (process.env['AGENT24_BACKEND'] !== 'node') return 'rust'
  if (packaged) {
    console.warn(
      '[backend] AGENT24_BACKEND=node ignored in packaged builds (ADR-037: node-daemon is a dev/contract-test reference only); using rust',
    )
    return 'rust'
  }
  try {
    resolveNode()
  } catch (err) {
    console.warn(
      `[backend] AGENT24_BACKEND=node requested but packages/node-daemon/dist/server.js is not resolvable (${String(err)}); using rust`,
    )
    return 'rust'
  }
  return 'node'
}

/** Daemon lifecycle status surfaced to the tray (F1b). */
export type BackendStatus = 'running' | 'starting' | 'stopped' | 'error' | 'blocked'

/** Pure derivation of the tray status from the manager's observable state.
 * Exported so the state machine is unit-testable without spawning a process:
 * - an incompatible external daemon pins `error` until a later probe finds it
 *   gone or on this protocol;
 * - a user-initiated stop pins `stopped` (the health loop won't auto-respawn);
 * - a capability-mode daemon of another host serving this data dir is
 *   `blocked`: there is nothing to connect to and nothing this app may start;
 * - a healthy endpoint is `running`;
 * - anything else (spawned but not yet ready, or briefly between respawns) is
 *   `starting`. */
export function deriveStatus(s: {
  userStopped: boolean
  hasEndpoint: boolean
  healthy: boolean
  incompatible?: boolean
  foreignHost?: boolean
}): BackendStatus {
  if (s.incompatible) return 'error'
  if (s.userStopped) return 'stopped'
  if (s.foreignHost) return 'blocked'
  if (s.hasEndpoint && s.healthy) return 'running'
  return 'starting'
}

function repoRootDev(): string {
  // __dirname = apps/desktop/dist/main → repo root is four levels up
  return path.join(__dirname, '..', '..', '..', '..')
}

/** `packaged` = Electron `app.isPackaged` (injected), NOT NODE_ENV: an installed
 * app may inherit NODE_ENV=development and must still use its bundled binary. */
export function resolveRustBinary(packaged: boolean): string {
  return process.env['AGENT24D_BIN'] || resolveBundledRustBinary('agent24d', packaged)
}

/** Set once by main.ts from `app.isPackaged` for callers that have no
 * BackendManager at hand (documents-cli.ts). Default `true` = fail closed:
 * use the bundled binary, never the dev build tree. */
let bundledPackaged = true
export function setBundledPackaged(packaged: boolean): void {
  bundledPackaged = packaged
}

/** A Rust binary the app ships: `agent24d`, or the `agent24` CLI whose
 * commands main runs (documents-cli.ts). `packaged` as for `resolveRustBinary`;
 * omitted → the value main.ts registered via `setBundledPackaged`. */
export function resolveBundledRustBinary(name: string, packaged: boolean = bundledPackaged): string {
  const exe = process.platform === 'win32' ? `${name}.exe` : name
  if (!packaged) {
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
  authMode: DaemonAuthMode
  /** Absent when daemon.json was written before version negotiation. */
  protocolVersion?: number
}

/** One `GET /api/v1/health`. `protocolVersion: null` means the field is absent. */
export interface DaemonHealthProbe {
  ok: boolean
  protocolVersion: number | null
}

/** Mirrors `agent24_protocol::state_file::state_path()` (Rust), i.e.
 * `data_dir()/daemon.json`, with the data root from the one shared resolver
 * (`@agent24/data-dir`, re-exported from `./data-dir`, ADR-037); unresolvable root → null. */
export function daemonStatePath(): string | null {
  const dir = tryDataDir()
  return dir === null ? null : joinLiteral(dir, 'daemon.json')
}

/** Read + parse `daemon.json`. Absent, unreadable, or malformed all mean the
 * same thing here — "nothing to reuse" — never a hard error; the caller falls
 * back to spawning its own daemon exactly as before this file's dep on it. */
export function readExternalDaemonState(): ExternalDaemonState | null {
  const statePath = daemonStatePath()
  if (!statePath) return null
  try {
    const raw = fs.readFileSync(statePath, 'utf8')
    const parsed = JSON.parse(raw) as {
      port?: unknown
      token?: unknown
      pid?: unknown
      auth_mode?: unknown
      protocol_version?: unknown
    }
    if (typeof parsed.port !== 'number' || typeof parsed.pid !== 'number') return null
    const token = typeof parsed.token === 'string' ? parsed.token : ''
    // Only a legacy record (or one from before `auth_mode` existed) shares its
    // token through this file; any other mode keeps host authority private.
    const legacy = parsed.auth_mode === undefined || parsed.auth_mode === 'legacy_single_token'
    const state: ExternalDaemonState = {
      port: parsed.port,
      token,
      pid: parsed.pid,
      authMode: legacy ? 'legacy' : 'capabilities',
    }
    const recorded = parsed.protocol_version
    if (typeof recorded === 'number' && Number.isInteger(recorded) && recorded >= 0) {
      state.protocolVersion = recorded
    }
    return state
  } catch {
    return null
  }
}

/** Discovery + health-probing, factored out so tests can inject both without
 * touching the real filesystem or making a real HTTP call. */
export interface ExternalDaemonSource {
  read(): ExternalDaemonState | null
  checkHealth(endpoint: BackendEndpoint | null): Promise<boolean>
  /** Body of `GET /api/v1/health`, including the protocol version. */
  fetchHealth(endpoint: BackendEndpoint): Promise<DaemonHealthProbe>
}

/** Constructor-injectable seams. Both default to the real implementations —
 * only tests pass overrides. */
export interface BackendManagerDeps {
  external?: ExternalDaemonSource
  /** Spawns/forks the backend child process. Defaulted to the real
   * spawn/fork call below; tests inject a fake so `spawnChild()` never
   * touches a real binary. */
  spawnProcess?: (kind: BackendKind, authMode: DaemonAuthMode) => ChildProcess
  /** Defaults to `selectedAuthMode(kind)`. */
  authMode?: DaemonAuthMode
  /** Electron `app.isPackaged`, injected by main.ts. Omitted → treated as
   * packaged (fail closed). */
  isPackaged?: boolean
  /** Test seam for the dev-time node-daemon resolution. */
  resolveNode?: () => string
}

/** Production probe for `tryReuseExternal`. Tests stub `globalThis.fetch`.
 * `/api/v1/health` takes no credential, so none is sent. */
export async function fetchDaemonHealth(endpoint: BackendEndpoint): Promise<DaemonHealthProbe> {
  try {
    const response = await fetch(`http://127.0.0.1:${endpoint.port}/api/v1/health`, {
      signal: AbortSignal.timeout(HEALTH_TIMEOUT_MS),
    })
    if (!response.ok) return { ok: false, protocolVersion: null }
    try {
      return { ok: true, protocolVersion: protocolVersionOf(await response.json()) }
    } catch {
      // 200 whose body is not JSON: a process is occupying the port.
      return { ok: true, protocolVersion: null }
    }
  } catch {
    return { ok: false, protocolVersion: null }
  }
}

// `/api/v1/health` takes no credential, so none is sent.
function checkHealth(endpoint: BackendEndpoint | null): Promise<boolean> {
  if (!endpoint) return Promise.resolve(false)
  return new Promise((resolve) => {
    const req = http.get(
      {
        host: '127.0.0.1',
        port: endpoint.port,
        path: '/api/v1/health',
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

function defaultSpawnProcess(kind: BackendKind, authMode: DaemonAuthMode, packaged: boolean): ChildProcess {
  if (kind === 'rust') {
    const bin = resolveRustBinary(packaged)
    if (authMode === 'capabilities') {
      // stdin is a pipe this process holds open for the child's lifetime: the
      // daemon treats EOF there as its host going away and shuts down.
      return spawn(bin, ['serve', '--port', '0', '--auth-mode', 'capabilities', '--host-bootstrap-stdio'], {
        stdio: ['pipe', 'pipe', 'pipe'],
        env: { ...process.env },
      })
    }
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

interface ReadyLine {
  type?: string
  port?: number
  token?: unknown
  product_host_token?: unknown
  auth_mode?: unknown
}

/** The credential the ready line hands over in the mode this manager asked
 * for; '' (so no connection) when the daemon answered in another mode. */
function readyToken(ready: ReadyLine, authMode: DaemonAuthMode): string {
  if (authMode === 'capabilities') {
    return ready.auth_mode === 'capabilities' && typeof ready.product_host_token === 'string'
      ? ready.product_host_token
      : ''
  }
  const legacy = ready.auth_mode === undefined || ready.auth_mode === 'legacy_single_token'
  return legacy && typeof ready.token === 'string' ? ready.token : ''
}

const defaultExternalDaemonSource: ExternalDaemonSource = {
  read: readExternalDaemonState,
  checkHealth,
  fetchHealth: fetchDaemonHealth,
}

export class BackendManager {
  private child: ChildProcess | null = null
  private healthTimer: NodeJS.Timeout | null = null
  private failureCount = 0
  private readonly kind: BackendKind
  private readonly external: ExternalDaemonSource
  private readonly spawnProcess: (kind: BackendKind, authMode: DaemonAuthMode) => ChildProcess
  private readonly authMode: DaemonAuthMode
  // F1b: last observed health (drives the tray status), and whether the user
  // deliberately stopped the daemon — while true, the health loop must NOT
  // auto-respawn it (that would defeat a manual stop).
  private lastHealthy = false
  private userStopped = false
  // DEP-A5: true while `current` points at a daemon this manager
  // discovered (via daemon.json) rather than spawned itself. Gates every
  // lifecycle action that would otherwise kill a process we don't own.
  private isExternal = false
  private externalPid: number | null = null
  // Set when a discoverable daemon is up but on another protocol. That process
  // holds the singleton lock, so spawning would fail and the health loop would
  // retry forever. While set, start/restart do not spawn and do not signal the
  // external pid. tick keeps probing: a compatible `/health` clears this and
  // reuses the peer; a failed probe clears it and spawns.
  private blockedReason: string | null = null
  // Set while a healthy capability-mode daemon of another host serves this
  // data dir (see tryReuseExternal); nothing is spawned until it is gone.
  private foreignPid: number | null = null
  // Reentrancy guard for tick(): setInterval doesn't wait for a previous
  // callback to resolve before scheduling the next one, and the failure-
  // threshold branch below now awaits a second health probe (tryReuseExternal)
  // on top of the one every tick already does — together those can approach
  // or exceed HEALTH_INTERVAL_MS. Without this guard an overlapping tick()
  // could double-count failures or spawn a second child while the first
  // tick is still mid-recovery (PR #602 review).
  private tickInFlight = false
  // Bumped by every manual lifecycle action (start/stop/restart from the tray,
  // app stop). A probe that was awaiting a health check when one of those ran
  // must not act on its stale result.
  private lifecycle = 0
  // Set by stop() (app quit): start() may still be awaiting its first probe
  // then, and must neither spawn nor arm the health loop afterwards.
  private disposed = false

  constructor(deps: BackendManagerDeps = {}) {
    this.kind = selectedBackend(deps.isPackaged ?? true, deps.resolveNode)
    this.external = deps.external ?? defaultExternalDaemonSource
    const packaged = deps.isPackaged ?? true
    this.spawnProcess =
      deps.spawnProcess ?? ((kind, authMode) => defaultSpawnProcess(kind, authMode, packaged))
    this.authMode = deps.authMode ?? selectedAuthMode(this.kind)
  }

  async start(): Promise<void> {
    // Only "nothing to reuse" may spawn. An incompatible or foreign daemon
    // already holds the singleton lock; spawning fails and the health loop
    // would retry forever.
    if ((await this.tryReuseExternal()) === 'absent') this.spawnChild()
    if (this.disposed) return
    this.healthTimer = setInterval(() => { void this.tick() }, HEALTH_INTERVAL_MS)
  }

  /** DEP-A5 step 1: before spawning anything, check whether a daemon started
   * outside this app (typically `agent24 daemon start`) is already up and
   * healthy — if so, point at it instead of racing it for the singleton lock.
   * Probe first. `incompatible` is only a live daemon whose `/health`
   * `protocol_version` this app must not attach to and must not spawn over.
   * The version recorded in daemon.json is named in that error; it is not a
   * reason to skip the probe. A healthy capability-mode daemon belongs to
   * another host: its authority never reaches this file, and spawning our own
   * would only lose the lock, so that is `foreign`. `absent` (no record, or
   * the probe did not connect) is the only outcome that may spawn. */
  private async tryReuseExternal(): Promise<'reused' | 'foreign' | 'absent' | 'incompatible' | 'superseded'> {
    const lifecycle = this.lifecycle
    const state = this.external.read()
    if (!state) {
      this.blockedReason = null
      this.foreignPid = null
      return 'absent'
    }
    const health = await this.external.fetchHealth({ port: state.port })
    if (lifecycle !== this.lifecycle) return 'superseded'
    if (!health.ok) {
      // Nothing answered. A leftover daemon.json must not block spawn, and a
      // peer that previously blocked us has released the lock.
      this.blockedReason = null
      this.foreignPid = null
      return 'absent'
    }
    const live = protocolCompatibility(health.protocolVersion, CLIENT_PROTOCOL_VERSION)
    if (!live.compatible) {
      this.foreignPid = null
      return this.refuseExternal(state, live.reason)
    }
    this.blockedReason = null
    if (state.authMode !== 'legacy') {
      if (this.foreignPid !== state.pid) {
        console.warn(`[backend:${this.kind}] daemon pid ${state.pid} serves this data dir in capability mode for another host — waiting for it to exit`)
      }
      // Coming from a reused legacy daemon that died: its port and token must
      // not outlive it here either.
      this.disconnectExternal()
      this.foreignPid = state.pid
      return 'foreign'
    }
    this.foreignPid = null
    this.isExternal = true
    this.externalPid = state.pid
    current = { port: state.port, auth: { kind: 'bearer', token: state.token } }
    this.lastHealthy = true
    console.log(
      `[backend:${this.kind}] reusing external daemon (pid ${state.pid}, port ${state.port}) — not spawning our own`,
    )
    return 'reused'
  }

  /** Live `/health` disagreed. The recorded version is only named in the hint. */
  private refuseExternal(state: ExternalDaemonState, reason: string): 'incompatible' {
    const recorded = state.protocolVersion === undefined
      ? 'daemon.json 没有记录协议版本'
      : `daemon.json 记录的协议版本为 ${state.protocolVersion}`
    const message =
      `${reason}。${recorded}。请升级较旧的一方，使两边协议版本一致，然后重新打开本应用。` +
      `外部 daemon（pid ${state.pid}）仍在运行，本应用不会终止它，也不会再启动另一个。`
    const already = this.blockedReason === message
    this.blockedReason = message
    this.isExternal = false
    this.externalPid = null
    this.lastHealthy = false
    current = null
    if (!already) {
      console.warn(
        `[backend:${this.kind}] 不复用外部 daemon（pid ${state.pid}，port ${state.port}）：${message}`,
      )
    }
    return 'incompatible'
  }

  /** pid of another host's capability-mode daemon while it blocks this app. */
  foreignDaemonPid(): number | null {
    return this.foreignPid
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
      hasEndpoint: current !== null,
      healthy: this.lastHealthy,
      incompatible: this.blockedReason !== null,
      foreignHost: this.foreignPid !== null,
    })
  }

  /** Upgrade hint while an incompatible external daemon blocks spawn. */
  statusDetail(): string | null {
    return this.blockedReason
  }

  /** Which backend implementation is running (shown in the tray tooltip). */
  backendKind(): BackendKind {
    return this.kind
  }

  /** Tray "启动 daemon": clear a prior manual stop and (re)spawn if needed. */
  startDaemon(): void {
    this.lifecycle += 1
    // Spawning against an incompatible peer's lock just restarts the failure
    // loop. (A `foreign` peer is different: a manual start is allowed through,
    // and its outcome is re-read on the next probe.)
    if (this.blockedReason !== null) return
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
    this.lifecycle += 1
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
    this.lifecycle += 1
    if (this.blockedReason !== null) return
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
    current = null
  }

  private spawnChild(): void {
    current = null
    // Any spawn (initial, respawn-after-failure, or an explicit restart)
    // always produces a desktop-owned child — never external past this point.
    this.isExternal = false
    this.externalPid = null
    this.foreignPid = null

    this.child = this.spawnProcess(this.kind, this.authMode)
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
          const parsed = JSON.parse(line) as ReadyLine
          // A ready line carries the credential: it is never echoed to the log,
          // even when malformed.
          if (parsed.type === 'ready') {
            if (typeof parsed.port === 'number' && this.child === child) {
              const auth: DaemonAuth =
                this.kind === 'node' ? { kind: 'none' } : { kind: 'bearer', token: readyToken(parsed, this.authMode) }
              current = { port: parsed.port, auth }
              console.log(`[backend:${this.kind}] ready on port ${parsed.port}`)
              if (auth.kind === 'bearer' && !auth.token) {
                console.error(`[backend:${this.kind}] ready line carries no credential for ${this.authMode} mode — requests are refused until restart`)
              }
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
        current = null
      }
    })
    child.on('error', (err) => {
      // spawn failures (e.g. ENOENT for a missing agent24d binary) emit
      // 'error' without a guaranteed 'exit' — clear state so the health loop
      // performs a plain (bounded-interval) respawn rather than leaking a ref
      console.error(`[backend:${this.kind}] process error`, err)
      if (this.child === child) {
        this.child = null
        current = null
      }
    })
  }

  private async tick(): Promise<void> {
    // Guard against overlapping invocations (see `tickInFlight` field doc):
    // skip this fire entirely rather than run a second tick concurrently
    // with one still in flight.
    if (this.tickInFlight) return
    this.tickInFlight = true
    try {
      // A deliberate stop suspends supervision entirely — don't probe, don't
      // count failures, and above all don't auto-respawn (F1b).
      if (this.userStopped) {
        this.lastHealthy = false
        return
      }
      // The external pid is not ours to kill, and another spawn cannot take
      // the singleton lock while that peer is still up. Keep probing so an
      // upgrade (or the peer exiting) recovers without reopening the app.
      if (this.blockedReason !== null) {
        this.lastHealthy = false
        const outcome = await this.tryReuseExternal()
        if (outcome === 'absent') {
          this.failureCount = 0
          this.spawnChild()
        }
        return
      }

      // Another host's daemon holds this data dir: wait for it to go away
      // instead of starting one that cannot take the singleton lock.
      if (this.foreignPid !== null) {
        this.lastHealthy = false
        if ((await this.tryReuseExternal()) === 'absent') this.spawnChild()
        return
      }

      const lifecycle = this.lifecycle
      const alive = await this.external.checkHealth(getBackendEndpoint())
      if (lifecycle !== this.lifecycle) return
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
        // PR #602 review (Medium): re-probe for an external daemon before
        // respawning our own. A near-simultaneous CLI + desktop startup can
        // make the very first tryReuseExternal() (in start()) miss a
        // daemon.json that simply hadn't been written yet — this manager
        // then spawns its own child, which loses the Rust singleton lock and
        // never binds a port, so it never reports healthy either. Without
        // this re-check, that self-spawned child would just get killed and
        // re-spawned forever every MAX_HEALTH_FAILURES ticks, even once the
        // real (CLI-owned) daemon is up and healthy. Only fall back to
        // spawning our own when the external daemon is still not reachable
        // (`absent`): an incompatible or foreign daemon parks this manager in
        // `error` / `blocked` instead, so the same loop cannot start there either.
        if ((await this.tryReuseExternal()) === 'absent') {
          this.spawnChild()
        }
      }
    } finally {
      this.tickInFlight = false
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
    current = null
  }

  stop(): void {
    this.lifecycle += 1
    this.disposed = true
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
