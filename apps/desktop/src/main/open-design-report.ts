// ADR-038 C3b: the desktop owns Open Design, so main reports its state to the
// daemon. The renderer never sees this path or the ProductHost credential.

import http from 'node:http'
import type { DaemonAuthMode, DaemonConnection } from './backend-manager'
import type { OpenDesignComponentState } from './open-design-component'
import type { SidecarState } from './sidecar-contract'
import type { ComponentState } from '../shared/component-state'

/** Same state again on this interval is the heartbeat. The daemon forgets a report after 90s. */
export const OPEN_DESIGN_HEARTBEAT_MS = 30_000
export const OPEN_DESIGN_REPORT_PATH = '/api/v1/components/open-design/report'

const VERSION_PATTERN = /^[A-Za-z0-9._+-]{1,64}$/

export interface OpenDesignReport {
  state: ComponentState
  version?: string
  reason?: string
}

export interface OpenDesignReportInput {
  installState: OpenDesignComponentState
  /** Installer `reason` (unavailable) or `error` (failed). */
  reason?: string
  version?: string | null
  /**
   * Sidecar lifecycle from `CreativeServeWeb.headlessPhaseForReport()`, derived
   * from the `SidecarManager` state that hosts the packaged instance (C8).
   * `healthy` reports as `ready`; `degraded` is reported as such.
   * Null before the first packaged headless launch, for a dev checkout,
   * and when start bails before a headless process exists. A leftover
   * `stopped` is cleared once the component leaves `installed`. A `failed`
   * whose instance the manager no longer owns is cleared only when the component itself
   * is gone, so a crash during an upgrade is still `failed` when the
   * install returns. A live phase stays through a download. A clean stop
   * while the component stays installed leaves this `stopped`.
   * Ignored unless the component itself is `installed`.
   */
  sidecar?: SidecarState | null
}

/** The daemon caps `reason` at 256 UTF-8 bytes (`component_routes.rs`), not characters. */
export const REPORT_REASON_MAX_BYTES = 256

/** No control characters, at most 256 UTF-8 bytes, never cut inside a code point. */
export function sanitizeReportReason(reason: string | undefined): string | undefined {
  if (!reason) return undefined
  // eslint-disable-next-line no-control-regex
  const stripped = reason.replace(/[\u0000-\u001f\u007f-\u009f]/g, '')
  let cleaned = ''
  let bytes = 0
  for (const codePoint of stripped) {
    const size = Buffer.byteLength(codePoint, 'utf8')
    if (bytes + size > REPORT_REASON_MAX_BYTES) break
    cleaned += codePoint
    bytes += size
  }
  return cleaned.length > 0 ? cleaned : undefined
}

function cleanVersion(version: string | null | undefined): string | undefined {
  if (!version || !VERSION_PATTERN.test(version)) return undefined
  return version
}

/** `SidecarState` (`sidecar-contract.ts`): the same names, and `healthy` is `ready`. */
export function mapSidecarState(state: SidecarState): ComponentState {
  switch (state) {
    case 'healthy': return 'ready'
    case 'stopped':
    case 'starting':
    case 'ready':
    case 'degraded':
    case 'stopping':
    case 'failed':
      return state
    default: {
      const never: never = state
      return never
    }
  }
}

/**
 * `OpenDesignComponentState` (`open-design-component.ts`).
 * `downloading`, `verifying` and `paused` are all `installing`.
 * `unavailable` is `absent` and always carries a reason.
 */
export function mapInstallState(state: OpenDesignComponentState, reason?: string): OpenDesignReport {
  switch (state) {
    case 'not-installed':
      return { state: 'absent' }
    case 'unavailable':
      return { state: 'absent', reason: sanitizeReportReason(reason) ?? 'unavailable' }
    case 'downloading':
    case 'verifying':
    case 'paused':
      return { state: 'installing' }
    case 'installed':
      return { state: 'installed' }
    case 'failed': {
      const failedReason = sanitizeReportReason(reason)
      return failedReason ? { state: 'failed', reason: failedReason } : { state: 'failed' }
    }
    default: {
      const never: never = state
      return never
    }
  }
}

/** Install mapping, replaced by the sidecar mapping once the installed headless instance is up. */
export function mapOpenDesignReport(input: OpenDesignReportInput): OpenDesignReport {
  const sidecar = input.installState === 'installed' ? input.sidecar ?? null : null
  const mapped = sidecar ? { state: mapSidecarState(sidecar) } : mapInstallState(input.installState, input.reason)
  const version = mapped.state === 'absent' || mapped.state === 'installing'
    ? undefined
    : cleanVersion(input.version)
  const report: OpenDesignReport = { state: mapped.state }
  if (version) report.version = version
  if (mapped.reason) report.reason = mapped.reason
  return report
}

/** Whole-request deadline. `timeout` below only covers an idle socket, so a trickling response would never end. */
export const REPORT_DEADLINE_MS = 10_000

