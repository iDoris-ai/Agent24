import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { LIBREOFFICE_VERSION } from './office-engine'
import { exportTranslation, safeTitle, type ExportDeps } from './translation-export'

const roots: string[] = []

afterEach(() => {
  for (const root of roots) fs.rmSync(root, { recursive: true, force: true })
  roots.length = 0
})

function tempDir(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'translation-export-'))
  roots.push(dir)
  return dir
}

function engineReady(dataDir: string): void {
  const dir = path.join(dataDir, 'os', 'documents', 'engines', 'libreoffice', LIBREOFFICE_VERSION)
  fs.mkdirSync(dir, { recursive: true })
  fs.writeFileSync(path.join(dir, 'READY.json'), '{}\n')
}

function leftovers(dataDir: string): string[] {
  const tmp = path.join(dataDir, 'tmp')
  if (!fs.existsSync(tmp)) return []
  return fs.readdirSync(tmp).filter((name) => name.startsWith('translate-'))
}

function produce(args: string[]): void {
  const convertTo = args[args.indexOf('--convert-to') + 1]
  const outdir = args[args.indexOf('--outdir') + 1]
  const input = args[args.length - 1]
  const ext = convertTo.startsWith('docx') ? '.docx' : '.pdf'
  const stem = path.basename(input, path.extname(input))
  fs.writeFileSync(path.join(outdir, `${stem}${ext}`), 'file-bytes')
}

function depsFor(dataDir: string, downloads: string, run: ExportDeps['run'], importPdf: ExportDeps['importPdf']): ExportDeps {
  return { dataDir: () => dataDir, downloadsDir: () => downloads, run, importPdf }
}

