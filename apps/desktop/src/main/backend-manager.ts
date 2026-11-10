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
import { joinLiteral, readDaemonState, tryDataDir } from './data-dir'
import { enrol, EnrolError, RENEW_EVERY_MS, type EnrolSession, type EnrolTarget } from './daemon-enroll'

const HEALTH_INTERVAL_MS = 5_000
const HEALTH_TIMEOUT_MS = 3_000
const MAX_HEALTH_FAILURES = 3
/** A refused daemon.json whose /health stays silent this many probes in a row stops blocking manual start/restart. */
const REJECTED_SILENT_LIMIT = 3
// T1-0: how long a takeover waits for the CLI daemon to exit after `/shutdown`.
const TAKEOVER_WAIT_MS = 10_000
const TAKEOVER_POLL_MS = 200
// M5 B3a: after a failed enrolment, how long until the next try: doubling
// from one tick up to five minutes (PR-Daemon #1726).
const ENROL_RETRY_MIN_MS = HEALTH_INTERVAL_MS
const ENROL_RETRY_MAX_MS = 5 * 60_000


export type BackendKind = 'node' | 'rust'

export type BackendStatusDetail = ProtocolMismatchDetail | EnrolRefusedDetail

export interface ProtocolMismatchDetail {
  readonly kind: 'protocol_mismatch'
  readonly daemonVersion: number | null
  readonly clientVersion: number
  readonly recordedVersion?: number
  readonly pid: number
  /** Set when daemon.json was refused by the strict parser (e.g. an auth_mode this app does not know). */
  readonly reason?: string
  /** With `reason`: /health has been silent for a while; the record may be stale, delete daemon.json to recover. */
  readonly hint?: 'delete_daemon_json'
}

/** M5 B2: the daemon the LaunchAgent runs did not enrol this app. It holds the
 * singleton lock, so nothing is spawned; the tray says why. */
export interface EnrolRefusedDetail {
  readonly kind: 'enrol_refused'
  /** The daemon's error code (`identity_rejected`, `unsupported_version`, …) or a local one (`timeout`, `endpoint_mismatch`, …). */
  readonly code: string
  readonly pid: number
  /** Never set: kept so the tray's start/restart guard reads one field for both kinds. */
  readonly hint?: undefined
}

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
/** Auth mode of the daemon behind `current`, and only while that connection is usable. */
let currentAuthMode: DaemonAuthMode | null = null
/** The token connection behind `current` when it is an enrolment with the
 * service daemon (M5 B3); null for our own child and a reused legacy daemon. */
let currentSession: EnrolSession | null = null
const daemonReadyListeners = new Set<() => void>()

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

/**
 * Fires each time a usable daemon connection is published: the first ready
 * line, a reused daemon, and every later connection after a restart.
 * Re-publishing the same port and credential does not fire again.
 * The listener runs after `daemonConnection()` already returns the new value.
 */
export function onDaemonReady(listener: () => void): () => void {
  daemonReadyListeners.add(listener)
  return () => { daemonReadyListeners.delete(listener) }
}

/** A turn on the enrolment's token connection, while the current connection
 * is one with the service daemon (M5 B3): there a daily token gets
 * `403 presence_required` on an elevated route, and the grant is asked for on
 * that connection. Null otherwise: our own child keeps the host token. */
export function daemonElevation(): EnrolSession['turn'] | null {
  const session = currentSession
  return session ? (fn) => session.turn(fn) : null
}

/** Auth mode of the current usable connection. Null when nothing can be called. */
export function daemonAuthMode(): DaemonAuthMode | null {
  return daemonConnection() ? currentAuthMode : null
}

function connectionKey(): string | null {
  const conn = daemonConnection()
  if (!conn) return null
  return `${conn.port}\u0000${conn.authHeaders.Authorization ?? ''}\u0000${currentAuthMode ?? ''}`
}

function publishConnection(
  next: { port: number; auth: DaemonAuth } | null, authMode: DaemonAuthMode | null, session: EnrolSession | null = null,
): void {
  const before = connectionKey()
  current = next
  currentSession = next ? session : null
  const connected = daemonConnection() !== null
  currentAuthMode = connected ? authMode : null
  const after = connectionKey()
  if (!after || after === before) return
  for (const listener of [...daemonReadyListeners]) {
    try {
      listener()
    } catch (error) {
      console.error('[backend] daemon ready listener failed', error)
    }
  }
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
  /** M5 B2: a capability-mode daemon the LaunchAgent runs advertises its enrolment socket. */
  enrollEndpoint?: string
  /** The daemon's generation, echoed in `hello`. */
  generation?: string
  /** The strict parser refused this record but it still names a pid and port: never reusable,
   * yet a live daemon may hold the singleton lock, so it must not read as "nothing there". */
  rejected?: string
}

