import { describe, expect, it, vi } from 'vitest'
import { deliverDocument, offeredName, unusedPath, type DeliverDeps } from './doc-deliver'

const DOC = 'doc_01M679KMY6D75P0EZACBS95JYA'
const COPY = 'doc_01M679KMY6D75P0EZACBS95JYB'
const PDF = 'doc_01M679KMY6D75P0EZACBS95JYC'
const job = (id: string, status: string, document?: string) => ({ job_id: id, kind: 'import', status, ...(document ? { result: { document_id: document } } : {}) })
const RECEIPT = { document_id: DOC, title: '合同.docx', revision: 3, at: '2026-11-26T10:00:00.000Z', format: 'pdf', file_name: '合同（交付）.pdf', saved_to: '/Users/u/Desktop/合同（交付）.pdf', sha256: 'sha256:aa', copy_document_id: PDF, again: false }

function deps(over: Partial<DeliverDeps> = {}) {
  const files = new Map<string, Buffer>()
  const calls: Array<Record<string, unknown>> = []
  const d: DeliverDeps = {
    showSaveDialog: vi.fn(async (o: { defaultPath: string }) => ({ canceled: false, filePath: o.defaultPath })),
    call: vi.fn(async (req: Record<string, unknown>) => {
      calls.push(req)
      switch (req.op) {
        case 'deliver': return { ok: true, status: 202, data: job('job_1', 'queued') }
        case 'convert': return { ok: true, status: 202, data: job('job_2', 'running') }
        case 'content': return { ok: true, status: 200, data: { media_type: 'x', bytes: 3, data: Buffer.from(req.documentId === PDF ? 'pdf' : 'doc').toString('base64') } }
        case 'receipt': return { ok: true, status: 201, data: { ...RECEIPT, saved_to: req.savedTo, file_name: String(req.savedTo).split('/').pop() } }
        default: throw new Error(`unexpected ${String(req.op)}`)
      }
    }) as DeliverDeps['call'],
    jobEnd: vi.fn(async (j: { job_id: string }) => ({ ok: true as const, job: (j.job_id === 'job_1' ? job('job_1', 'succeeded', COPY) : job('job_2', 'succeeded', PDF)) as never })),
    desktopDir: () => '/Users/u/Desktop',
    exists: (p: string) => files.has(p),
    writeFile: vi.fn((p: string, b: Buffer) => { files.set(p, b) }),
    ...over,
  }
  return { d, files, calls }
}

const REQ = { documentId: DOC, revision: 3, format: 'pdf' as const, title: '合同.docx' }

