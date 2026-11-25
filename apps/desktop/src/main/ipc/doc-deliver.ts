import { ipcMain } from 'electron'
import { IpcChannels, type DeliverRequest, type DeliverResult } from '../../shared/ipc-types'
import type { IpcGuard } from './guard'

const DOCUMENT_ID = /^doc_[0-7][0-9A-HJKMNP-TV-Z]{25}$/

/** One `{ documentId, revision, format, title }`, or null. */
export function deliverRequestOf(raw: unknown): DeliverRequest | null {
  if (!raw || typeof raw !== 'object') return null
  const r = raw as Record<string, unknown>
  if (Object.keys(r).some((k) => !['documentId', 'revision', 'format', 'title'].includes(k))) return null
  if (typeof r.documentId !== 'string' || !DOCUMENT_ID.test(r.documentId)) return null
  if (typeof r.revision !== 'number' || !Number.isSafeInteger(r.revision) || r.revision < 1) return null
  if (r.format !== 'docx' && r.format !== 'pdf') return null
  if (typeof r.title !== 'string' || r.title.length > 500) return null
  return { documentId: r.documentId, revision: r.revision, format: r.format, title: r.title }
}

/** L8 DOC-4: `documents:deliver`, from the delivery card's button. */
export function registerDocDeliver(options: { guard: IpcGuard; deliver: (req: DeliverRequest) => Promise<DeliverResult> }): void {
  ipcMain.handle(IpcChannels.DocumentsDeliver, options.guard(async (_event, ...args: unknown[]) => {
    const req = args.length === 1 ? deliverRequestOf(args[0]) : null
    if (!req) throw new Error('documents:deliver takes { documentId, revision, format, title }')
    return options.deliver(req)
  }))
}