/** One `GET /api/v1/health`. `protocolVersion: null` means the field is absent. */
export interface DaemonHealthProbe {
  ok: boolean
  protocolVersion: number | null
  /** `/health`'s `version` (the daemon's package version), when it is a string. */
  version?: string | null
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
  // The one parser (`@agent24/data-dir`, mirrors Rust `state_file.rs`): a
  // record that fails its validation — including an unknown `auth_mode`, which
  // this function used to treat as "capabilities" — is nothing to reuse.
  const read = readDaemonState(statePath)
  if (!read.ok) {
    // File absent / unreadable / not a daemon record: nothing there. A refused record that
    // still names a pid and port (a newer auth_mode, a bad field) is a daemon we cannot
    // talk to, which is a different thing: callers probe it and must not spawn over it.
    if (read.endpoint === undefined) return null
    return { port: read.endpoint.port, pid: read.endpoint.pid, token: '', authMode: 'capabilities', rejected: read.message }
  }
  const { state } = read
  // Only a legacy record shares its token through this file; capability mode
  // keeps host authority private (token is '' there, by validation).
  const out: ExternalDaemonState = {
    port: state.port,
    token: state.token,
    pid: state.pid,
    authMode: state.authMode === 'legacy_single_token' ? 'legacy' : 'capabilities',
  }
  if (state.protocolVersion !== undefined) out.protocolVersion = state.protocolVersion
  if (state.enrollEndpoint !== undefined) {
    out.enrollEndpoint = state.enrollEndpoint
    out.generation = state.generation
  }
  return out
}

/** Discovery + health-probing, factored out so tests can inject both without
 * touching the real filesystem or making a real HTTP call. */
