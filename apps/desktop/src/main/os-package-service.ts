// Stop the LaunchAgent's daemon for `agent24 os install`, then kickstart it.
// A clean exit is not restarted by launchd (KeepAlive SuccessfulExit:false),
// so kickstart is what brings it back. The desktop looks for the new record
// because the caller holds `whileServiceRestarts`.

import { DAEMON_EXIT_WAIT_MS, processAlive, readExternalDaemonState } from './backend-manager'
import { elevatedShutdown, kickstartService, servicePid } from './resident-service'

export interface ServicePauseDeps {
  shutdown: () => Promise<{ status: number; code?: string }>
  /** True once that daemon has exited and launchd is not running the job. */
  exited: () => Promise<boolean>
  kickstart: () => Promise<void>
  /** True once a new daemon is on record. */
  started: () => Promise<boolean>
}

export type ServicePauseResult<T> =
  | { ok: true; value: T }
  | { ok: false; stage: 'shutdown' | 'exit' | 'kickstart'; code: string }

async function until(check: () => boolean | Promise<boolean>, ms: number): Promise<boolean> {
  for (const end = Date.now() + ms; ;) {
    if (await check()) return true
    if (Date.now() >= end) return false
    await new Promise((r) => setTimeout(r, 200))
  }
}

/** Runs `work` only after a clean shutdown. Kickstart runs afterwards unless
 * the shutdown itself was refused — then the service is still the old one. */
export async function runWhileServiceStopped<T>(
  deps: ServicePauseDeps,
  work: () => Promise<T>,
): Promise<ServicePauseResult<T>> {
  const down = await deps.shutdown()
  if (down.status < 200 || down.status >= 300) {
    return { ok: false, stage: 'shutdown', code: down.code ?? `http_${down.status}` }
  }
  if (!(await deps.exited())) {
    try { await deps.kickstart() } catch { /* the exit failure is the one to report */ }
    return { ok: false, stage: 'exit', code: 'still_running' }
  }
  const value = await work()
  try {
    await deps.kickstart()
  } catch {
    return { ok: false, stage: 'kickstart', code: 'kickstart_failed' }
  }
  if (!(await deps.started())) return { ok: false, stage: 'kickstart', code: 'not_started' }
  return { ok: true, value }
}

/** The real LaunchAgent steps for the enrolled service daemon `pid`. */
export function realOsServicePause(pid: number): ServicePauseDeps {
  const uid = process.getuid?.()
  if (uid === undefined) throw new Error('service pause is only available on macOS')
  return {
    shutdown: elevatedShutdown,
    exited: async () => (await until(() => !processAlive(pid), DAEMON_EXIT_WAIT_MS)) && (await servicePid(uid)) === null,
    kickstart: () => kickstartService(uid),
    started: () => until(() => {
      const state = readExternalDaemonState()
      return state !== null && state.pid !== pid && processAlive(state.pid)
    }, 30_000),
  }
}
