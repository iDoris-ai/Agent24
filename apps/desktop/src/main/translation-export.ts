// D4: turn translated HTML into a DOCX in Downloads and a PDF in the document
// library. LibreOffice is the one the Office → PDF installer put in the data
// directory (office-engine.ts). The profile follows
// rust/apps/agent24-documents/src/office.rs: a fresh directory, user/fonts
// linked at the engine fonts, macros off. The font-replacement table is left
// out; this HTML names Noto Sans SC itself.
//
// HTML → PDF uses writer_web_pdf_Export (checked locally: a two-break HTML
// came out as a 3-page PDF). If that produces nothing, the DOCX is converted
// with writer_pdf_Export.

import crypto from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { pathToFileURL } from 'node:url'
import { officeEngineDir } from './office-engine'

const CONVERT_TIMEOUT_MS = 120_000

const MACRO_XCU = `<?xml version="1.0" encoding="UTF-8"?>
<oor:items xmlns:oor="http://openoffice.org/2001/registry" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
<item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="MacroSecurityLevel" oor:op="fuse"><value>3</value></prop></item>
<item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="DisableMacrosExecution" oor:op="fuse"><value>true</value></prop></item>
</oor:items>
`

export interface ExportDeps {
  dataDir: () => string | null
  /** `app.getPath('downloads')`. */
  downloadsDir: () => string
  run: (cmd: string, args: string[], timeoutMs: number) => Promise<{ code: number | null }>
  /** Import the PDF and wait until its job succeeds or fails. */
  importPdf: (pdfPath: string) => Promise<{ ok: boolean; message?: string }>
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

function nonempty(file: string): boolean {
  try {
    return fs.statSync(file).size > 0
  } catch {
    return false
  }
}

function sofficeArgs(profile: string, convertTo: string, outDir: string, input: string): string[] {
  return [
    '--headless',
    '--norestore',
    '--nolockcheck',
    '--nodefault',
    '--nofirststartwizard',
    `-env:UserInstallation=${pathToFileURL(profile).href}`,
    '--convert-to',
    convertTo,
    '--outdir',
    outDir,
    input,
  ]
}

export async function exportTranslation(
  deps: ExportDeps,
  req: { title: string; html: string; lang: 'en' | 'zh' },
): Promise<ExportResult> {
  const dataDir = deps.dataDir()
  const engineDir = dataDir === null ? null : officeEngineDir(dataDir)
  if (dataDir === null || engineDir === null || !fs.existsSync(path.join(engineDir, 'READY.json'))) {
    return { ok: false, code: 'engine_unavailable' }
  }
  const soffice = path.join(engineDir, 'LibreOffice.app', 'Contents', 'MacOS', 'soffice')
  const tmp = path.join(dataDir, 'tmp', `translate-${crypto.randomBytes(8).toString('hex')}`)
  try {
    const stem = safeTitle(req.title)
    const label = `${stem}-${langSuffix(req.lang)}`
    fs.mkdirSync(path.join(tmp, 'profile', 'user'), { recursive: true })
    fs.writeFileSync(path.join(tmp, 'profile', 'user', 'registrymodifications.xcu'), MACRO_XCU)
    fs.symlinkSync(path.join(engineDir, 'fonts'), path.join(tmp, 'profile', 'user', 'fonts'))
    const htmlPath = path.join(tmp, `${stem}.html`)
    fs.writeFileSync(htmlPath, req.html, 'utf8')
    const profile = path.join(tmp, 'profile')

    const docxRun = await deps.run(
      soffice,
      sofficeArgs(profile, 'docx:MS Word 2007 XML', tmp, htmlPath),
      CONVERT_TIMEOUT_MS,
    )
    const docxPath = path.join(tmp, `${stem}.docx`)
    if (docxRun.code !== 0 || !nonempty(docxPath)) {
      return { ok: false, code: 'convert_failed', message: `soffice docx exited ${docxRun.code}` }
    }

    const webRun = await deps.run(
      soffice,
      sofficeArgs(profile, 'pdf:writer_web_pdf_Export', tmp, htmlPath),
      CONVERT_TIMEOUT_MS,
    )
    const pdfPath = path.join(tmp, `${stem}.pdf`)
    if (webRun.code !== 0 || !nonempty(pdfPath)) {
      if (fs.existsSync(pdfPath)) fs.rmSync(pdfPath, { force: true })
      const writerRun = await deps.run(
        soffice,
        sofficeArgs(profile, 'pdf:writer_pdf_Export', tmp, docxPath),
        CONVERT_TIMEOUT_MS,
      )
      if (writerRun.code !== 0 || !nonempty(pdfPath)) {
        return { ok: false, code: 'convert_failed', message: `soffice pdf exited ${writerRun.code}` }
      }
    }

    const downloads = deps.downloadsDir()
    fs.mkdirSync(downloads, { recursive: true })
    const docxName = uniqueName(downloads, label, '.docx')
    fs.copyFileSync(docxPath, path.join(downloads, docxName))
    const importPath = path.join(tmp, `${label}.pdf`)
    fs.copyFileSync(pdfPath, importPath)
    const imported = await deps.importPdf(importPath)
    if (!imported.ok) {
      const saved = `Word 已保存为下载/${docxName}`
      return { ok: false, code: 'import_failed', docxName, message: imported.message ? `${imported.message}（${saved}）` : saved }
    }
    return { ok: true, docxName }
  } catch (error) {
    return { ok: false, code: 'export_failed', message: error instanceof Error ? error.message : String(error) }
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true })
  }
}
