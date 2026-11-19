// D4: a translation as a DOCX in Downloads and a PDF in the document library.
// PR-Daemon #1915 B1: nothing runs LibreOffice here. The DOCX is written in
// plain code (docx-write.ts); the documents module imports it and makes the
// PDF from it inside its own sandbox (`convert` job, office.rs), as it does
// for any Word document.

import crypto from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { docxBytes, type DocxParagraph } from './docx-write'

export interface ExportDeps {
  dataDir: () => string | null
  /** `app.getPath('downloads')`. */
  downloadsDir: () => string
  /** Import the DOCX, then have the module convert it; resolves once the PDF
   * is in the library, or with why not (`engine_unavailable`: the converter
   * is not installed). */
  importAndConvert: (docxPath: string, pdfTitle: string) => Promise<{ ok: boolean; code?: string; message?: string }>
  /** D5a: a library document's current file (the `content` op), base64. */
  fetchContent?: (documentId: string) => Promise<{ ok: true; data: string; mediaType: string } | { ok: false; code?: string; message?: string }>
}

export interface ExportResult {
  ok: boolean
  docxName?: string
  code?: string
  message?: string
}

const UNSAFE_NAME = new Set(['\\', '/', ':', '*', '?', '"', '<', '>', '|'])

/** A file name: no path or control characters, at most 80 characters. */
export function safeTitle(title: string): string {
  let cleaned = ''
  for (const ch of title) {
    const code = ch.codePointAt(0) ?? 0
    if (code <= 0x1f || code === 0x7f || UNSAFE_NAME.has(ch)) continue
    cleaned += ch
  }
  // The original's extension goes: 「合同.pdf」 is exported as 「合同-English.docx」.
  const stem = cleaned.trim().replace(/\.(pdf|docx?|xlsx?|pptx?|png|jpe?g|txt|md|html?)$/i, '')
  const trimmed = stem.slice(0, 80).trim()
  return trimmed || 'translation'
}

function langSuffix(lang: 'en' | 'zh'): string {
  return lang === 'zh' ? '中文' : 'English'
}

function uniqueName(dir: string, stem: string, ext: string): string {
  const first = `${stem}${ext}`
  if (!fs.existsSync(path.join(dir, first))) return first
  for (let n = 2; n < 1000; n++) {
    const name = `${stem} (${n})${ext}`
    if (!fs.existsSync(path.join(dir, name))) return name
  }
  return `${stem} (${Date.now()})${ext}`
}

export async function exportTranslation(
  deps: ExportDeps,
  req: { title: string; paragraphs: readonly DocxParagraph[]; lang: 'en' | 'zh' },
): Promise<ExportResult> {
  const dataDir = deps.dataDir()
  if (dataDir === null) return { ok: false, code: 'export_failed', message: 'the data directory is unknown' }
  const tmp = path.join(dataDir, 'tmp', `translate-${crypto.randomBytes(8).toString('hex')}`)
  try {
    const label = `${safeTitle(req.title)}-${langSuffix(req.lang)}`
    const docx = docxBytes(req.paragraphs)
    const downloads = deps.downloadsDir()
    fs.mkdirSync(downloads, { recursive: true })
    const docxName = uniqueName(downloads, label, '.docx')
    fs.writeFileSync(path.join(downloads, docxName), docx)
    fs.mkdirSync(tmp, { recursive: true })
    const importPath = path.join(tmp, `${label}.docx`)
    fs.writeFileSync(importPath, docx)
    const done = await deps.importAndConvert(importPath, `${label}.pdf`)
    if (!done.ok) {
      const saved = `Word 已保存为下载/${docxName}`
      return { ok: false, code: done.code ?? 'import_failed', docxName, message: done.message ? `${done.message}（${saved}）` : saved }
    }
    return { ok: true, docxName }
  } catch (error) {
    return { ok: false, code: 'export_failed', message: error instanceof Error ? error.message : String(error) }
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true })
  }
}

const EXT_OF: Record<string, string> = {
  'application/vnd.openxmlformats-officedocument.wordprocessingml.document': '.docx',
  'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet': '.xlsx',
  'application/vnd.openxmlformats-officedocument.presentationml.presentation': '.pptx',
  'application/pdf': '.pdf',
  'image/png': '.png',
  'image/jpeg': '.jpg',
}

/** D5a: a library document (a filled template, a patched DOCX) saved as a
 * file of the Downloads folder, under its title; a name already there gets
 * 「 (2)」. */
export async function saveToDownloads(deps: ExportDeps, documentId: string, title: string): Promise<ExportResult> {
  if (!deps.fetchContent) return { ok: false, code: 'export_failed', message: 'saving is not available' }
  const got = await deps.fetchContent(documentId)
  if (!got.ok) return { ok: false, code: got.code ?? 'export_failed', message: got.message ?? 'the file could not be read' }
  try {
    const downloads = deps.downloadsDir()
    fs.mkdirSync(downloads, { recursive: true })
    const stem = safeTitle(title)
    const name = uniqueName(downloads, stem === 'translation' ? 'document' : stem, EXT_OF[got.mediaType] ?? '')
    fs.writeFileSync(path.join(downloads, name), Buffer.from(got.data, 'base64'))
    return { ok: true, docxName: name }
  } catch (e) {
    return { ok: false, code: 'export_failed', message: e instanceof Error ? e.message : String(e) }
  }
}
