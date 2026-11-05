import { spawn, type ChildProcess } from 'node:child_process'
import fs from 'node:fs'
import openDesignPin from '../shared/open-design.pin.json'
import path from 'node:path'
import type { Readable } from 'node:stream'

import { assertResourceSafeBaseAncestryIsSafe, verifyEntryFileIntegrity } from './open-design-component'
import {
  OpenDesignHealth,
  OpenDesignLauncher,
  openDesignSidecarSpec,
  AGENT24_HEADLESS_PROTOCOL,
  OPEN_DESIGN_READY_PHASE_TIMEOUT_MS,
  creativeChildEnvironment,
  resolveBundledAgent24Cli,
  type KillFn,
  type OpenDesignLaunchPlan,
  type OpenDesignSidecarTimings,
} from './open-design-sidecar'
import { MemoryEndpointHandoff, type SidecarState } from './sidecar-contract'
import { SidecarManager, exactTreeStopper, type SidecarLogger } from './sidecar-manager'

// These moved to open-design-sidecar.ts (the launcher needs them); re-exported so callers are unchanged.
export {
  creativeChildEnvironment,
  parseAgent24HeadlessReady,
  resolveBundledAgent24Cli,
  resolveNodeRuntimeExecutable,
  type Agent24HeadlessReady,
} from './open-design-sidecar'

export type CreativeServeWebState = 'stopped' | 'starting' | 'ready' | 'failed' | 'needs-download'

export interface CreativeServeWebStatus {
  state: CreativeServeWebState
  origin?: string
  error?: string
  /** Set when state === 'needs-download': the Open Design component's
   * declared download size, surfaced from the baked manifest, so the
   * renderer can show "~N MB" without a round trip of its own. */
  size?: number
  /** Set with state 'failed' when no Open Design runtime exists at all (no
   * downloadable component for this build, no local checkout). `error` keeps
   * the technical detail for logs; the UI shows a neutral not-installed note. */
  notInstalled?: boolean
  /** Set with a headless `ready`: the sidecar instance serving `origin`. It changes on every
   * launch, even when the new instance reuses the old port, so the view can tell a stale page. */
  instanceId?: string
  /** Set (true) with `ready` when health checks have failed repeatedly. Main-process only; it is
   * not sent to the renderer, and it never triggers a restart (a long generation may be running). */
  degraded?: boolean
}

/** The subset of OpenDesignComponentInstaller's surface CreativeServeWeb
 * needs — kept narrow and duck-typed here so this file never imports
 * Electron-coupled installer wiring directly (keeps the existing unit tests
 * free of filesystem/network fixtures unless they opt in). */
export interface OpenDesignComponentAccessor {
  status(): { state: string; dir?: string; size?: number }
}

export interface CreativeServeWebOptions {
  checkoutDir?: string
  resourcesPath?: string
  stateRoot?: string
  runtimeExecutable?: string
  pinVersion?: string
  environment?: NodeJS.ProcessEnv
  nodeBinary?: string
  port?: number
  readyTimeoutMs?: number
  /** On-demand Open Design component (owner decision 2026-10-05: no longer
   * bundled in the installer). When set and no explicit dev checkout/legacy
   * bundled resources are found, CreativeServeWeb consults this instead of
   * `resourcesPath` to locate the headless launcher + resource root. */
  component?: OpenDesignComponentAccessor
  /** The `agent24` CLI the Open Design `agent24` runtime should run, passed as
   * AGENT24_BIN. Defaults to the copy bundled in Agent24.app; null disables. */
  agent24Cli?: string | null
  /** Sidecar lifecycle timings; defaults are the ADR-038 D5 values (tests shorten them). */
  sidecarTimings?: OpenDesignSidecarTimings
}

/** Test seams for the sidecar path. Production uses `process.kill` and `console`. */
export interface CreativeServeWebDeps {
  kill?: KillFn
  logger?: SidecarLogger
  platform?: NodeJS.Platform
}

const consoleSidecarLogger: SidecarLogger = {
  info: (event, fields) => console.info(`[sidecar] ${event}`, fields ?? {}),
  warn: (event, fields) => console.warn(`[sidecar] ${event}`, fields ?? {}),
}

type SpawnFn = typeof spawn
type FetchFn = typeof fetch
type CreativeChild = ChildProcess & { stdout: Readable; stderr: Readable }