export interface ExternalDaemonSource {
  read(): ExternalDaemonState | null
  checkHealth(endpoint: BackendEndpoint | null): Promise<boolean>
  /** Body of `GET /api/v1/health`, including the protocol version. */
  fetchHealth(endpoint: BackendEndpoint): Promise<DaemonHealthProbe>
  /** True while `agent24 service install`'s LaunchAgent plist exists: launchd
   * starts the service daemon, so this app waits for it and never spawns its
   * own (M5 B4a). Not proof of who started a daemon that is up. */
  serviceInstalled(): boolean
  /** `GET /api/v1/os` with the record's token answers 200 with a `modules` array. */
  acceptsToken(endpoint: BackendEndpoint, token: string): Promise<boolean>
  /** `POST /api/v1/shutdown` with the record's token, as `agent24 daemon stop`
   * sends it: the HTTP status, or null when nothing answered. */
  requestShutdown(endpoint: BackendEndpoint, token: string): Promise<number | null>
  /** M5 B2: enrol as this app's first-party shell. Defaults to the real socket client. */
  enrol?(target: EnrolTarget): Promise<EnrolSession>
  /** Whether a process with this pid still exists. */
  isAlive(pid: number): boolean
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
      const body: unknown = await response.json()
      const version = (body as { version?: unknown } | null)?.version
      return { ok: true, protocolVersion: protocolVersionOf(body), version: typeof version === 'string' ? version : null }
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

/** Mirrors `agent24-cli`'s `service::plist_path()`. */
export function launchAgentPlistExists(): boolean {
  const home = process.env['HOME']
  return home !== undefined && fs.existsSync(path.join(home, 'Library', 'LaunchAgents', 'ai.auraai.agent24.plist'))
}

async function fetchWithToken(endpoint: BackendEndpoint, route: string, token: string, method = 'GET'): Promise<Response | null> {
  return fetch(`http://127.0.0.1:${endpoint.port}${route}`, {
    method, headers: { Authorization: `Bearer ${token}` }, signal: AbortSignal.timeout(HEALTH_TIMEOUT_MS),
  }).catch(() => null)
}

export async function daemonAcceptsToken(endpoint: BackendEndpoint, token: string): Promise<boolean> {
  const response = await fetchWithToken(endpoint, '/api/v1/os', token)
  if (response?.status !== 200) return false
  const body = (await response.json().catch(() => null)) as { modules?: unknown } | null
  return Array.isArray(body?.modules)
}

export async function requestDaemonShutdown(endpoint: BackendEndpoint, token: string): Promise<number | null> {
  return (await fetchWithToken(endpoint, '/api/v1/shutdown', token, 'POST'))?.status ?? null
}

export function processAlive(pid: number): boolean {
  // kill(0, 0) signals our own process group and pid 1 answers EPERM: neither is a
  // daemon that wrote this record (Rust pid_alive treats them as not alive too).
  if (!Number.isInteger(pid) || pid <= 1) return false
  try {
    process.kill(pid, 0)
    return true
  } catch (err) {
    return (err as NodeJS.ErrnoException).code === 'EPERM'
  }
}

const defaultExternalDaemonSource: ExternalDaemonSource = {
  read: readExternalDaemonState,
  checkHealth,
  fetchHealth: fetchDaemonHealth,
  serviceInstalled: launchAgentPlistExists,
  acceptsToken: daemonAcceptsToken,
  requestShutdown: requestDaemonShutdown,
  isAlive: processAlive,
  enrol: (target) => enrol(target),
}

/** How long whileStopped() waits for the daemon to exit: past the CLI's own
 * wait for a stopping daemon (agent24-cli WAIT_SECS, ~55 s), which covers the
 * daemon's worst-case shutdown (agent24-protocol shutdown_bounds.rs). */
export const DAEMON_EXIT_WAIT_MS = 60_000

/** True once `child` has exited; false if it has not within `timeoutMs`. */
function exited(child: ChildProcess, timeoutMs: number): Promise<boolean> {
  if (child.exitCode !== null && child.exitCode !== undefined) return Promise.resolve(true)
  if (child.signalCode !== null && child.signalCode !== undefined) return Promise.resolve(true)
  return new Promise((resolve) => {
    const timer = setTimeout(() => resolve(false), timeoutMs)
    child.once('exit', () => {
      clearTimeout(timer)
      resolve(true)
    })
  })
}

export class BackendManager {
  private child: ChildProcess | null = null
  /** Every daemon this manager spawned: their records are not the service's (M5 B5). */
  private readonly spawnedPids = new Set<number>()
  /** M5 B4a: when this app began waiting for the service daemon, or null. */
  private serviceWaitSince: number | null = null
  /** M5 B4a (Codex C2): the legacy daemon already asked to shut down. With the
   * LaunchAgent installed no child of ours replaces its record, so without
   * this it would be asked again, with its token, on every tick. */
  private shutdownAsked: number | null = null
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
  private blockedReason: BackendStatusDetail | null = null
  // Consecutive silent /health probes against a refused (unparseable) daemon.json record.
  private rejectedSilent = 0
  // Set while a healthy capability-mode daemon of another host serves this
  // data dir (see tryReuseExternal); nothing is spawned until it is gone.
  private foreignPid: number | null = null
  // M5 B2: the token connection to the daemon the LaunchAgent runs, while
  // that daemon is the reused external one, and its renew timer.
  private enrolment: EnrolSession | null = null
  /** Where `enrolment` was made, for the presence connection (M5 B3). */
  private enrolledWith: { socketPath: string; generation: string } | null = null
  /** The service daemon's `/health` version, read just before enrolling with it. */
  private serviceVersion: string | null = null
  /** M5 B5: while the service daemon restarts, its lock is not ours to take. */
  private serviceRestart = false
  private renewTimer: NodeJS.Timeout | null = null
  // Set when that connection ends under a live daemon: the next tick
  // enrols again rather than waiting out MAX_HEALTH_FAILURES.
  private reprobe = false
  // True while an enrolment is under way, or the probe of a re-enrolment
  // from its start: the service daemon is there, so a tray start/restart
  // must not spawn over it (Codex B2 C1, r2 C2).
  private enrolling = false
  private attaching = false
  // M5 B3a: the next enrolment try after a failed one, and the record it
  // failed against: a new daemon (pid or generation) is tried at once.
  private enrolRetryAt = 0
  private enrolRetryMs = 0
  private refusedBy: { pid: number; generation: string | undefined } | null = null
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
  /** Set while whileStopped() runs: whether to bring the daemon back after. */
  private held: { resume: boolean } | null = null
  /** The last whileStopped() call's end; null when none is pending. */
  private stoppedQueue: Promise<void> | null = null
  // T1-1: what start() waits for before it reuses or spawns anything (the
  // bundled documents module is installed first, so the daemon mounts it).
  // While it is pending, the tray's start / restart / stop only record what
  // the user wants; nothing is spawned ahead of it.
  private startGate: Promise<unknown> | null = null
  private gateOpen = true