/** POST the report with main's daemon credential. Resolves with the HTTP status; network errors reject. */
export function postOpenDesignReport(
  connection: DaemonConnection,
  body: OpenDesignReport,
  deadlineMs: number = REPORT_DEADLINE_MS,
): Promise<number> {
  const payload = JSON.stringify(body)
  return new Promise((resolve, reject) => {
    const req = http.request({
      host: '127.0.0.1',
      port: connection.port,
      method: 'POST',
      path: OPEN_DESIGN_REPORT_PATH,
      headers: {
        ...connection.authHeaders,
        'Content-Type': 'application/json',
        'Content-Length': Buffer.byteLength(payload),
      },
      timeout: 5_000,
    }, (res) => {
      res.resume()
      res.on('error', () => reject(new Error('open-design report response failed')))
      res.on('end', () => resolve(res.statusCode ?? 0))
    })
    const deadline = setTimeout(() => req.destroy(new Error('open-design report missed its deadline')), deadlineMs)
    deadline.unref?.()
    req.on('close', () => clearTimeout(deadline))
    req.on('error', reject)
    req.on('timeout', () => req.destroy(new Error('open-design report timed out')))
    req.end(payload)
  })
}

export interface OpenDesignReporterDeps {
  snapshot: () => OpenDesignReport
  connection: () => DaemonConnection | null
  /**
   * Auth mode of the daemon `connection()` would reach. Null while that
   * daemon is not up. Anything other than `capabilities` is not reported.
   */
  authMode?: () => DaemonAuthMode | null
  send: (connection: DaemonConnection, body: OpenDesignReport) => Promise<number>
  log?: (message: string) => void
  /** Legacy-mode skip. Defaults to console.info. Failures stay on `log`. */
  info?: (message: string) => void
}

/**
 * Reports on start, on every mapped-state change, and again every
 * {@link OPEN_DESIGN_HEARTBEAT_MS}. `reportNow` sends even when the payload
 * matches the last success; backend ready uses it so a new daemon is not
 * stuck until the next heartbeat. A daemon that is not in capabilities mode
 * is not reported (one info line). HTTP 403/404 stops reporting until the
 * next `reportNow` (one warn). Other failures are logged and retried on the
 * next change or heartbeat. `stop` clears the timer.
 */
export class OpenDesignStatusReporter {
  private timer: ReturnType<typeof setInterval> | null = null
  private stopped = true
  private lastPayload = ''
  private lastOk = false
  /** Quiet until the next `reportNow`: legacy mode, or a 403/404 from this daemon. */
  private blocked: 'auth' | 'route' | null = null
  private authSkipLogged = false
  /** Bumped by `reportNow` so an in-flight attempt cannot re-block the new daemon. */
  private epoch = 0
  private chain: Promise<void> = Promise.resolve()
  private readonly log: (message: string) => void
  private readonly info: (message: string) => void
  private readonly authMode: () => DaemonAuthMode | null

  constructor(private readonly deps: OpenDesignReporterDeps) {
    this.log = deps.log ?? ((message) => console.warn(`[open-design-report] ${message}`))
    this.info = deps.info ?? ((message) => console.info(`[open-design-report] ${message}`))
    this.authMode = deps.authMode ?? (() => 'capabilities')
  }

  start(): void {
    this.stop()
    this.stopped = false
    this.enqueue(this.deps.snapshot(), true)
    this.timer = setInterval(() => {
      if (!this.stopped) this.enqueue(this.deps.snapshot(), true)
    }, OPEN_DESIGN_HEARTBEAT_MS)
    this.timer.unref?.()
  }

  /** Call when the install or headless state may have changed. The body is captured now. */
  notify(): void {
    if (this.stopped) return
    this.enqueue(this.deps.snapshot(), false)
  }

  /**
   * Send the current snapshot now, even when it matches the last accepted
   * report. Backend ready calls this: that daemon has not seen the payload,
   * and a 403/404 suspension ends here.
   */
  reportNow(): void {
    if (this.stopped) return
    this.epoch += 1
    this.blocked = null
    this.enqueue(this.deps.snapshot(), true)
  }

  stop(): void {
    this.stopped = true
    if (this.timer) {
      clearInterval(this.timer)
      this.timer = null
    }
  }

  private enqueue(body: OpenDesignReport, force: boolean): void {
    const payload = JSON.stringify(body)
    const epoch = this.epoch
    this.chain = this.chain.then(() => this.send(body, payload, force, epoch)).catch((error: unknown) => {
      this.lastOk = false
      this.log(`open-design report failed: ${error instanceof Error ? error.message : String(error)}`)
    })
  }

  private async send(body: OpenDesignReport, payload: string, force: boolean, epoch: number): Promise<void> {
    if (this.stopped || epoch !== this.epoch || this.blocked) return
    if (!force && payload === this.lastPayload && this.lastOk) return
    const mode = this.authMode()
    if (mode !== null && mode !== 'capabilities') {
      this.lastOk = false
      this.blocked = 'auth'
      if (!this.authSkipLogged) {
        this.authSkipLogged = true
        this.info(`open-design report skipped: daemon auth mode is ${mode}, not capabilities`)
      }
      return
    }
    const connection = this.deps.connection()
    if (!connection) {
      this.lastOk = false
      this.log('open-design report skipped: daemon is not up')
      return
    }
    let status: number
    try {
      status = await this.deps.send(connection, body)
    } catch (error) {
      this.lastOk = false
      this.log(`open-design report failed: ${error instanceof Error ? error.message : String(error)}`)
      return
    }
    if (this.stopped || epoch !== this.epoch) return
    if (status === 204) {
      this.lastPayload = payload
      this.lastOk = true
      return
    }
    this.lastOk = false
    if (status === 403 || status === 404) {
      this.blocked = 'route'
      this.log(`open-design report stopped: HTTP ${status}`)
      return
    }
    this.log(`open-design report failed: HTTP ${status}`)
  }
}
