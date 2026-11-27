import { ipcMain } from 'electron'
import { IpcChannels, type DeliverRequest, type DeliverResult } from '../../shared/ipc-types'
import { deliverRequestOf } from '../doc-deliver'
import type { IpcGuard } from './guard'

/** L8 DOC-4: `documents:deliver`, from the delivery card's button. */
export function registerDocDeliver(options: { guard: IpcGuard; deliver: (req: DeliverRequest) => Promise<DeliverResult> }): void {
  ipcMain.handle(IpcChannels.DocumentsDeliver, options.guard(async (_event, ...args: unknown[]) => {
    const req = args.length === 1 ? deliverRequestOf(args[0]) : null
    if (!req) throw new Error('documents:deliver takes { documentId, revision, format, title }')
    return options.deliver(req)
  }))
}
