import path from 'node:path'
import { ipcMain, shell } from 'electron'
import { IpcChannels, type TranslationExportResult } from '../../shared/ipc-types'
import { exportTranslation, type ExportDeps } from '../translation-export'
import type { IpcGuard } from './guard'

const MAX_TITLE = 500
const MAX_HTML_BYTES = 5 * 1024 * 1024

function invalid(message: string): TranslationExportResult {
  return { ok: false, code: 'invalid_request', message }
}

/** One `{ title, html, lang }` object, or a result that says why it was refused. */
function requestOf(raw: unknown): { title: string; html: string; lang: 'en' | 'zh' } | TranslationExportResult {
  if (!raw || typeof raw !== 'object') return invalid('title, html and lang are required')
  const rec = raw as Record<string, unknown>
  if (typeof rec.title !== 'string' || typeof rec.html !== 'string') return invalid('title, html and lang are required')
  if (rec.lang !== 'en' && rec.lang !== 'zh') return invalid('lang must be en or zh')
  if (rec.title.length > MAX_TITLE) return invalid('title is longer than 500 characters')
  if (Buffer.byteLength(rec.html) > MAX_HTML_BYTES) return invalid('html is larger than 5 MB')
  return { title: rec.title, html: rec.html, lang: rec.lang }
}

/** D4: HTML of a translation → DOCX in Downloads and PDF in the document library. */
export function registerTranslationExport(options: { guard: IpcGuard; deps: ExportDeps }): void {
  // ① 「在 Finder 中显示」: a bare file name of the Downloads folder, nothing else.
  ipcMain.handle(IpcChannels.DocumentsRevealDownload, options.guard((_event, ...args: unknown[]) => {
    const name = args[0]
    if (args.length !== 1 || typeof name !== 'string' || name === '' || name !== path.basename(name) || name.startsWith('.')) {
      throw new Error('documents:reveal-download takes a file name')
    }
    shell.showItemInFolder(path.join(options.deps.downloadsDir(), name))
    return true
  }))
  ipcMain.handle(IpcChannels.DocumentsExportTranslation, options.guard(async (_event, ...args: unknown[]) => {
    if (args.length !== 1) throw new Error('documents:export-translation takes one argument')
    const req = requestOf(args[0])
    if (!('title' in req)) return req
    return exportTranslation(options.deps, req)
  }))
}
