import { spawn, type ChildProcess } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import type { Readable } from 'node:stream'

export type CreativeServeWebState = 'stopped' | 'starting' | 'ready' | 'failed' | 'needs-download'

export interface CreativeServeWebStatus {
  state: CreativeServeWebState
  origin?: string
  error?: string
  /** Set when state === 'needs-download': the Open Design component's
   * declared download size, surfaced from the baked manifest, so the
   * renderer can show "~N MB" without a round trip of its own. */
  size?: number
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
}

type SpawnFn = typeof spawn
type FetchFn = typeof fetch
type CreativeChild = ChildProcess & { stdout: Readable; stderr: Readable }

const DEFAULT_PORT = 7456
const DEFAULT_READY_TIMEOUT_MS = 15_000
const CHECKOUT_STOP_TIMEOUT_MS = 5_000
const HEADLESS_STOP_TIMEOUT_MS = 60_000
export const OPEN_DESIGN_PIN_VERSION = '0.22.2'
// v2 (Opus review of #670, 2026-10-05): the fork's agent24-headless.cjs now
// accepts an explicit resourceSafeBase (fork PR iDoris-ai/open-design-
// agent24#7, not yet merged/pinned — see the on-demand branch below for why
// this constant is bumped here ahead of that pin update). NOTE: this makes
// the on-demand path incompatible with the CURRENTLY pinned fork commit
// (still protocol v1) until the fork PR merges and OPEN_DESIGN_SHA is
// re-pinned — intentional per review instruction ("pin 暂不改...先写好代码与
// 测试"), not an oversight.
const AGENT24_HEADLESS_PROTOCOL = 2

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

function canonicalHeadlessOrigin(raw: unknown, field: string): string {
  if (typeof raw !== 'string') throw new Error(`Open Design ${field} is invalid`)
  const url = new URL(raw)
  if (url.protocol !== 'http:' || url.hostname !== '127.0.0.1' || !url.port) {
    throw new Error(`Open Design ${field} must use an exact 127.0.0.1 HTTP origin`)
  }
  if (url.username || url.password || url.pathname !== '/' || url.search || url.hash) {
    throw new Error(`Open Design ${field} must be an origin without credentials or path data`)
  }
  return url.origin
}

export interface Agent24HeadlessReady {
  type: 'ready'
  protocol: 2
  instanceId: string
  pinVersion: string
  webOrigin: string
  daemonOrigin: string
  ownership: { ownerPid: number; kind: 'process-tree' }
}