describe('L8 DOC-4: delivering a clean copy', () => {
  it('cancelled in the save dialog: no copy, no file, no receipt', async () => {
    const { d, calls, files } = deps({ showSaveDialog: vi.fn(async () => ({ canceled: true })) })
    expect(await deliverDocument(d, REQ)).toEqual({ ok: false, canceled: true })
    expect(calls).toEqual([])
    expect(files.size).toBe(0)
  })

  it('asks where first, then copies the version checked, converts it, writes the file, and only then records the receipt', async () => {
    const { d, calls, files } = deps()
    const out = await deliverDocument(d, REQ)
    expect(d.showSaveDialog).toHaveBeenCalledWith({ defaultPath: '/Users/u/Desktop/合同（交付）.pdf', filters: [{ name: 'PDF', extensions: ['pdf'] }] })
    expect(calls.map((c) => c.op)).toEqual(['deliver', 'convert', 'content', 'receipt'])
    expect(calls[0]).toEqual({ op: 'deliver', documentId: DOC, expectRevision: 3 })
    expect(calls[1]).toMatchObject({ op: 'convert', documentId: COPY })
    expect(calls[2]).toEqual({ op: 'content', documentId: PDF })
    expect(calls[3]).toEqual({ op: 'receipt', documentId: DOC, revision: 3, copyDocumentId: PDF, savedTo: '/Users/u/Desktop/合同（交付）.pdf' })
    expect(files.get('/Users/u/Desktop/合同（交付）.pdf')?.toString()).toBe('pdf')
    expect(out).toMatchObject({ ok: true, savedTo: '/Users/u/Desktop/合同（交付）.pdf', fileName: '合同（交付）.pdf' })
    // The receipt came after the file.
    const written = (d.writeFile as ReturnType<typeof vi.fn>).mock.invocationCallOrder[0]!
    const receipted = (d.call as ReturnType<typeof vi.fn>).mock.invocationCallOrder[3]!
    expect(written).toBeLessThan(receipted)
  })

  it('a Word copy is the clean copy itself, no conversion', async () => {
    const { d, calls, files } = deps()
    const out = await deliverDocument(d, { ...REQ, format: 'docx' })
    expect(calls.map((c) => c.op)).toEqual(['deliver', 'content', 'receipt'])
    expect(calls[1]).toEqual({ op: 'content', documentId: COPY })
    expect(files.get('/Users/u/Desktop/合同（交付）.docx')?.toString()).toBe('doc')
    expect(out.ok).toBe(true)
  })

  it('never writes over a file already there: 「名称 (2)」, and the receipt says that path', async () => {
    const { d, calls, files } = deps()
    files.set('/Users/u/Desktop/合同（交付）.pdf', Buffer.from('old'))
    const out = await deliverDocument(d, REQ)
    expect(files.get('/Users/u/Desktop/合同（交付）.pdf')?.toString()).toBe('old')
    expect(files.get('/Users/u/Desktop/合同（交付） (2).pdf')?.toString()).toBe('pdf')
    expect(calls[3]!.savedTo).toBe('/Users/u/Desktop/合同（交付） (2).pdf')
    expect(out).toMatchObject({ ok: true, fileName: '合同（交付） (2).pdf' })
  })

  it('a failed copy, conversion or write writes no receipt', async () => {
    const copyFails = deps({ call: vi.fn(async () => ({ ok: false, status: 409, error: { code: 'revision_conflict', message: 'at 4' } })) as never })
    expect(await deliverDocument(copyFails.d, REQ)).toMatchObject({ ok: false, stage: 'copy', code: 'revision_conflict' })
    expect(copyFails.files.size).toBe(0)

    const convertFails = deps({ jobEnd: vi.fn(async (j: { job_id: string }) => (j.job_id === 'job_1' ? { ok: true as const, job: job('job_1', 'succeeded', COPY) as never } : { ok: false as const, code: 'engine_unavailable', message: 'no LibreOffice' })) })
    expect(await deliverDocument(convertFails.d, REQ)).toMatchObject({ ok: false, stage: 'convert', code: 'engine_unavailable' })
    expect(convertFails.calls.map((c) => c.op)).toEqual(['deliver', 'convert'])
    expect(convertFails.files.size).toBe(0)

    const writeFails = deps({ writeFile: vi.fn(() => { throw new Error('EACCES') }) })
    const w = await deliverDocument(writeFails.d, REQ)
    expect(w).toMatchObject({ ok: false, stage: 'write' })
    expect(w).not.toHaveProperty('savedTo')
    expect(writeFails.calls.map((c) => c.op)).not.toContain('receipt')
  })

  it('a receipt not recorded is said, with the file that was saved', async () => {
    const { d } = deps()
    const base = d.call
    d.call = vi.fn(async (req: Record<string, unknown>) => (req.op === 'receipt' ? { ok: false, status: 503, error: { code: 'storage_unavailable', message: 'down' } } : base(req))) as never
    expect(await deliverDocument(d, REQ)).toMatchObject({ ok: false, stage: 'receipt', savedTo: '/Users/u/Desktop/合同（交付）.pdf' })
  })
})

describe('L8 DOC-4: the file name', () => {
  it('is the title with its extension replaced and （交付） added, safe for a file name', () => {
    expect(offeredName('合同.docx', 'pdf')).toBe('合同（交付）.pdf')
    expect(offeredName('合同.DOCX', 'docx')).toBe('合同（交付）.docx')
    expect(offeredName('a/b:c', 'docx')).toBe('a_b_c（交付）.docx')
    expect(offeredName('   ', 'pdf')).toBe('文档（交付）.pdf')
  })

  it('takes the first 「 (n)」 not there', () => {
    const taken = new Set(['/d/a.pdf', '/d/a (2).pdf'])
    expect(unusedPath('/d/a.pdf', (p) => taken.has(p))).toBe('/d/a (3).pdf')
    expect(unusedPath('/d/b.pdf', (p) => taken.has(p))).toBe('/d/b.pdf')
  })
})
