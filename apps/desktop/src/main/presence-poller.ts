// CORE-2 AgentEar slice S3 (docs/agent/CORE-2-AGENTEAR-SLICE.md §3.4): main's
// side of the daemon's desktop confirmation channel.
//
// Main long-polls `GET /api/v1/presence/challenges/next`, shows a native dialog
// for each challenge, and answers `POST /api/v1/presence/challenges/{id}`. The
// renderer never sees any of it (the backendProxy refuses `/presence/`). The
// text in the dialog is the daemon's own summary of the request, never text the
// requesting CLI supplied.
//
// The dialog is shown concurrently with the next poll (one dialog at a time),
// so the daemon keeps seeing a live desktop while the user is deciding.
// Pure of Electron imports: the dialog, fetch, clock and timers are injected.

import type { DaemonConnection } from './backend-manager'
import { mt } from './main-i18n'

export interface PresenceChallenge {
  challenge_id: string
  operation: string
  summary: string
  requester: { identifier: string; pid: number }
  /** Unix seconds. */
  expires_at: number
}

export interface PresenceDialogOptions {
  type: 'question'
  title: string
  message: string
  detail: string
  buttons: string[]
  defaultId: number
  cancelId: number
  noLink: true
}

interface PresenceParent {
  isDestroyed(): boolean
  isMinimized(): boolean
  restore(): void
  show(): void
  focus(): void
}

export async function showPresenceDialog<Parent extends PresenceParent>(
  parent: Parent | null,
  opts: PresenceDialogOptions,
  signal: AbortSignal,
  showBox: (parent: Parent, opts: PresenceDialogOptions & { signal: AbortSignal }) => Promise<{ response: number }>,
): Promise<{ response: number }> {
  if (!parent || parent.isDestroyed()) throw new Error('Trusted presence window unavailable')
  if (signal.aborted) throw new Error('Presence challenge expired')
  if (parent.isMinimized()) parent.restore()
  parent.show()
  parent.focus()
  return showBox(parent, { ...opts, signal })
}

export interface PresencePollerDeps {
  connection: () => DaemonConnection | null
  fetch: typeof fetch
  /** Show the native box; aborting `signal` closes it where the platform can. */
  showDialog: (opts: PresenceDialogOptions, signal: AbortSignal) => Promise<{ response: number }>
  t: typeof mt
  nowMs: () => number
  /** Resolves after `ms`, or early when `signal` aborts. */
  sleep: (ms: number, signal: AbortSignal) => Promise<void>
  /** Runs `fn` after `ms`; returns the canceller. */
  setTimer: (fn: () => void, ms: number) => () => void
  log: (message: string) => void
}

export const POLL_WAIT_SECONDS = 25
export const IDLE_MS = 2_000
export const BACKOFF_START_MS = 1_000
export const BACKOFF_MAX_MS = 30_000
/** The daemon has no presence routes (legacy mode, older daemon): check rarely. */
export const NOT_SUPPORTED_MS = 5 * 60_000
const FAST_EMPTY_POLL_MS = 200
const MAX_FIELD = 500

const PRESENCE_BASE = '/api/v1/presence/challenges'

function realSleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) return resolve()
    const done = (): void => {
      clearTimeout(timer)
      signal.removeEventListener('abort', done)
      resolve()
    }
    const timer = setTimeout(done, ms)
    signal.addEventListener('abort', done)
  })
}

export function defaultPresenceDeps(
  partial: Pick<PresencePollerDeps, 'connection' | 'showDialog'> & Partial<PresencePollerDeps>,
): PresencePollerDeps {
  return {
    fetch: (...args) => fetch(...args),
    t: mt,
    nowMs: () => Date.now(),
    sleep: realSleep,
    setTimer: (fn, ms) => {
      const handle = setTimeout(fn, ms)
      return () => clearTimeout(handle)
    },
    log: (message) => console.warn(`[presence] ${message}`),
    ...partial,
  }
}

function clip(value: string): string {
  return value.length > MAX_FIELD ? `${value.slice(0, MAX_FIELD)}…` : value
}

export function parseChallenge(raw: unknown): PresenceChallenge | null {
  if (typeof raw !== 'object' || raw === null) return null
  const c = raw as Record<string, unknown>
  const req = c.requester as Record<string, unknown> | null | undefined
  if (
    typeof c.challenge_id !== 'string' || c.challenge_id === '' ||
    typeof c.operation !== 'string' ||
    typeof c.summary !== 'string' ||
    typeof c.expires_at !== 'number' || !Number.isFinite(c.expires_at) ||
    typeof req !== 'object' || req === null ||
    typeof req.identifier !== 'string' || typeof req.pid !== 'number'
  ) {
    return null
  }
  return {
    challenge_id: c.challenge_id,
    operation: c.operation,
    summary: c.summary,
    requester: { identifier: req.identifier, pid: req.pid },
    expires_at: c.expires_at,
  }
}