export function parseAgent24HeadlessReady(
  value: unknown,
  expectedPinVersion: string,
  expectedOwnerPid: number,
): Agent24HeadlessReady {
  if (value == null || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('Open Design headless readiness must be an object')
  }
  const raw = value as Record<string, unknown>
  const allowed = new Set(['type', 'protocol', 'instanceId', 'pinVersion', 'webOrigin', 'daemonOrigin', 'ownership'])
  const unknown = Object.keys(raw).find((key) => !allowed.has(key))
  if (unknown) throw new Error(`Open Design headless readiness contains unsupported field: ${unknown}`)
  if (raw.type !== 'ready' || raw.protocol !== AGENT24_HEADLESS_PROTOCOL) {
    throw new Error('Open Design headless readiness protocol mismatch')
  }
  if (raw.pinVersion !== expectedPinVersion) throw new Error('Open Design headless pin version mismatch')
  if (typeof raw.instanceId !== 'string' || !raw.instanceId.trim() || /\s/u.test(raw.instanceId)) {
    throw new Error('Open Design headless instance id is invalid')
  }
  if (raw.ownership == null || typeof raw.ownership !== 'object' || Array.isArray(raw.ownership)) {
    throw new Error('Open Design headless ownership is invalid')
  }
  const ownership = raw.ownership as Record<string, unknown>
  const unknownOwnership = Object.keys(ownership).find((key) => key !== 'ownerPid' && key !== 'kind')
  if (unknownOwnership) {
    throw new Error(`Open Design headless ownership contains unsupported field: ${unknownOwnership}`)
  }
  if (ownership.kind !== 'process-tree' || ownership.ownerPid !== expectedOwnerPid) {
    throw new Error('Open Design headless ownership does not match the launched process')
  }
  return {
    type: 'ready',
    protocol: AGENT24_HEADLESS_PROTOCOL,
    instanceId: raw.instanceId,
    pinVersion: expectedPinVersion,
    webOrigin: canonicalHeadlessOrigin(raw.webOrigin, 'web origin'),
    daemonOrigin: canonicalHeadlessOrigin(raw.daemonOrigin, 'daemon origin'),
    ownership: { ownerPid: expectedOwnerPid, kind: 'process-tree' },
  }
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

export function creativeChildEnvironment(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  // A24_* belongs to the Agent24 host authority/bootstrap namespace. The
  // desktop may consume A24_OPEN_DESIGN_* to choose how to launch Creative,
  // but the less-trusted Open Design child must never inherit host secrets.
  // Keep the rest of the environment intact so Open Design's own provider,
  // media, proxy, locale, and toolchain configuration continues to work.
  const childEnv = Object.create(null) as NodeJS.ProcessEnv
  for (const [key, value] of Object.entries(env)) {
    if (!key.toUpperCase().startsWith('A24_')) childEnv[key] = value
  }
  return childEnv
}

export class CreativeServeWeb {
  private child: CreativeChild | null = null
  private childKind: 'checkout' | 'headless' | null = null
  private current: CreativeServeWebStatus = { state: 'stopped' }
  private starting: Promise<CreativeServeWebStatus> | null = null
  private stopping: Promise<CreativeServeWebStatus> | null = null

  constructor(
    private readonly options: CreativeServeWebOptions = {},
    private readonly spawnFn: SpawnFn = spawn,
    private readonly fetchFn: FetchFn = fetch,
  ) {}

  status(): CreativeServeWebStatus {
    return { ...this.current }
  }

  start(): Promise<CreativeServeWebStatus> {
    if (this.stopping) return this.stopping.then(() => this.start())
    if (this.starting) return this.starting
    if (this.child) return Promise.resolve(this.status())

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
          return this.startPackagedHeadless(installedHeadlessEntry, environment, componentStatus.dir, resourceSafeBase)
        }
        // Installed-but-missing-launcher (e.g. the dir was deleted after the
        // installer last validated it): surface needs-download so the
        // renderer's existing "下载" button doubles as "重新下载", same as
        // the not-installed/failed branch below.
        this.current = { state: 'needs-download', size: undefined }
        return this.status()
      }
      if (componentStatus.state !== 'unavailable') {
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
    this.current = { state: 'starting' }

    const child = this.spawnFn(
      nodeBinary,
      [entry, 'daemon', 'start', '--serve-web', '--no-open', '--host', '127.0.0.1', '--port', String(port)],
      { cwd: checkoutDir, env: creativeChildEnvironment(environment), stdio: ['ignore', 'pipe', 'pipe'] },
    ) as CreativeChild
    this.child = child
    this.childKind = 'checkout'

    try {
      const origin = await this.waitForOrigin(child, readyTimeoutMs, port)
      await this.waitUntilReady(origin, readyTimeoutMs)
      if (this.child !== child) throw new Error('Open Design launch was superseded')
      this.current = { state: 'ready', origin }
      child.once('exit', () => {
        if (this.child !== child) return
        this.child = null
        this.childKind = null
        this.current = { state: 'stopped' }
      })
      return this.status()
    } catch (error) {
      if (this.child !== child) return this.status()
      const stopped = await this.terminateOwnedChild(child, 'checkout')
      if (this.child !== child) return this.status()
      if (stopped) {
        this.child = null
        this.childKind = null
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
    if (!resourcesPath || !path.isAbsolute(resourcesPath)) {
      this.current = { state: 'failed', error: 'Open Design packaged resources path is unavailable' }
      return this.status()
    }
    if (!stateRoot || !path.isAbsolute(stateRoot)) {
      this.current = { state: 'failed', error: 'Open Design packaged state root is unavailable' }
      return this.status()
    }
    if (!runtimeExecutable || !path.isAbsolute(runtimeExecutable)) {
      this.current = { state: 'failed', error: 'Open Design packaged runtime executable is unavailable' }
      return this.status()
    }

    const resourceRoot = path.join(resourcesPath, 'open-design')
    const dataRoot = path.join(stateRoot, 'data')
    const runtimeRoot = path.join(stateRoot, 'runtime')
    const configPath = path.join(stateRoot, 'agent24-headless.json')
    fs.mkdirSync(dataRoot, { recursive: true })
    fs.mkdirSync(runtimeRoot, { recursive: true })
    fs.writeFileSync(configPath, `${JSON.stringify({
      protocol: AGENT24_HEADLESS_PROTOCOL,
      pinVersion,
      resourceRoot,
      dataRoot,
      runtimeRoot,
      runtimeExecutable,
      ...(resourceSafeBase ? { resourceSafeBase } : {}),
    })}\n`, { encoding: 'utf8', mode: 0o600 })
    fs.chmodSync(configPath, 0o600)

    this.current = { state: 'starting' }
    const childEnv = creativeChildEnvironment(environment)
    childEnv.ELECTRON_RUN_AS_NODE = '1'
    const child = this.spawnFn(
      runtimeExecutable,
      [headlessEntry, '--config', configPath],
      { cwd: resourcesPath, env: childEnv, stdio: ['ignore', 'pipe', 'pipe'] },
    ) as CreativeChild
    this.child = child
    this.childKind = 'headless'

    try {
      if (!Number.isInteger(child.pid) || !child.pid || child.pid < 1) {
        throw new Error('Open Design headless launcher did not provide a pid')
      }
      const readyTimeoutMs = this.options.readyTimeoutMs ?? DEFAULT_READY_TIMEOUT_MS
      const ready = await this.waitForHeadlessReady(child, readyTimeoutMs, pinVersion, child.pid)
      await this.waitUntilReady(ready.webOrigin, readyTimeoutMs)
      if (this.child !== child) throw new Error('Open Design launch was superseded')
      this.current = { state: 'ready', origin: ready.webOrigin }
      child.once('exit', () => {
        if (this.child !== child) return
        this.child = null
        this.childKind = null
        this.current = { state: 'stopped' }
      })
      return this.status()
    } catch (error) {
      if (this.child !== child) return this.status()
      const stopped = await this.terminateOwnedChild(child, 'headless')
      if (this.child !== child) return this.status()
      if (stopped) {
        this.child = null
        this.childKind = null
      } else {
        this.current = {
          state: 'failed',
          error: `Open Design headless launcher did not stop within ${HEADLESS_STOP_TIMEOUT_MS}ms; refusing an owner-only force kill`,
        }
        this.observeRetainedOwner(child)
        return this.status()
      }
      this.current = {
        state: 'failed',
        error: error instanceof Error ? error.message : String(error),
      }
      return this.status()
    }
  }

  async stop(): Promise<CreativeServeWebStatus> {
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
    const child = this.child
    const kind = this.childKind ?? 'checkout'
    this.child = null
    this.childKind = null
    // Do not deduplicate a later restart onto this generation's stale promise.
    this.starting = null
    this.current = { state: 'stopped' }
    if (!child) {
      return this.status()
    }

    const stopped = await this.terminateOwnedChild(child, kind)
    if (!stopped && kind === 'headless') {
      this.child = child
      this.childKind = kind
      this.current = {
        state: 'failed',
        error: `Open Design headless launcher did not stop within ${HEADLESS_STOP_TIMEOUT_MS}ms; refusing an owner-only force kill`,
      }
      this.observeRetainedOwner(child)
    }
    return this.status()
  }

  private terminateOwnedChild(child: CreativeChild, kind: 'checkout' | 'headless'): Promise<boolean> {
    const timeoutMs = kind === 'headless' ? HEADLESS_STOP_TIMEOUT_MS : CHECKOUT_STOP_TIMEOUT_MS
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
        if (kind === 'checkout') {
          child.kill('SIGKILL')
          finish(true)
          return
        }
        finish(false)
      }, timeoutMs)
      timer.unref?.()
      child.once('exit', onExit)
      child.kill('SIGTERM')
    })
  }

  private observeRetainedOwner(child: CreativeChild): void {
    child.once('exit', () => {
      if (this.child !== child) return
      this.child = null
      this.childKind = null
      this.current = { state: 'stopped' }
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

  private waitForHeadlessReady(
    child: CreativeChild,
    timeoutMs: number,
    expectedPinVersion: string,
    expectedOwnerPid: number,
  ): Promise<Agent24HeadlessReady> {
    return new Promise((resolve, reject) => {
      let stdout = ''
      let stderr = ''
      const timeout = setTimeout(() => done(new Error('Open Design headless readiness output timed out')), timeoutMs)
      timeout.unref?.()
      const done = (value: Error | Agent24HeadlessReady): void => {
        clearTimeout(timeout)
        child.stdout.off('data', onStdout)
        child.stderr.off('data', onStderr)
        child.off('error', onError)
        child.off('exit', onExit)
        value instanceof Error ? reject(value) : resolve(value)
      }
      const inspect = (): void => {
        if (Buffer.byteLength(stdout, 'utf8') > 16_384) {
          done(new Error('Open Design headless readiness output exceeded the host limit'))
          return
        }
        const newline = stdout.indexOf('\n')
        if (newline < 0) return
        const line = stdout.slice(0, newline).trim()
        if (!line) {
          done(new Error('Open Design headless readiness line is empty'))
          return
        }
        try {
          done(parseAgent24HeadlessReady(JSON.parse(line), expectedPinVersion, expectedOwnerPid))
        } catch (error) {
          done(error instanceof Error ? error : new Error(String(error)))
        }
      }
      const onStdout = (chunk: Buffer | string): void => { stdout += chunk.toString(); inspect() }
      const onStderr = (chunk: Buffer | string): void => { stderr = (stderr + chunk.toString()).slice(-2_000) }
      const onError = (error: Error): void => done(error)
      const onExit = (code: number | null): void =>
        done(new Error(`Open Design headless launcher exited before ready (${code ?? 'signal'})${stderr ? `: ${stderr.trim()}` : ''}`))
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
