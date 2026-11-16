import { ipcMain } from 'electron'
import { IpcChannels, type ModelSource, type ModelSourceResult } from '../../shared/ipc-types'
import { clearModelSource, normalizeModelName, normalizeModelSourceUrl, readModelSource, writeModelSource } from '../model-source'
import type { IpcGuard } from './guard'

/** Model source setting. Set and clear write `file` and then restart the
 * daemon (`restart`), which reads it as it starts. */
export function registerModelSource(options: {
  guard: IpcGuard
  file: () => string | null
  restart: () => Promise<{ ok: true } | { ok: false; code: string }>
}): void {
  const applied = async (): Promise<ModelSourceResult> => {
    const r = await options.restart().catch(() => ({ ok: false as const, code: 'restart_failed' }))
    return r.ok ? { ok: true, restart: 'restarted' } : { ok: true, restart: 'restart_failed', code: r.code }
  }
  ipcMain.handle(IpcChannels.ModelSourceGet, options.guard((): ModelSource | null => {
    const file = options.file()
    return file ? readModelSource(file) : null
  }))
  ipcMain.handle(IpcChannels.ModelSourceSet, options.guard(async (_event, ...args: unknown[]): Promise<ModelSourceResult> => {
    const url = normalizeModelSourceUrl(args[0])
    if (!url) return { ok: false, reason: 'invalid_url' }
    const model = normalizeModelName(args[1])
    if (!model) return { ok: false, reason: 'invalid_model' }
    const file = options.file()
    if (!file) return { ok: false, reason: 'no_data_dir' }
    try { writeModelSource(file, { url, model }) } catch { return { ok: false, reason: 'not_saved' } }
    return applied()
  }))
  ipcMain.handle(IpcChannels.ModelSourceClear, options.guard(async (): Promise<ModelSourceResult> => {
    const file = options.file()
    if (!file) return { ok: false, reason: 'no_data_dir' }
    try { clearModelSource(file) } catch { return { ok: false, reason: 'not_saved' } }
    return applied()
  }))
}