const DEFAULT_PORT = 7456
const DEFAULT_READY_TIMEOUT_MS = 15_000
const CHECKOUT_STOP_TIMEOUT_MS = 5_000
/** before-quit waits at most this long for Creative to stop: 15 s grace + 5 s kill + slack. */
export const CREATIVE_QUIT_STOP_BUDGET_MS = 22_000
export const OPEN_DESIGN_PIN_VERSION = openDesignPin.version

export const CREATIVE_SESSION_PARTITION = 'persist:agent24-creative'

export type CreativeUrlDisposition = 'same-origin' | 'external-http' | 'blocked'

export class CreativeViewRequestFence {
  private generation = 0
  private desiredVisible = false

  begin(): number {
    this.desiredVisible = true
    this.generation += 1
    return this.generation
  }

  invalidate(): void {
    this.desiredVisible = false
    this.generation += 1
  }

  isCurrent(generation: number): boolean {
    return this.desiredVisible && this.generation === generation
  }

  wantsVisible(): boolean {
    return this.desiredVisible
  }
}

export function canonicalCreativeOrigin(raw: string, expectedPort: number): string {
  let url: URL
  try {
    url = new URL(raw)
  } catch {
    throw new Error('Open Design reported an invalid origin')
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') {
    throw new Error('Open Design origin must use HTTP(S)')
  }
  if (url.hostname !== '127.0.0.1') {
    throw new Error('Open Design origin must use 127.0.0.1')
  }
  if (url.username || url.password || url.pathname !== '/' || url.search || url.hash) {
    throw new Error('Open Design origin must not include credentials, a path, query, or fragment')
  }
  const port = Number(url.port || (url.protocol === 'https:' ? 443 : 80))
  if (port !== expectedPort) throw new Error('Open Design origin port does not match the launched port')
  return url.origin
}

/**
 * Wait for `service.stop()` for at most `budgetMs`; never rejects. Resolves `true` when the stop
 * finished inside the budget. before-quit uses this so a stuck tree cannot hold the app open.
 */
export async function stopWithin(service: { stop: () => Promise<unknown> }, budgetMs: number): Promise<boolean> {
  let timer: NodeJS.Timeout | undefined
  try {
    return await Promise.race([
      service.stop().then(() => true, () => false),
      new Promise<boolean>((resolve) => { timer = setTimeout(() => resolve(false), budgetMs) }),
    ])
  } finally {
    if (timer) clearTimeout(timer)
  }
}

/**
 * Whether the Creative view must (re)load. A new sidecar instance may reuse the previous port, so
 * an origin match alone would leave the old instance's page on screen.
 */
export function shouldReloadCreativeView(input: {
  forceReload: boolean
  currentUrl: string
  origin: string
  loadedInstanceId: string | null
  instanceId: string | undefined
}): boolean {
  if (input.forceReload) return true
  if (input.instanceId !== undefined && input.instanceId !== input.loadedInstanceId) return true
  return classifyCreativeUrl(input.currentUrl, input.origin) !== 'same-origin'
}

export async function loadCreativeView(input: Parameters<typeof shouldReloadCreativeView>[0] & {
  loadURL: (url: string) => Promise<unknown>
  isCurrent: () => boolean
  rememberInstance: (instanceId: string | null) => void
}): Promise<void> {
  if (!shouldReloadCreativeView(input)) return
  await input.loadURL(input.origin)
  if (input.isCurrent()) input.rememberInstance(input.instanceId ?? null)
}

