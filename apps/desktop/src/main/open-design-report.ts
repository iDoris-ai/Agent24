// ADR-038 C3b: the desktop owns Open Design, so main reports its state to the
// daemon. The renderer never sees this path or the ProductHost credential.

import http from 'node:http'
import type { DaemonConnection } from './backend-manager'
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
   * Sidecar lifecycle of the creative view's headless instance.
   * Null when that instance is not in a run (never started, or not headless).
   * Ignored unless the component itself is `installed`.
   */
  sidecar?: SidecarState | null
}

/** No control characters, at most 256 chars — the daemon rejects anything else. */
export function sanitizeReportReason(reason: string | undefined): string | undefined {
  if (!reason) return undefined
  // eslint-disable-next-line no-control-regex
  const cleaned = reason.replace(/[\u0000-\u001f\u007f-\u009f]/g, '').slice(0, 256)
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

/** POST the report with main's daemon credential. Resolves with the HTTP status; network errors reject. */
export function postOpenDesignReport(connection: DaemonConnection, body: OpenDesignReport): Promise<number> {
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
    req.on('error', reject)
    req.on('timeout', () => req.destroy(new Error('open-design report timed out')))
    req.end(payload)
  })
}

export interface OpenDesignReporterDeps {
  snapshot: () => OpenDesignReport
  connection: () => DaemonConnection | null
  send: (connection: DaemonConnection, body: OpenDesignReport) => Promise<number>
  log?: (message: string) => void
}

/**
 * Reports on start and on every mapped-state change, and repeats the current
 * state every {@link OPEN_DESIGN_HEARTBEAT_MS}. A failed attempt is logged and
 * retried on the next change or heartbeat. `stop` clears the timer.
 */
export class OpenDesignStatusReporter {
  private timer: ReturnType<typeof setInterval> | null = null
  private stopped = true
  private lastPayload = ''
  private lastOk = false
  private chain: Promise<void> = Promise.resolve()
  private readonly log: (message: string) => void

  constructor(private readonly deps: OpenDesignReporterDeps) {
    this.log = deps.log ?? ((message) => console.warn(`[open-design-report] ${message}`))
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

  stop(): void {
    this.stopped = true
    if (this.timer) {
      clearInterval(this.timer)
      this.timer = null
    }
  }

  private enqueue(body: OpenDesignReport, force: boolean): void {
    const payload = JSON.stringify(body)
    this.chain = this.chain.then(() => this.send(body, payload, force)).catch((error: unknown) => {
      this.lastOk = false
      this.log(`open-design report failed: ${error instanceof Error ? error.message : String(error)}`)
    })
  }

  private async send(body: OpenDesignReport, payload: string, force: boolean): Promise<void> {
    if (this.stopped) return
    if (!force && payload === this.lastPayload && this.lastOk) return
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
    if (status === 204) {
      this.lastPayload = payload
      this.lastOk = true
      return
    }
    this.lastOk = false
    this.log(`open-design report failed: HTTP ${status}`)
  }
}
