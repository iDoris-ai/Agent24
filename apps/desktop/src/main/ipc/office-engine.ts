import { ipcMain, type BrowserWindow } from 'electron'
import { IpcChannels, type OfficeEngineStatus } from '../../shared/ipc-types'
import type { OfficeEngine } from '../office-engine'
import type { IpcGuard } from './guard'

/** D1: whether LibreOffice is installed, and the download that puts it there. */
export function registerOfficeEngine(options: {
  guard: IpcGuard
  engine: OfficeEngine
  windows: () => readonly BrowserWindow[]
}): void {
  ipcMain.handle(IpcChannels.DocumentsOfficeEngineStatus, options.guard((_event, ...args: unknown[]) => {
    if (args.length) throw new Error('documents:office-engine-status takes no arguments')
    return options.engine.status()
  }))
  ipcMain.handle(IpcChannels.DocumentsOfficeEngineInstall, options.guard((_event, ...args: unknown[]) => {
    if (args.length) throw new Error('documents:office-engine-install takes no arguments')
    return options.engine.install()
  }))
  options.engine.onChange((status: OfficeEngineStatus) => {
    for (const win of options.windows()) {
      if (!win.isDestroyed()) win.webContents.send(IpcChannels.DocumentsOfficeEngineChanged, status)
    }
  })
}
