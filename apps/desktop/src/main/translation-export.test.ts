import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { execFileSync } from 'node:child_process'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { crc32, docxBytes } from './docx-write'
import { exportTranslation, safeTitle, saveToDownloads, writeUnique, type ExportDeps } from './translation-export'

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

function leftovers(dataDir: string): string[] {
  const tmp = path.join(dataDir, 'tmp')
  if (!fs.existsSync(tmp)) return []
  return fs.readdirSync(tmp).filter((name) => name.startsWith('translate-'))
}

const PARAS = [
  { text: 'Article 1 Lease <A&B>', pageBreakBefore: false },
  { text: 'Article 6 Breach', pageBreakBefore: true },
]

describe('exportTranslation (PR-Daemon #1915 B1: no LibreOffice in this process)', () => {
  it('writes the DOCX to Downloads, then has the module import and convert it', async () => {
    const dataDir = tempDir()
    const downloads = path.join(dataDir, 'downloads')
    const seen: Array<{ docx: Buffer; title: string }> = []
    const importAndConvert = vi.fn(async (docxPath: string, pdfTitle: string) => {
      seen.push({ docx: fs.readFileSync(docxPath), title: pdfTitle })
      return { ok: true }
    })
    const deps: ExportDeps = { dataDir: () => dataDir, downloadsDir: () => downloads, importAndConvert }
    const first = await exportTranslation(deps, { title: '租赁合同.pdf', paragraphs: PARAS, lang: 'en' })
    expect(first).toEqual({ ok: true, docxName: '租赁合同-English.docx' })
    expect(seen[0].title).toBe('租赁合同-English.pdf')
    expect(seen[0].docx.equals(fs.readFileSync(path.join(downloads, '租赁合同-English.docx')))).toBe(true)
    const again = await exportTranslation(deps, { title: '租赁合同.pdf', paragraphs: PARAS, lang: 'en' })
    expect(again.docxName).toBe('租赁合同-English (2).docx')
    expect(leftovers(dataDir)).toEqual([])
  })

  it('keeps the DOCX and says so when the PDF is not made', async () => {
    const dataDir = tempDir()
    const downloads = path.join(dataDir, 'downloads')
    const deps: ExportDeps = {
      dataDir: () => dataDir,
      downloadsDir: () => downloads,
      importAndConvert: async () => ({ ok: false, code: 'engine_unavailable', message: 'not installed' }),
    }
    const out = await exportTranslation(deps, { title: 'a', paragraphs: PARAS, lang: 'zh' })
    expect(out.ok).toBe(false)
    expect(out.code).toBe('engine_unavailable')
    expect(out.docxName).toBe('a-中文.docx')
    expect(fs.existsSync(path.join(downloads, 'a-中文.docx'))).toBe(true)
    expect(leftovers(dataDir)).toEqual([])
  })

  it('runs nothing: the module never imports child_process', () => {
    const src = fs.readFileSync(path.join(__dirname, 'translation-export.ts'), 'utf8')
    expect(src).not.toMatch(/child_process|execFile|spawn|soffice/)
  })
})