export class PresencePoller {
  private readonly deps: PresencePollerDeps
  private stopped = true
  private abort: AbortController | null = null
  private loop: Promise<void> | null = null
  private dialogs: Promise<void> = Promise.resolve()

  constructor(deps: PresencePollerDeps) {
    this.deps = deps
  }

  /** Start polling. Idempotent while running. */
  start(): void {
    if (!this.stopped) return
    this.stopped = false
    const abort = new AbortController()
    this.abort = abort
    this.loop = this.run(abort.signal)
  }

  /** Stop polling and close any open dialog; resolves when the loop has ended. */
  async stop(): Promise<void> {
    this.stopped = true
    this.abort?.abort()
    await this.loop
    this.loop = null
    this.abort = null
  }

  private async run(signal: AbortSignal): Promise<void> {
    let backoff = BACKOFF_START_MS
    while (!signal.aborted) {
      const conn = this.deps.connection()
      // The node mock authenticates nothing; there is no host authority to poll as.
      if (!conn || !conn.authHeaders.Authorization) {
        await this.deps.sleep(IDLE_MS, signal)
        continue
      }
      const startedAt = this.deps.nowMs()
      try {
        const res = await this.deps.fetch(
          `http://127.0.0.1:${conn.port}${PRESENCE_BASE}/next?wait=${POLL_WAIT_SECONDS}`,
          { headers: conn.authHeaders, signal },
        )
        if (res.status === 204) {
          backoff = BACKOFF_START_MS
          // A daemon that answers an empty poll at once would turn this into a spin.
          if (this.deps.nowMs() - startedAt < FAST_EMPTY_POLL_MS) await this.deps.sleep(IDLE_MS, signal)
          continue
        }
        if (res.status === 404) {
          await this.deps.sleep(NOT_SUPPORTED_MS, signal)
          continue
        }
        if (res.status !== 200) {
          this.deps.log(`poll answered ${res.status}`)
          await this.deps.sleep(backoff, signal)
          backoff = Math.min(backoff * 2, BACKOFF_MAX_MS)
          continue
        }
        backoff = BACKOFF_START_MS
        const challenge = parseChallenge(await res.json())
        if (challenge) this.enqueue(challenge, conn)
        else this.deps.log('ignored a malformed challenge')
      } catch (err) {
        if (signal.aborted) break
        this.deps.log(`poll failed: ${(err as Error).message}`)
        await this.deps.sleep(backoff, signal)
        backoff = Math.min(backoff * 2, BACKOFF_MAX_MS)
      }
    }
  }

  /** Dialogs run one at a time, off the polling path. */
  private enqueue(challenge: PresenceChallenge, conn: DaemonConnection): void {
    this.dialogs = this.dialogs.then(() => this.ask(challenge, conn)).catch((err) => {
      this.deps.log(`dialog failed: ${(err as Error).message}`)
    })
  }

  private async ask(challenge: PresenceChallenge, conn: DaemonConnection): Promise<void> {
    const remaining = challenge.expires_at * 1000 - this.deps.nowMs()
    if (this.stopped || remaining <= 0) return

    const expiry = new AbortController()
    const cancelTimer = this.deps.setTimer(() => expiry.abort(), remaining)
    const onStop = (): void => expiry.abort()
    this.abort?.signal.addEventListener('abort', onStop)
    let response: number
    try {
      const { t } = this.deps
      const result = await this.deps.showDialog(
        {
          type: 'question',
          title: t('presence.title'),
          message: t('presence.message', {
            requester: clip(challenge.requester.identifier),
            pid: challenge.requester.pid,
            operation: clip(challenge.operation),
          }),
          detail: t('presence.detail', { summary: clip(challenge.summary) }),
          buttons: [t('presence.allow'), t('presence.deny')],
          // Deny is both the default and the cancel (Esc / closed) answer.
          defaultId: 1,
          cancelId: 1,
          noLink: true,
        },
        expiry.signal,
      )
      response = result.response
    } catch (err) {
      this.deps.log(`dialog error, denying: ${(err as Error).message}`)
      response = 1
    } finally {
      cancelTimer()
      this.abort?.signal.removeEventListener('abort', onStop)
    }
    // Expired (or shutting down) while the box was up: the answer is late.
    if (expiry.signal.aborted || this.stopped || this.deps.nowMs() >= challenge.expires_at * 1000) return
    await this.answer(challenge.challenge_id, response === 0 ? 'allow' : 'deny', conn)
  }

  private async answer(id: string, decision: 'allow' | 'deny', conn: DaemonConnection): Promise<void> {
    try {
      const res = await this.deps.fetch(
        `http://127.0.0.1:${conn.port}${PRESENCE_BASE}/${encodeURIComponent(id)}`,
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/json', ...conn.authHeaders },
          body: JSON.stringify({ decision }),
        },
      )
      if (!res.ok) this.deps.log(`decision for ${id} answered ${res.status}`)
    } catch (err) {
      this.deps.log(`decision for ${id} failed: ${(err as Error).message}`)
    }
  }
}
