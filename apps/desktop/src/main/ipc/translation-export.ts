import path from 'node:path'
import { ipcMain, shell } from 'electron'
import { IpcChannels, type TranslationExportResult } from '../../shared/ipc-types'
import { exportTranslation, type ExportDeps } from '../translation-export'
import type { IpcGuard } from './guard'

const MAX_TITLE = 500
const MAX_PARAGRAPHS = 20_000
const MAX_TEXT_BYTES = 5 * 1024 * 1024

function invalid(message: string): TranslationExportResult {
  return { ok: false, code: 'invalid_request', message }
}

type Paragraph = { text: string; pageBreakBefore: boolean }

/** One `{ title, paragraphs, lang }` object, or a result that says why it was refused. */
function requestOf(raw: unknown): { title: string; paragraphs: Paragraph[]; lang: 'en' | 'zh' } | TranslationExportResult {
  if (!raw || typeof raw !== 'object') return invalid('title, paragraphs and lang are required')
  const rec = raw as Record<string, unknown>
  if (typeof rec.title !== 'string' || !Array.isArray(rec.paragraphs)) return invalid('title, paragraphs and lang are required')
  if (rec.lang !== 'en' && rec.lang !== 'zh') return invalid('lang must be en or zh')
  if (rec.title.length > MAX_TITLE) return invalid('title is longer than 500 characters')
  if (rec.paragraphs.length > MAX_PARAGRAPHS) return invalid('too many paragraphs')
  const paragraphs: Paragraph[] = []
  let bytes = 0
  for (const p of rec.paragraphs as unknown[]) {
    const q = p as { text?: unknown; pageBreakBefore?: unknown } | null
    if (!q || typeof q.text !== 'string' || typeof q.pageBreakBefore !== 'boolean') return invalid('each paragraph is { text, pageBreakBefore }')
    bytes += Buffer.byteLength(q.text)
    if (bytes > MAX_TEXT_BYTES) return invalid('the text is larger than 5 MB')
    paragraphs.push({ text: q.text, pageBreakBefore: q.pageBreakBefore })
  }
  return { title: rec.title, paragraphs, lang: rec.lang }
}

/** D4: a translation's paragraphs → DOCX in Downloads and PDF in the document library. */
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