describe('exportTranslation', () => {
  it('refuses when LibreOffice is not installed', async () => {
    const dataDir = tempDir()
    const run = vi.fn()
    const missing = await exportTranslation(
      depsFor(dataDir, path.join(dataDir, 'downloads'), run, vi.fn()),
      { title: '租赁合同', html: '<p>hi</p>', lang: 'en' },
    )
    expect(missing).toEqual({ ok: false, code: 'engine_unavailable' })
    expect(run).not.toHaveBeenCalled()
    expect(leftovers(dataDir)).toEqual([])

    const noDir = await exportTranslation(
      { dataDir: () => null, downloadsDir: () => path.join(dataDir, 'downloads'), run, importPdf: vi.fn() },
      { title: '租赁合同', html: '<p>hi</p>', lang: 'en' },
    )
    expect(noDir).toEqual({ ok: false, code: 'engine_unavailable' })
  })

  it('copies the docx into Downloads, imports the pdf, and removes the temp dir', async () => {
    const dataDir = tempDir()
    const downloads = path.join(dataDir, 'downloads')
    engineReady(dataDir)
    const imported: string[] = []
    let xcu = ''
    let fontsLink = ''
    const run = vi.fn(async (_cmd: string, args: string[]) => {
      const convertTo = args[args.indexOf('--convert-to') + 1]
      if (convertTo.startsWith('docx') && !xcu) {
        const env = args.find((arg) => arg.startsWith('-env:UserInstallation=')) ?? ''
        const profile = fileURLToPath(env.slice('-env:UserInstallation='.length))
        xcu = fs.readFileSync(path.join(profile, 'user', 'registrymodifications.xcu'), 'utf8')
        fontsLink = fs.readlinkSync(path.join(profile, 'user', 'fonts'))
      }
      produce(args)
      return { code: 0 }
    })
    const importPdf = vi.fn(async (pdfPath: string) => {
      expect(fs.statSync(pdfPath).size).toBeGreaterThan(0)
      imported.push(path.basename(pdfPath))
      return { ok: true }
    })
    const out = await exportTranslation(
      depsFor(dataDir, downloads, run, importPdf),
      { title: '租赁/合同', html: '<p>甲</p>', lang: 'en' },
    )
    expect(out).toEqual({ ok: true, docxName: '租赁合同-English.docx' })
    const first = run.mock.calls[0][1]
    expect(first.slice(0, 5)).toEqual(['--headless', '--norestore', '--nolockcheck', '--nodefault', '--nofirststartwizard'])
    expect(first.some((arg: string) => arg.startsWith('-env:UserInstallation=file://'))).toBe(true)
    expect(xcu).toContain('DisableMacrosExecution')
    expect(fontsLink).toContain(`${path.sep}fonts`)
    expect(fs.readFileSync(path.join(downloads, '租赁合同-English.docx'), 'utf8')).toBe('file-bytes')
    expect(imported).toEqual(['租赁合同-English.pdf'])
    expect(run.mock.calls.map((call) => call[1][call[1].indexOf('--convert-to') + 1])).toEqual([
      'docx:MS Word 2007 XML',
      'pdf:writer_web_pdf_Export',
    ])
    expect(leftovers(dataDir)).toEqual([])

    const again = await exportTranslation(
      depsFor(dataDir, downloads, run, importPdf),
      { title: '租赁/合同', html: '<p>甲</p>', lang: 'en' },
    )
    expect(again).toEqual({ ok: true, docxName: '租赁合同-English (2).docx' })
    expect(fs.existsSync(path.join(downloads, '租赁合同-English.docx'))).toBe(true)
    expect(fs.existsSync(path.join(downloads, '租赁合同-English (2).docx'))).toBe(true)
    expect(leftovers(dataDir)).toEqual([])

    const zh = await exportTranslation(
      depsFor(dataDir, downloads, run, importPdf),
      { title: '租赁合同', html: '<p>甲</p>', lang: 'zh' },
    )
    expect(zh.docxName).toBe('租赁合同-中文.docx')
    expect(leftovers(dataDir)).toEqual([])
  })

  it('converts the docx to pdf when the html pdf filter produces nothing', async () => {
    const dataDir = tempDir()
    const downloads = path.join(dataDir, 'downloads')
    engineReady(dataDir)
    const filters: string[] = []
    const run = vi.fn(async (_cmd: string, args: string[]) => {
      const convertTo = args[args.indexOf('--convert-to') + 1]
      filters.push(convertTo)
      if (convertTo === 'pdf:writer_web_pdf_Export') return { code: 1 }
      produce(args)
      return { code: 0 }
    })
    const importPdf = vi.fn(async () => ({ ok: true }))
    const out = await exportTranslation(
      depsFor(dataDir, downloads, run, importPdf),
      { title: 'note', html: '<p>a</p>', lang: 'en' },
    )
    expect(out.ok).toBe(true)
    expect(filters).toEqual(['docx:MS Word 2007 XML', 'pdf:writer_web_pdf_Export', 'pdf:writer_pdf_Export'])
    expect(importPdf).toHaveBeenCalledTimes(1)
    expect(leftovers(dataDir)).toEqual([])
  })

  it('returns failure and removes the temp dir when soffice fails', async () => {
    const dataDir = tempDir()
    const downloads = path.join(dataDir, 'downloads')
    engineReady(dataDir)
    const importPdf = vi.fn()
    const run = vi.fn(async () => ({ code: 1 }))
    const out = await exportTranslation(
      depsFor(dataDir, downloads, run, importPdf),
      { title: 'note', html: '<p>a</p>', lang: 'en' },
    )
    expect(out.ok).toBe(false)
    expect(out.code).toBe('convert_failed')
    expect(importPdf).not.toHaveBeenCalled()
    expect(leftovers(dataDir)).toEqual([])
    expect(fs.existsSync(downloads)).toBe(false)
  })
})

describe('safeTitle (General 11-18)', () => {
  it('drops the original extension', () => {
    expect(safeTitle('房屋租赁合同-测试.pdf')).toBe('房屋租赁合同-测试')
    expect(safeTitle('Report.DOCX')).toBe('Report')
    expect(safeTitle('v1.2 plan')).toBe('v1.2 plan')
  })
})
