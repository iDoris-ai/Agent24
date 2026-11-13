import { ipcMain } from 'electron'
import { IpcChannels, type ResidentServiceResult, type ResidentServiceStatus } from '../../shared/ipc-types'
import type { IpcGuard } from './guard'

/** M5 B4c2: status takes no argument; set takes exactly one boolean. A set
 * while another one the same way is running gets that one's result, not a
 * second run; one the other way is refused as busy, not given a result that
 * is not its own (Codex B4c2 C2). */
export function registerResidentService(options: {
  guard: IpcGuard
  status: () => ResidentServiceStatus
  set: (on: boolean) => Promise<ResidentServiceResult>
}): void {
  ipcMain.handle(IpcChannels.ResidentServiceStatus, options.guard((_event, ...args: unknown[]) => {
    if (args.length) throw new Error('resident-service:status takes no arguments')
    return options.status()
  }))
  let pending: { on: boolean; run: Promise<ResidentServiceResult> } | null = null
  ipcMain.handle(IpcChannels.ResidentServiceSet, options.guard((_event, ...args: unknown[]) => {
    if (args.length !== 1 || typeof args[0] !== 'boolean') throw new Error('resident-service:set takes one boolean')
    const on = args[0]
    if (pending && pending.on !== on) return Promise.resolve<ResidentServiceResult>({ ok: false, stage: on ? 'install' : 'shutdown', code: 'busy' })
    if (!pending) {
      const run: Promise<ResidentServiceResult> = options.set(on).finally(() => { if (pending?.run === run) pending = null })
      pending = { on, run }
    }
    return pending.run
  }))
}