  constructor(deps: BackendManagerDeps = {}) {
    this.kind = selectedBackend(deps.isPackaged ?? true, deps.resolveNode)
    this.external = deps.external ?? defaultExternalDaemonSource
    const packaged = deps.isPackaged ?? true
    this.spawnProcess =
      deps.spawnProcess ?? ((kind, authMode) => defaultSpawnProcess(kind, authMode, packaged))
    this.authMode = deps.authMode ?? selectedAuthMode(this.kind)
  }

  /** T1-1: start() waits for `gate` (which must not reject) before it does
   * anything. Call before start(). */
  holdStartUntil(gate: Promise<unknown>): void {
    this.startGate = gate
    this.gateOpen = false
  }

  async start(): Promise<void> {
    if (this.startGate) {
      await this.startGate.catch(() => {})
      this.gateOpen = true
      if (this.disposed) return
    }
    // A tray stop while start() was held: stay down until the user starts it
    // (the health loop below already leaves a user-stopped daemon alone).
    if (!this.userStopped) {
      // Only "nothing to reuse" may spawn. An incompatible or foreign daemon
      // already holds the singleton lock; spawning fails and the health loop
      // would retry forever.
      const reuse = await this.tryReuseExternal()
      if (this.disposed) return
      if (reuse === 'absent') this.spawnChild()
    }
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
   * would only lose the lock, so that is `foreign` — unless daemon.json names
   * its enrolment socket (M5 B2, the LaunchAgent's daemon): then this app
   * enrols and reuses it, or reports why it could not. A healthy legacy daemon is
   * taken over (T1-0) unless the LaunchAgent manages it or this app runs in
   * legacy mode; then it is reused once its token is proven on that port.
   * `absent` (no record, the probe did not connect, a takeover, or a token the
   * port refused) is the only outcome that may spawn. */
  private async tryReuseExternal(): Promise<'reused' | 'foreign' | 'absent' | 'incompatible' | 'superseded'> {
    const lifecycle = this.lifecycle
    const state = this.external.read()
    if (!state || state.rejected === undefined) this.rejectedSilent = 0
    // A record whose pid is gone is stale: whatever holds its port now gets
    // neither a probe nor the token (the Rust readers check the same).
    if (!state || !this.external.isAlive(state.pid)) {
      this.blockedReason = null
      this.foreignPid = null
      return 'absent'
    }
    const health = await this.external.fetchHealth({ port: state.port })
    if (lifecycle !== this.lifecycle) return 'superseded'
    if (state.rejected !== undefined) {
      // A live pid wrote a record this app refuses to parse. It holds the singleton
      // lock, so spawning would loop forever: report it, never `absent`. Never reused.
      this.foreignPid = null
      this.rejectedSilent = health.ok ? 0 : this.rejectedSilent + 1
      return this.refuseExternal(state, health.ok ? health.protocolVersion : null)
    }
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
      return this.refuseExternal(state, health.protocolVersion)
    }
    if (state.authMode !== 'legacy' && state.enrollEndpoint !== undefined && state.generation !== undefined) {
      this.foreignPid = null
      this.serviceVersion = health.version ?? null
      return this.enrolExternal(state, state.enrollEndpoint, state.generation, lifecycle)
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
      this.serviceWaitSince = null
      return 'foreign'
    }
    this.foreignPid = null
    // T1-0: Knowledge needs a capability-mode daemon this app hosts, so a
    // legacy one is taken over. Only a legacy daemon gets here; the service
    // daemon enrolled above (M5 B6). With the LaunchAgent installed the
    // takeover then waits for the service daemon instead of spawning (M5 B4a).
    if (this.kind === 'rust' && this.authMode === 'capabilities') {
      if (this.shutdownAsked === state.pid) return 'absent'
      return this.takeoverExternal(state, lifecycle)
    }
    // #1187: /health takes no credential; prove the token on that port first.
    const accepted = await this.external.acceptsToken({ port: state.port }, state.token)
    if (lifecycle !== this.lifecycle) return 'superseded'
    if (!accepted) {
      console.warn(`[backend:${this.kind}] daemon.json token not accepted by port ${state.port}; not reusing`)
      this.disconnectExternal()
      return 'absent'
    }
    this.serviceWaitSince = null
    this.isExternal = true
    this.externalPid = state.pid
    publishConnection({ port: state.port, auth: { kind: 'bearer', token: state.token } }, state.authMode)
    this.lastHealthy = true
    console.log(
      `[backend:${this.kind}] reusing external daemon (pid ${state.pid}, port ${state.port}) — not spawning our own`,
    )
    return 'reused'
  }

  /** M5 B2: a capability-mode daemon with an enrolment socket is the one the
   * LaunchAgent runs (`agent24 service install`). Enrol as this app's
   * first-party shell and use the day-to-day token; never spawn over it. A
   * refused or failed enrolment parks the manager in `error` with its own
   * detail: the daemon holds the lock, so it is not `absent`, and it is
   * reachable, so it is not `foreign`. */
  private async enrolExternal(
    state: ExternalDaemonState, socketPath: string, generation: string, lifecycle: number,
  ): Promise<'reused' | 'incompatible' | 'superseded'> {
    this.disconnectExternal()
    let session: EnrolSession
    this.enrolling = true
    try {
      session = await (this.external.enrol ?? enrol)({ socketPath, generation, httpPort: state.port })
    } catch (error) {
      if (lifecycle !== this.lifecycle) return 'superseded'
      return this.refuseEnrolment(state, error instanceof EnrolError ? error.code : 'internal')
    } finally {
      this.enrolling = false
    }
    if (lifecycle !== this.lifecycle) {
      session.close()
      return 'superseded'
    }
    this.blockedReason = null
    // Enrolled: an earlier failure's backoff does not carry over (Codex B3a C1).
    this.enrolRetryMs = 0
    this.refusedBy = null
    this.enrolment = session
    this.serviceWaitSince = null
    this.shutdownAsked = null
    this.isExternal = true
    this.externalPid = state.pid
    this.lastHealthy = true
    this.enrolledWith = { socketPath, generation }
    publishConnection({ port: state.port, auth: { kind: 'bearer', token: session.token } }, 'capabilities', session)
    this.renewTimer = setInterval(() => { void this.renewEnrolment(session, state.port) }, RENEW_EVERY_MS)
    session.onClose((why) => {
      if (this.enrolment !== session) return
      console.warn(`[backend:${this.kind}] enrolment with the service daemon (pid ${state.pid}) ended (${why.code}); enrolling again`)
      this.disconnectExternal()
      this.lastHealthy = false
      this.reprobe = true
    })
    console.log(`[backend:${this.kind}] enrolled as first-party shell with the service daemon (pid ${state.pid}, port ${state.port}) — not spawning our own`)
    return 'reused'
  }

  /** Renew on the token connection; the old token is void once the new one
   * arrives, so it is published at once. A failed renew closes the
   * connection, and its close handler enrols again. */
  private async renewEnrolment(session: EnrolSession, port: number): Promise<void> {
    if (this.enrolment !== session) return
    try {
      const token = await session.renew()
      if (this.enrolment === session) publishConnection({ port, auth: { kind: 'bearer', token } }, 'capabilities', session)
    } catch {
      // onClose has it.
    }
  }

  private refuseEnrolment(state: ExternalDaemonState, code: string): 'incompatible' {
    // A daemon is up and said why it will not have this app: not a wait (Codex B4a C3).
    this.serviceWaitSince = null
    const already = this.blockedReason?.kind === 'enrol_refused'
      && this.blockedReason.pid === state.pid && this.blockedReason.code === code
    const sameDaemon = this.refusedBy?.pid === state.pid && this.refusedBy.generation === state.generation
    this.enrolRetryMs = sameDaemon ? Math.min(this.enrolRetryMs * 2, ENROL_RETRY_MAX_MS) : ENROL_RETRY_MIN_MS
    this.enrolRetryAt = Date.now() + this.enrolRetryMs
    this.refusedBy = { pid: state.pid, generation: state.generation }
    this.blockedReason = { kind: 'enrol_refused', code, pid: state.pid }
    this.isExternal = false
    this.externalPid = null
    this.lastHealthy = false
    publishConnection(null, null)
    if (!already) {
      console.warn(`[backend:${this.kind}] the service daemon (pid ${state.pid}, port ${state.port}) did not enrol this app (${code}); not spawning over it`)
    }
    return 'incompatible'
  }

  /** True while the service daemon is being enrolled with, or is to be:
   * then a tray start/restart only lifts a user stop (M5 B2). */
  private serviceAttach(): boolean {
    if (!this.enrolling && !this.attaching && !this.reprobe) {
      // Not attaching yet, but no child of ours and the service daemon is on
      // record and alive: attach to it at the next tick, never spawn over it
      // (a steady disconnect → start, or a stop that superseded a re-enrolment;
      // PR-Daemon #1726).
      if (this.child || this.blockedReason !== null || !this.serviceDaemonOnRecord()) return false
      this.reprobe = true
    }
    this.userStopped = false
    this.failureCount = 0
    return true
  }

  /** daemon.json names a live capability-mode daemon with an enrolment socket. */
  private serviceDaemonOnRecord(): boolean {
    const state = this.external.read()
    return state !== null && state.rejected === undefined && state.authMode !== 'legacy'
      && state.enrollEndpoint !== undefined && this.external.isAlive(state.pid)
  }

  /** While a failed enrolment waits out its backoff, against the same live daemon. */
  private enrolRetryPending(): boolean {
    if (this.blockedReason?.kind !== 'enrol_refused' || Date.now() >= this.enrolRetryAt) return false
    const state = this.external.read()
    return state !== null && this.external.isAlive(state.pid)
      && state.pid === this.refusedBy?.pid && state.generation === this.refusedBy.generation
  }

  /** Close the token connection, if any. */
  private dropEnrolment(): void {
    const session = this.enrolment
    this.enrolment = null
    this.enrolledWith = null
    if (this.renewTimer) {
      clearInterval(this.renewTimer)
      this.renewTimer = null
    }
    session?.close()
  }

  /** T1-0: ask a CLI-started legacy daemon to shut down with its own token,
   * then wait (bounded) for its process to exit — it drops daemon.json and its
   * port when shutdown starts, but holds the singleton lock until exit. Every
   * outcome lets the caller spawn; a lock still held is the health loop's. */
  private async takeoverExternal(state: ExternalDaemonState, lifecycle: number): Promise<'absent' | 'superseded'> {
    this.disconnectExternal()
    this.shutdownAsked = state.pid
    console.log(`[backend:${this.kind}] taking over CLI daemon (pid ${state.pid}, port ${state.port}) so Knowledge can work — asking it to shut down`)
    const status = await this.external.requestShutdown({ port: state.port }, state.token)
    // Stopped meanwhile: this ask is not the one that counts (PR-Daemon B4a N2).
    if (lifecycle !== this.lifecycle) {
      if (this.shutdownAsked === state.pid) this.shutdownAsked = null
      return 'superseded'
    }
    if (status === null || status < 200 || status > 299) {
      console.warn(`[backend:${this.kind}] CLI daemon refused shutdown (${status ?? 'no answer'}); starting our own anyway`)
      return 'absent'
    }
    const deadline = Date.now() + TAKEOVER_WAIT_MS
    while (this.external.isAlive(state.pid) && Date.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, TAKEOVER_POLL_MS))
      if (lifecycle !== this.lifecycle) return 'superseded'
    }
    if (this.external.isAlive(state.pid)) {
      console.warn(`[backend:${this.kind}] CLI daemon pid ${state.pid} still running after ${TAKEOVER_WAIT_MS} ms; starting our own anyway`)
    }
    return 'absent'
  }