export function classifyCreativeUrl(candidate: string, allowedOrigin: string): CreativeUrlDisposition {
  let url: URL
  let allowed: URL
  try {
    url = new URL(candidate)
    allowed = new URL(allowedOrigin)
  } catch {
    return 'blocked'
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') return 'blocked'
  if (url.origin === allowed.origin) return 'same-origin'
  return 'external-http'
}

export function resolveOpenDesignCheckout(
  cwd = process.cwd(),
  env: NodeJS.ProcessEnv = process.env,
  resourcesPath?: string,
): string | null {
  const explicit = env.A24_OPEN_DESIGN_DIR?.trim()
  const candidates = [
    explicit || null,
    resourcesPath ? path.join(resourcesPath, 'open-design') : null,
    path.resolve(cwd, '../../../open-design-agent24'),
    path.resolve(cwd, '../open-design-agent24'),
  ].filter((candidate): candidate is string => Boolean(candidate))

  return candidates.find((candidate) =>
    fs.existsSync(path.join(candidate, 'apps/daemon/bin/od.mjs')),
  ) ?? null
}

export function resolveAgent24HeadlessLauncher(resourcesPath?: string): string | null {
  if (!resourcesPath) return null
  const entry = path.join(resourcesPath, 'app', 'prebundled', 'agent24-headless.cjs')
  return fs.existsSync(entry) ? entry : null
}

export class CreativeServeWeb {
  /** Dev checkout child only. The packaged headless instance lives in `manager`. */
  private child: CreativeChild | null = null
  private storedStatus: CreativeServeWebStatus = { state: 'stopped' }
  private startGeneration = 0
  private starting: Promise<CreativeServeWebStatus> | null = null
  private stopping: Promise<CreativeServeWebStatus> | null = null
  private closed = false
  private explicitStops = 0
  private readonly statusListeners = new Set<(status: CreativeServeWebStatus) => void>()

  // --- packaged headless instance: everything about its process is the manager's ---
  private readonly handoff = new MemoryEndpointHandoff()
  private launchPlan: OpenDesignLaunchPlan | null = null
  private readonly manager: SidecarManager
  /** Set while a headless `manager.start()` is in flight; its outcome is reported by that call, not by events. */
  private launching = false
  /** A headless launch happened since the phase was last cleared. Without it the phase is null. */
  private launchedSinceClear = false
  /** A failure before any process existed (integrity check, bad paths). Cleared by the next start. */
  private preflightFailed = false

  private get current(): CreativeServeWebStatus {
    return this.storedStatus
  }

  private set current(value: CreativeServeWebStatus) {
    this.storedStatus = value
    this.emitStatus()
  }

  /** Fires after every status assignment. Listeners must not throw into the launch path. */
  onStatus(listener: (status: CreativeServeWebStatus) => void): () => void {
    this.statusListeners.add(listener)
    return () => { this.statusListeners.delete(listener) }
  }

  /**
   * Sidecar state while the creative view's headless instance is in a run, derived from the
   * manager. Null for the dev checkout and before the first headless launch.
   */
  headlessSidecarState(): SidecarState | null {
    if (this.preflightFailed) return 'failed'
    if (!this.launchedSinceClear) return null
    return this.manager.status().state
  }

  /**
   * True while any Open Design process may exist: the dev checkout child, or a headless
   * instance that is not cleanly stopped (including a failed one whose owner is still retained).
   */
  hasLiveProcess(): boolean {
    if (this.child) return true
    const { state, instanceId } = this.manager.status()
    return state !== 'stopped' && !(state === 'failed' && instanceId === undefined)
  }

  /**
   * Phase to put on an Open Design report (ADR-038 D5).
   *
   * A non-`installed` snapshot drops only a terminal leftover. `stopped` is
   * one: the run is over, and the next `installed` snapshot must be
   * `installed`, not `installing → stopped`. `failed` with no instance is the
   * other, and only once the component itself is gone (`not-installed` /
   * `unavailable`) — a reinstall then reports `installed`. "No instance" means the
   * manager no longer holds an owner: a crash whose tree cleanup is still running, or
   * whose stop failed, still has one and keeps `failed`.
   *
   * An upgrade (`downloading` / `verifying` / `paused`, or the installer's
   * own `failed`) does not drop `failed`. The crash's status event reports
   * through this method while the install is still in progress; clearing it
   * there would make the following `installed` snapshot say `installed`.
   * `starting` / `ready` / `degraded` / `stopping` are a live run, so they
   * stay. A clean stop while the component stays installed keeps `stopped`.
   * Does not emit: the snapshot that calls this is already the status event.
   */
  headlessPhaseForReport(installState: string): SidecarState | null {
    if (installState !== 'installed') {
      const phase = this.headlessSidecarState()
      const componentGone = installState === 'not-installed' || installState === 'unavailable'
      const leftover = phase === 'stopped'
        || (phase === 'failed' && this.manager.status().instanceId === undefined && componentGone)
      if (leftover) {
        this.launchedSinceClear = false
        this.preflightFailed = false
        return null
      }
    }
    return this.headlessSidecarState()
  }

  private emitStatus(): void {
    if (this.statusListeners.size === 0) return
    const snapshot = this.status()
    for (const listener of this.statusListeners) {
      try { listener(snapshot) } catch (error) {
        console.error('[creative-serve-web] status listener failed', error)
      }
    }
  }

  /**
   * Manager transitions drive the headless status. While `start()` is in flight its result
   * sets the status; here we only handle what happens after: degraded/recovered health and a
   * crash. A crash reports `stopped` (as before); the phase reports `failed`.
   */
  private onManagerStatus(): void {
    if (!this.launching && !this.stopping) {
      const { state } = this.manager.status()
      const endpoint = this.handoff.current()
      if ((state === 'ready' || state === 'healthy' || state === 'degraded') && endpoint) {
        this.storedStatus = {
          state: 'ready',
          origin: endpoint.endpoint.origin,
          instanceId: endpoint.instanceId,
          ...(state === 'degraded' ? { degraded: true } : {}),
        }
      } else if (state === 'failed' && this.storedStatus.state === 'ready') {
        this.storedStatus = { state: 'stopped' }
      }
    }
    this.emitStatus()
  }

  constructor(
    private readonly options: CreativeServeWebOptions = {},
    private readonly spawnFn: SpawnFn = spawn,
    private readonly fetchFn: FetchFn = fetch,
    deps: CreativeServeWebDeps = {},
  ) {
    const logger = deps.logger ?? consoleSidecarLogger
    const timings: OpenDesignSidecarTimings = {
      readyPhaseTimeoutMs: options.readyTimeoutMs ?? OPEN_DESIGN_READY_PHASE_TIMEOUT_MS,
      ...options.sidecarTimings,
    }
    this.manager = new SidecarManager(
      openDesignSidecarSpec(timings),
      new OpenDesignLauncher(() => {
        if (!this.launchPlan) throw new Error('Open Design launch plan is unavailable')
        return this.launchPlan
      }, spawnFn, fetchFn, logger, deps.platform, deps.kill),
      new OpenDesignHealth(fetchFn),
      exactTreeStopper,
      this.handoff,
      logger,
    )
    this.manager.onStatus(() => this.onManagerStatus())
  }

  status(): CreativeServeWebStatus {
    return { ...this.current }
  }

  start(): Promise<CreativeServeWebStatus> {
    // After `shutdown()` nothing may launch again, including restarts queued behind a stop.
    if (this.closed) return Promise.resolve(this.status())
    // A show waiting on a stop yields to any explicit `stop()` issued after it, and does not
    // retry a stop that left the run degraded.
    if (this.stopping) return this.stopping.then(this.afterStop(this.explicitStops))
    if (this.starting) return this.starting
    if (this.child) return Promise.resolve(this.status())
    const live = this.manager.status().state
    // A show lazily replaces a degraded run through the normal, deduplicated stop flow. An
    // explicit `stop()` meanwhile cancels the replacement, and a stop that leaves the run
    // degraded is not retried.
    if (live === 'degraded') return this.stopOnceShared().then(this.afterStop(this.explicitStops))
    if (live === 'ready' || live === 'healthy') return Promise.resolve(this.status())

    const pending = this.startOnce()
    this.starting = pending
    pending.then(
      () => { if (this.starting === pending) this.starting = null },
      () => { if (this.starting === pending) this.starting = null },
    )
    return pending
  }

  private async startOnce(): Promise<CreativeServeWebStatus> {
    const environment = this.options.environment ?? process.env
    const explicitCheckout = this.options.checkoutDir ?? environment.A24_OPEN_DESIGN_DIR?.trim()
    // Legacy path: a build that still bundles the Open Design trees directly
    // under resourcesPath (kept for back-compat / tests that pass a fully
    // staged resourcesPath without a component accessor).
    const headlessEntry = explicitCheckout
      ? null
      : resolveAgent24HeadlessLauncher(this.options.resourcesPath)
    if (headlessEntry) return this.startPackagedHeadless(headlessEntry, environment, this.options.resourcesPath)

    // On-demand path (owner decision 2026-10-05): the installer owns whether
    // the component is present; this file never downloads anything itself.
    //
    // PR #670 review (H1): `unavailable` means the installer has no baked
    // manifest at all (a dev/unreleased build) — it does NOT mean "no Open
    // Design available". Such a build must still fall through to the
    // original resolveOpenDesignCheckout sibling-checkout discovery below
    // (dev convenience, no env var required). Only `not-installed` /
    // `downloading` / `verifying` / `failed` — i.e. a real baked manifest
    // that isn't installed yet — short-circuits to `needs-download` here.
    if (!explicitCheckout && this.options.component) {
      const componentStatus = this.options.component.status()
      if (componentStatus.state === 'installed' && componentStatus.dir) {
        const installedHeadlessEntry = resolveAgent24HeadlessLauncher(componentStatus.dir)
        if (installedHeadlessEntry) {
          // C1 (Opus review of #670, 2026-10-05): a symlink written at
          // runtime into the app's own resourcesPath used to stand in for
          // this — EROFS on a read-only AppImage FUSE mount, EACCES on a
          // root-owned deb /opt install, breaks a signed mac bundle, and
          // races across multiple users on the same machine. The real fix
          // is fork-side (iDoris-ai/open-design-agent24#7, not yet merged):
          // agent24-headless.cjs now accepts an explicit resourceSafeBase
          // instead of always deriving it from runtimeExecutable's own
          // location. We never touch resourcesPath for the on-demand case
          // at all now — resourceSafeBase is just componentStatus.dir's own
          // realpath (it must equal its own realpath: the fork's new check
          // rejects a resourceSafeBase that is itself reached through a
          // symlink, by design — see OpenDesignComponentInstaller, which
          // installs with mode 0700 so this directory's ownership/
          // writability already satisfies the fork's POSIX check).
          const resourceSafeBase = fs.realpathSync(componentStatus.dir)
          // M1/M4 ("self-checking is not a boundary", Opus re-review of the
          // fork PR, 2026-10-06): verify BEFORE spawning anything, not just
          // trust the fork's own (later, post-spawn) self-check — the
          // ancestor-chain ownership/mode walk (M1) and the installed
          // entry files' content hashes against the marker recorded at
          // install time (M4). Both throw with a clear message on failure;
          // caught here and surfaced as a normal 'failed' status rather
          // than an uncaught exception, same as every other failure path
          // in this method.
          try {
            assertResourceSafeBaseAncestryIsSafe(resourceSafeBase)
            verifyEntryFileIntegrity(componentStatus.dir)
          } catch (error) {
            this.preflightFailed = true
            this.current = { state: 'failed', error: error instanceof Error ? error.message : String(error) }
            return this.status()
          }
          return this.startPackagedHeadless(installedHeadlessEntry, environment, componentStatus.dir, resourceSafeBase)
        }
        // Installed-but-missing-launcher (e.g. the dir was deleted after the
        // installer last validated it): surface needs-download so the
        // renderer's existing "下载" button doubles as "重新下载", same as
        // the not-installed/failed branch below. No headless instance is in a run.
        this.launchedSinceClear = false
        this.preflightFailed = false
        this.current = { state: 'needs-download', size: undefined }
        return this.status()
      }
      if (componentStatus.state !== 'unavailable') {
        this.launchedSinceClear = false
        this.preflightFailed = false
        this.current = { state: 'needs-download', size: componentStatus.size }
        return this.status()
      }
    }

    const checkoutDir = explicitCheckout
      ?? resolveOpenDesignCheckout(process.cwd(), environment, this.options.resourcesPath)
    if (!checkoutDir) {
      this.current = {
        state: 'failed',
        error: 'Open Design runtime not found; set A24_OPEN_DESIGN_DIR or package resources/open-design',
        notInstalled: true,
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

    const envPort = Number(environment.A24_OPEN_DESIGN_PORT)
    const port = this.options.port ?? (Number.isInteger(envPort) && envPort > 0 ? envPort : DEFAULT_PORT)
    const nodeBinary = this.options.nodeBinary ?? (environment.A24_OPEN_DESIGN_NODE?.trim() || 'node')
    const readyTimeoutMs = this.options.readyTimeoutMs ?? DEFAULT_READY_TIMEOUT_MS
    // A dev checkout is not the packaged headless sidecar.
    this.launchedSinceClear = false
    this.preflightFailed = false
    this.current = { state: 'starting' }

    const child = this.spawnFn(
      nodeBinary,
      [entry, 'daemon', 'start', '--serve-web', '--no-open', '--host', '127.0.0.1', '--port', String(port)],
      { cwd: checkoutDir, env: creativeChildEnvironment(environment), stdio: ['ignore', 'pipe', 'pipe'] },
    ) as CreativeChild
    this.child = child

    try {
      const origin = await this.waitForOrigin(child, readyTimeoutMs, port)
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
      if (this.child !== child) return this.status()
      const stopped = await this.terminateOwnedChild(child)
      if (this.child !== child) return this.status()
      if (stopped) {
        this.child = null
      }
      this.current = {
        state: 'failed',
        error: error instanceof Error ? error.message : String(error),
      }
      return this.status()
    }
  }

  private async startPackagedHeadless(
    headlessEntry: string,
    environment: NodeJS.ProcessEnv,
    baseResourcesPath: string | undefined,
    /** v2-only (see the on-demand branch above): an explicit, pre-realpath'd
     * trust root for resourceRoot. Omitted for the legacy bundled-resources
     * path, which keeps relying on the fork's v1 runtimeExecutable-derived
     * default. */
    resourceSafeBase?: string,
  ): Promise<CreativeServeWebStatus> {
    const resourcesPath = baseResourcesPath
    const stateRoot = this.options.stateRoot
    const runtimeExecutable = this.options.runtimeExecutable
    const pinVersion = this.options.pinVersion ?? OPEN_DESIGN_PIN_VERSION
    const preflightFailure = (error: string): CreativeServeWebStatus => {
      this.preflightFailed = true
      this.current = { state: 'failed', error }
      return this.status()
    }
    if (!resourcesPath || !path.isAbsolute(resourcesPath)) {
      return preflightFailure('Open Design packaged resources path is unavailable')
    }
    if (!stateRoot || !path.isAbsolute(stateRoot)) {
      return preflightFailure('Open Design packaged state root is unavailable')
    }
    if (!runtimeExecutable || !path.isAbsolute(runtimeExecutable)) {
      return preflightFailure('Open Design packaged runtime executable is unavailable')
    }

    // M2 closed-loop finding (2026-10-05): resourceRoot must be built from
    // the SAME base as resourceSafeBase when one is given. The fork's own
    // v2 check (isUnderSafeBase) does a plain path.relative(safeBase,
    // resourceRoot) string comparison — it never realpath()s resourceRoot
    // itself, only resourceSafeBase. Building resourceRoot from the raw,
    // un-realpath'd resourcesPath while resourceSafeBase is already
    // fs.realpathSync'd (see the on-demand branch above) means the two
    // would silently disagree on ANY system where the installed
    // component's path traverses a symlink (observed locally on macOS's
    // /var/folders tmp mount; a real ~/.agent24/... path is not usually
    // symlinked, but nothing guarantees that on every host) — the real
    // headless launcher would then reject every on-demand launch with
    // "resourceRoot must be under resourceSafeBase". Anchoring both to the
    // identical resolved base removes the mismatch unconditionally.
    const resourceRoot = path.join(resourceSafeBase ?? resourcesPath, 'open-design')
    const dataRoot = path.join(stateRoot, 'data')
    const runtimeRoot = path.join(stateRoot, 'runtime')
    this.launchPlan = {
      headlessEntry,
      resourcesPath,
      runtimeExecutable,
      configPath: path.join(stateRoot, 'agent24-headless.json'),
      config: {
        protocol: AGENT24_HEADLESS_PROTOCOL,
        pinVersion,
        resourceRoot,
        dataRoot,
        runtimeRoot,
        runtimeExecutable,
        ...(resourceSafeBase ? { resourceSafeBase } : {}),
      },
      dataRoot,
      runtimeRoot,
      pinVersion,
      environment,
      // The manager's own readiness bound is 2x this; keep the launcher's phases inside it.
      agent24Cli: this.options.agent24Cli !== undefined ? this.options.agent24Cli : resolveBundledAgent24Cli(),
      readyPhaseTimeoutMs: this.options.readyTimeoutMs ?? OPEN_DESIGN_READY_PHASE_TIMEOUT_MS,
    }

    this.preflightFailed = false
    this.launchedSinceClear = true
    // Silent assignment: `manager.start()` emits the `starting` transition right away, after
    // which both the status and the derived phase read `starting`.
    this.storedStatus = { state: 'starting' }
    this.launching = true
    const generation = ++this.startGeneration
    try {
      const result = await this.manager.start()
      if (generation !== this.startGeneration) return this.status()
      if (result.state === 'stopped') return this.status() // superseded by stop()
      if (result.state !== 'ready' && result.state !== 'healthy' && result.state !== 'degraded') {
        // start() declines while an earlier instance is still owned (its stop failed).
        this.current = {
          state: 'failed',
          error: 'Open Design previous instance has not stopped; it will not be replaced while it may still run',
        }
        return this.status()
      }
      const endpoint = this.handoff.current()
      if (!endpoint) throw new Error('Open Design launch was superseded')
      this.storedStatus = { state: 'ready', origin: endpoint.endpoint.origin, instanceId: endpoint.instanceId }
      this.emitStatus()
      return this.status()
    } catch (error) {
      if (generation !== this.startGeneration) return this.status()
      // The manager already reaped (or retained and reported) the tree before rethrowing.
      this.current = { state: 'failed', error: error instanceof Error ? error.message : String(error) }
      return this.status()
    } finally {
      if (generation === this.startGeneration) this.launching = false
    }
  }

  private afterStop(stops: number): () => Promise<CreativeServeWebStatus> {
    return () => (stops !== this.explicitStops || this.manager.status().state === 'degraded'
      ? Promise.resolve(this.status())
      : this.start())
  }

  /** Stops for good: used by app quit, so a later or queued `start()` cannot relaunch. */
  shutdown(): Promise<CreativeServeWebStatus> {
    this.closed = true
    return this.stop()
  }

  async stop(): Promise<CreativeServeWebStatus> {
    this.explicitStops += 1
    return this.stopOnceShared()
  }

  private stopOnceShared(): Promise<CreativeServeWebStatus> {
    if (this.stopping) return this.stopping
    const pending = this.stopOnce()
    this.stopping = pending
    pending.then(
      () => { if (this.stopping === pending) this.stopping = null },
      () => { if (this.stopping === pending) this.stopping = null },
    )
    return pending
  }

  private async stopOnce(): Promise<CreativeServeWebStatus> {
    this.startGeneration += 1
    this.launching = false
    const child = this.child
    this.child = null
    // Do not deduplicate a later restart onto this generation's stale promise.
    this.starting = null
    const managed = this.manager.status()
    // `failed` with no owner is already clean; stopping it would add an unplanned failed -> stopped.
    const headless = managed.state !== 'stopped' && !(managed.state === 'failed' && managed.instanceId === undefined)
    // Start the manager's stop first: its synchronous part publishes `stopping`, so the phase
    // never reads `ready` next to a `stopped` status.
    const managerStop = headless ? this.manager.stop() : null
    this.current = { state: 'stopped' }
    if (managerStop) {
      try {
        await managerStop
      } catch (error) {
        // Reaping could not be verified. The owner stays retained in the manager, which refuses to start over it.
        this.current = {
          state: 'failed',
          error: `Open Design headless sidecar did not stop: ${error instanceof Error ? error.message : String(error)}`,
        }
      }
      return this.status()
    }
    if (!child) return this.status()
    await this.terminateOwnedChild(child)
    return this.status()
  }

  private terminateOwnedChild(child: CreativeChild): Promise<boolean> {
    return new Promise((resolve) => {
      if (child.exitCode != null || child.signalCode != null) return resolve(true)
      let settled = false
      const finish = (stopped: boolean): void => {
        if (settled) return
        settled = true
        clearTimeout(timer)
        child.off('exit', onExit)
        resolve(stopped)
      }
      const onExit = (): void => finish(true)
      const timer = setTimeout(() => {
        child.kill('SIGKILL')
        finish(true)
      }, CHECKOUT_STOP_TIMEOUT_MS)
      timer.unref?.()
      child.once('exit', onExit)
      child.kill('SIGTERM')
    })
  }

  private waitForOrigin(child: CreativeChild, timeoutMs: number, expectedPort: number): Promise<string> {
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
        if (!match?.[1]) return
        try {
          done(canonicalCreativeOrigin(match[1], expectedPort))
        } catch (error) {
          done(error instanceof Error ? error : new Error(String(error)))
        }
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
