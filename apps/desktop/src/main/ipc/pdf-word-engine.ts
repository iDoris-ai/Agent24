import { ipcMain, type BrowserWindow } from 'electron'
import { IpcChannels, type OfficeEngineStatus } from '../../shared/ipc-types'
import type { PdfWordEngine } from '../pdf-word-engine'
import type { IpcGuard } from './guard'

/** L12-3: whether the PDF → Word engine is installed, and the download that puts it there. */
export function registerPdfWordEngine(options: {
  guard: IpcGuard
  engine: PdfWordEngine
  windows: () => readonly BrowserWindow[]
}): void {
  ipcMain.handle(IpcChannels.DocumentsPdfWordEngineStatus, options.guard((_event, ...args: unknown[]) => {
    if (args.length) throw new Error('documents:pdf-word-engine-status takes no arguments')
    return options.engine.status()
  }))
  ipcMain.handle(IpcChannels.DocumentsPdfWordEngineInstall, options.guard((_event, ...args: unknown[]) => {
    if (args.length) throw new Error('documents:pdf-word-engine-install takes no arguments')
    return options.engine.install()
  }))
  options.engine.onChange((status: OfficeEngineStatus) => {
    for (const win of options.windows()) {
      if (!win.isDestroyed()) win.webContents.send(IpcChannels.DocumentsPdfWordEngineChanged, status)
    }
  })
}