  /** Live `/health` disagreed. The recorded version is only named in the hint. */
  private refuseExternal(state: ExternalDaemonState, daemonVersion: number | null): 'incompatible' {
    this.serviceWaitSince = null
    const detail: ProtocolMismatchDetail = {
      kind: 'protocol_mismatch', daemonVersion, clientVersion: CLIENT_PROTOCOL_VERSION,
      recordedVersion: state.protocolVersion, pid: state.pid,
      ...(state.rejected === undefined ? {} : { reason: state.rejected }),
      ...(state.rejected !== undefined && this.rejectedSilent >= REJECTED_SILENT_LIMIT ? { hint: 'delete_daemon_json' as const } : {}),
    }
    const already = this.blockedReason?.kind === 'protocol_mismatch'
      && this.blockedReason.pid === detail.pid
      && this.blockedReason.reason === detail.reason
      && this.blockedReason.hint === detail.hint
      && this.blockedReason.daemonVersion === detail.daemonVersion
      && this.blockedReason.recordedVersion === detail.recordedVersion
    this.blockedReason = detail
    this.dropEnrolment()
    this.isExternal = false
    this.externalPid = null
    this.lastHealthy = false
    publishConnection(null, null)
    if (!already) {
      console.warn(
        `[backend:${this.kind}] refusing external daemon (port ${state.port}): ${JSON.stringify(detail)}`,
      )
    }
    return 'incompatible'
  }