describe('docxBytes', () => {
  it('is a zip of the three parts, the paragraphs in order, the page break kept', () => {
    const bytes = docxBytes(PARAS)
    expect(bytes.readUInt32LE(0)).toBe(0x04034b50)
    expect(bytes.readUInt32LE(bytes.length - 22)).toBe(0x06054b50)
    const text = bytes.toString('utf8')
    for (const part of ['[Content_Types].xml', '_rels/.rels', 'word/document.xml']) expect(text).toContain(part)
    expect(text.indexOf('Article 1 Lease &lt;A&amp;B&gt;')).toBeLessThan(text.indexOf('<w:pageBreakBefore/></w:pPr><w:r><w:rPr><w:rFonts w:eastAsia="Noto Sans SC"/></w:rPr><w:t xml:space="preserve">Article 6 Breach'))
    expect(crc32(Buffer.from('123456789'))).toBe(0xcbf43926)
  })

  it.skipIf(process.platform !== 'darwin')('opens as a Word document (macOS textutil)', () => {
    const dir = tempDir()
    const file = path.join(dir, 't.docx')
    fs.writeFileSync(file, docxBytes([{ text: '第一条 租赁', pageBreakBefore: false }, { text: 'Article 6', pageBreakBefore: true }]))
    const out = execFileSync('/usr/bin/textutil', ['-convert', 'txt', '-stdout', file]).toString()
    expect(out).toContain('第一条 租赁')
    expect(out.indexOf('第一条')).toBeLessThan(out.indexOf('Article 6'))
  })

  // N1 (Loop 5): headings carry Word's built-in Heading1–3, which the styles
  // part defines with their outline level (what the navigation pane reads).
  it('writes a heading with its Heading style and defines Heading1–3 with outline levels', () => {
    const bytes = docxBytes([
      { text: 'Lease Contract', pageBreakBefore: false, level: 1 },
      { text: 'Chapter 1', pageBreakBefore: true, level: 2 },
      { text: 'Article 1 The deposit.', pageBreakBefore: false },
    ])
    const text = bytes.toString('utf8')
    expect(text).toContain('<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr>')
    expect(text).toContain('<w:p><w:pPr><w:pStyle w:val="Heading2"/><w:pageBreakBefore/></w:pPr>')
    expect(text).toContain('<w:p><w:r><w:rPr><w:rFonts w:eastAsia="Noto Sans SC"/></w:rPr><w:t xml:space="preserve">Article 1')
    for (const part of ['word/styles.xml', 'word/_rels/document.xml.rels', '/word/styles.xml']) expect(text).toContain(part)
    for (const level of [1, 2, 3]) {
      expect(text).toContain(`w:styleId="Heading${level}"><w:name w:val="heading ${level}"/>`)
      expect(text).toContain(`<w:outlineLvl w:val="${level - 1}"/>`)
    }
  })

  it.skipIf(process.platform !== 'darwin')('the headings open as headings (macOS textutil to HTML)', () => {
    const dir = tempDir()
    const file = path.join(dir, 'h.docx')
    fs.writeFileSync(file, docxBytes([{ text: 'Lease Contract', pageBreakBefore: false, level: 1 }, { text: 'Body text.', pageBreakBefore: false }]))
    const out = execFileSync('/usr/bin/textutil', ['-convert', 'html', '-stdout', file]).toString()
    expect(out).toContain('Lease Contract')
    expect(out).toContain('Body text.')
  })
})

describe('safeTitle (General 11-18)', () => {
  it('drops the original extension', () => {
    expect(safeTitle('房屋租赁合同-测试.pdf')).toBe('房屋租赁合同-测试')
    expect(safeTitle('Report.DOCX')).toBe('Report')
    expect(safeTitle('v1.2 plan')).toBe('v1.2 plan')
  })
})

describe('saveToDownloads (D5a)', () => {
  it('writes the document under its title with its type\'s extension, a taken name getting (2)', async () => {
    const downloads = tempDir()
    const deps: ExportDeps = {
      dataDir: () => tempDir(),
      downloadsDir: () => downloads,
      importAndConvert: vi.fn(),
      fetchContent: vi.fn(async () => ({ ok: true as const, data: Buffer.from('PK-docx').toString('base64'), mediaType: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document' })),
    }
    const first = await saveToDownloads(deps, 'doc_x', '租赁合同（填写）.docx')
    expect(first).toEqual({ ok: true, docxName: '租赁合同（填写）.docx' })
    expect(fs.readFileSync(path.join(downloads, '租赁合同（填写）.docx'), 'utf8')).toBe('PK-docx')
    const second = await saveToDownloads(deps, 'doc_x', '租赁合同（填写）.docx')
    expect(second.docxName).toBe('租赁合同（填写） (2).docx')
  })

  it('L6-F5: an old version is read as that version', async () => {
    const fetchContent = vi.fn(async () => ({ ok: true as const, data: Buffer.from('PK-v1').toString('base64'), mediaType: 'application/pdf' }))
    const deps: ExportDeps = { dataDir: () => null, downloadsDir: () => tempDir(), importAndConvert: vi.fn(), fetchContent }
    expect(await saveToDownloads(deps, 'doc_x', '合同（第 1 版）', 1)).toEqual({ ok: true, docxName: '合同（第 1 版）.pdf' })
    expect(fetchContent).toHaveBeenCalledWith('doc_x', 1)
  })

  it('says why when the file cannot be read', async () => {
    const deps: ExportDeps = {
      dataDir: () => null,
      downloadsDir: () => tempDir(),
      importAndConvert: vi.fn(),
      fetchContent: vi.fn(async () => ({ ok: false as const, code: 'not_found', message: 'no such document' })),
    }
    expect(await saveToDownloads(deps, 'doc_x', 'a')).toEqual({ ok: false, code: 'not_found', message: 'no such document' })
  })
})

describe('writeUnique (PR-Daemon #1930 F3)', () => {
  it('never writes through a dangling link of that name: the next name is taken instead', () => {
    const downloads = tempDir()
    const outside = path.join(tempDir(), 'outside.docx')
    fs.symlinkSync(outside, path.join(downloads, 'a.docx'))
    expect(writeUnique(downloads, 'a', '.docx', Buffer.from('x'))).toBe('a (2).docx')
    expect(fs.existsSync(outside)).toBe(false)
    expect(fs.readFileSync(path.join(downloads, 'a (2).docx'), 'utf8')).toBe('x')
  })
})