  /** M5 B4a: how long this app has been waiting for the LaunchAgent's service
   * daemon instead of spawning its own (milliseconds), or null when it is not
   * waiting: no LaunchAgent, a daemon is attached or spawned, or the user
   * stopped it from the tray. */
  serviceWaitMs(): number | null {
    return this.serviceWaitSince === null ? null : Date.now() - this.serviceWaitSince
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

  /** The service daemon's enrolment socket and generation while this app is
   * enrolled with it: the presence connection's target (M5 B3). */
  presenceTarget(): { socketPath: string; generation: string } | null {
    return this.enrolledWith
  }

  /** Whether this manager spawned `pid` (M5 B5: its restarts are not the service's). */
  spawned(pid: number): boolean {
    return this.spawnedPids.has(pid)
  }

  /** The enrolled service daemon's version; null when not enrolled (M5 B5). */
  serviceDaemonVersion(): string | null {
    return this.enrolledWith ? this.serviceVersion : null
  }

  /** M5 B5: runs `work` (the service daemon's restart) with nothing spawned
   * meanwhile, then looks for the service daemon again. */
  async whileServiceRestarts<T>(work: () => Promise<T>): Promise<T> {
    this.serviceRestart = true
    try {
      return await work()
    } finally {
      this.serviceRestart = false
      this.reprobe = true
    }
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
  statusDetail(): BackendStatusDetail | null {
    return this.blockedReason
  }

  /** Which backend implementation is running (shown in the tray tooltip). */
  backendKind(): BackendKind {
    return this.kind
  }

  /** Tray "启动 daemon": clear a prior manual stop and (re)spawn if needed. */
  startDaemon(): void {
    if (this.held) {
      this.held.resume = true
      return
    }
    // The service daemon is there, enrolling or about to again (M5 B2):
    // nothing to spawn, and the enrolment in flight must not be superseded.
    // A stop is undone all the same, so the tick enrols again (Codex B2 r2 C1).
    if (this.serviceAttach()) return
    this.lifecycle += 1
    // Spawning against an incompatible peer's lock just restarts the failure
    // loop. (A `foreign` peer is different: a manual start is allowed through,
    // and its outcome is re-read on the next probe.)
    if (this.blockedReason !== null && this.blockedReason.hint === undefined) return
    this.userStopped = false
    this.failureCount = 0
    if (!this.gateOpen) return // start() spawns once its gate opens
    if (!this.child && !this.isExternal) this.spawnChild()
  }

  /** Tray "停止 daemon": for a daemon this manager spawned, kill the child and
   * keep it down (no auto-respawn) — unchanged from before DEP-A5.
   * For a reused *external* daemon (DEP-A5 requirement #2; since T1-0 one the
   * LaunchAgent manages, or any for a legacy-mode app), "stop" must never kill
   * it — it is not this app's. Instead this only forgets the connection: `userStopped` still
   * pins the tray to "stopped" and suspends the health loop, exactly as a
   * real stop would, but the external process itself is left untouched. */
  stopDaemon(): void {
    this.serviceWaitSince = null
    // A Start after this asks the legacy daemon again, once (PR-Daemon B4a N2).
    this.shutdownAsked = null
    if (this.held) this.held.resume = false
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
    if (this.held) {
      this.held.resume = true
      return
    }
    if (this.serviceAttach()) return
    this.lifecycle += 1
    if (this.blockedReason !== null && this.blockedReason.hint === undefined) return
    this.userStopped = false
    this.failureCount = 0
    this.lastHealthy = false
    if (!this.gateOpen) return // start() spawns once its gate opens
    if (this.isExternal) {
      this.disconnectExternal()
    } else {
      this.killChild()
    }
    this.spawnChild()
  }

  /**
   * T1-2: runs `work` with the daemon this app spawned stopped — waited for
   * until it has exited, so its singleton lock is free — and brings it back
   * afterwards unless the user had stopped it. A capabilities-mode daemon
   * holds the lock and gives the CLI no credential, so `agent24 os
   * install|uninstall` cannot revoke consent while it runs (agent24-cli
   * `attach_only` → offline store → busy). A reused external daemon is not
   * ours to stop: `work` just runs, and the CLI reaches it as it can.
   */
  whileStopped<T>(work: () => Promise<T>, exitTimeoutMs = DAEMON_EXIT_WAIT_MS): Promise<T> {
    // One hold at a time: `held` belongs to one (Codex B4b C1, the first-launch
    // install and a module action at once). The first runs at once, as before.
    const prior = this.stoppedQueue
    const run = prior ? prior.then(() => this.holdStopped(work, exitTimeoutMs)) : this.holdStopped(work, exitTimeoutMs)
    const tail: Promise<void> = run.then(() => {}, () => {}).then(() => { if (this.stoppedQueue === tail) this.stoppedQueue = null })
    this.stoppedQueue = tail
    return run
  }

  private async holdStopped<T>(work: () => Promise<T>, exitTimeoutMs: number): Promise<T> {
    if (this.isExternal || this.disposed) return work()
    const resume = !this.userStopped
    const child = this.child
    this.stopDaemon()
    // Until `work` is done, the tray only records what the user wants: a
    // daemon started meanwhile would take the lock the CLI needs.
    this.held = { resume }
    try {
      if (child && !(await exited(child, exitTimeoutMs))) {
        throw new Error(`the daemon (pid ${String(child.pid)}) did not exit within ${exitTimeoutMs / 1000} s`)
      }
      return await work()
    } finally {
      const wanted = this.held.resume
      this.held = null
      if (wanted && !this.disposed) this.startDaemon()
    }
  }

  /** Forget a reused external daemon without touching the process itself. */
  private disconnectExternal(): void {
    this.dropEnrolment()
    this.isExternal = false
    this.externalPid = null
    publishConnection(null, null)
  }

  private spawnChild(): void {
    if (this.serviceRestart) return
    this.dropEnrolment()
    // Our own child from here on: its daemon.json is not the service's to
    // enrol with again (Codex B2 C2).
    this.reprobe = false
    publishConnection(null, null)
    // Any spawn (initial, respawn-after-failure, or an explicit restart)
    // always produces a desktop-owned child — never external past this point.
    this.isExternal = false
    this.externalPid = null
    this.foreignPid = null
    // M5 B4a: launchd starts the service daemon; spawning ours would race it
    // for the singleton lock. Wait, and look for it again on the next tick
    // (the connection to a daemon that died is already let go above).
    if (this.external.serviceInstalled()) {
      this.serviceWaitSince ??= Date.now()
      this.reprobe = true
      return
    }
    this.serviceWaitSince = null
    this.shutdownAsked = null

    this.child = this.spawnProcess(this.kind, this.authMode)
    if (this.child.pid !== undefined) this.spawnedPids.add(this.child.pid)
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
              publishConnection(
                { port: parsed.port, auth },
                this.kind === 'node' ? 'legacy' : this.authMode,
              )
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
        publishConnection(null, null)
      }
    })
    child.on('error', (err) => {
      // spawn failures (e.g. ENOENT for a missing agent24d binary) emit
      // 'error' without a guaranteed 'exit' — clear state so the health loop
      // performs a plain (bounded-interval) respawn rather than leaking a ref
      console.error(`[backend:${this.kind}] process error`, err)
      if (this.child === child) {
        this.child = null
        publishConnection(null, null)
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
        // A refused enrolment is tried again on a backoff, at once for a new daemon.
        if (this.enrolRetryPending()) return
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

      // M5 B2: the token connection ended under a live service daemon.
      if (this.reprobe) {
        this.reprobe = false
        this.attaching = true
        let outcome: Awaited<ReturnType<BackendManager['tryReuseExternal']>>
        try {
          outcome = await this.tryReuseExternal()
        } finally {
          this.attaching = false
        }
        if (outcome === 'absent') this.spawnChild()
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
    publishConnection(null, null)
  }

  stop(): void {
    this.lifecycle += 1
    this.disposed = true
    if (this.healthTimer) {
      clearInterval(this.healthTimer)
      this.healthTimer = null
    }
    // App exit must never kill a reused external daemon (DEP-A5 requirement
    // #2) — just stop watching it. A daemon taken over (T1-0) is replaced by our child.
    if (this.isExternal) {
      this.disconnectExternal()
      return
    }
    this.killChild()
  }
}
