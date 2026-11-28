// L8 DOC-4 (first slice): a clean copy of a document saved where the user
// picks, and a receipt once it is on disk. Nothing is sent out.
//
// The order is the receipt's meaning (监工 11-26): a receipt = the file is on
// this Mac's disk.
//   1. The system save dialog first. Cancelled: nothing at all (no copy in
//      the library, no receipt).
//   2. `deliver` (the module accepts the tracked changes, removes the
//      comments, empties the author into a NEW document 「<名>（交付）」), and
//      its import job waited for; for a PDF, `convert` of that copy, waited for.
//   3. The copy's file (`content`) written at the path picked; a file already
//      there is never overwritten: 「名称 (2).ext」 instead.
//   4. Only once written: `receipt` (the module takes the hash and format
//      from the library's copy, not from here).
// A failure at 2 or 3 writes no receipt; one at 4 says the file was saved
// but the receipt was not.

import path from 'node:path'
import type { DeliverRequest, DeliverResult, DocumentsResponse, DocumentsResults, DeliveryReceipt } from '../shared/ipc-types'

export type { DeliverRequest, DeliverResult }
export type DeliverFormat = DeliverRequest['format']

type Job = DocumentsResults['job']
type Stage = 'copy' | 'convert' | 'write' | 'receipt'

export interface DeliverDeps {
  /** The system save dialog (Electron's `dialog.showSaveDialog`). */
  showSaveDialog: (options: { defaultPath: string; filters: Array<{ name: string; extensions: string[] }> }) => Promise<{ canceled: boolean; filePath?: string }>
  /** One documents operation (writes delegated as the page's are). */
  call: (req: Record<string, unknown>) => Promise<DocumentsResponse>
  /** A job read until it ends, or `ms` pass. */
  jobEnd: (job: Job, ms: number) => Promise<{ ok: true; job: Job } | { ok: false; code?: string; message: string }>
  desktopDir: () => string
  exists: (path: string) => boolean
  writeFile: (path: string, bytes: Buffer) => void
  /** A file this delivery began to write and could not finish. */
  remove: (path: string) => void
}

/** How long the copy's import and the PDF conversion may take. */
export const IMPORT_MS = 120_000
export const CONVERT_MS = 300_000

/** PR-Daemon #2088 B1: pure, so its test needs no Electron. */
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

// Also control characters (PR-Daemon #2088 P3: Node refuses a path with NUL).
// eslint-disable-next-line no-control-regex
const ILLEGAL = /[/\\:*?"<>|\u0000-\u001f\u007f]/g

/** `<title, extension aside>（交付）.<ext>`, with nothing a file name cannot hold. */
export function offeredName(title: string, format: DeliverFormat): string {
  const stripped = title.replace(/\.(docx|pdf|doc)$/i, '').replace(ILLEGAL, '_').trim()
  const suffix = `（交付）.${format}`
  // By characters, not UTF-16 units: an emoji is never cut in half (PR-Daemon #2088 P3).
  let stem = Array.from(stripped || '文档').slice(0, Math.max(0, 200 - suffix.length)).join('').trim()
  if (!stem) stem = '文档'
  return `${stem}${suffix}`
}

/** `path`, or `stem (2).ext`, `stem (3).ext` … the first that is not there.
 * (2)…(999); past that, the original path, so the exclusive write fails. */
export function unusedPath(filePath: string, exists: (p: string) => boolean): string {
  if (!exists(filePath)) return filePath
  const dir = path.dirname(filePath)
  const ext = path.extname(filePath)
  const stem = path.basename(filePath, ext)
  for (let n = 2; n <= 999; n++) {
    const candidate = path.join(dir, `${stem} (${n})${ext}`)
    if (!exists(candidate)) return candidate
  }
  return filePath
}

function failed(stage: Stage, error: unknown, savedTo?: string): DeliverResult {
  const rec = error && typeof error === 'object' ? error as { code?: unknown; message?: unknown } : {}
  const code = typeof rec.code === 'string' && rec.code ? rec.code : 'failed'
  const message = typeof rec.message === 'string' ? rec.message : ''
  return savedTo === undefined ? { ok: false, stage, code, message } : { ok: false, stage, code, message, savedTo }
}

function isJob(data: unknown): data is Job {
  return !!data && typeof data === 'object' && typeof (data as { job_id?: unknown }).job_id === 'string'
}

function copyIdOf(job: Job): string | null {
  const id = job.result?.document_id
  return typeof id === 'string' && id ? id : null
}

function isReceipt(data: unknown): data is DeliveryReceipt {
  return !!data && typeof data === 'object' && typeof (data as { document_id?: unknown }).document_id === 'string'
}

/** `deliver` or `convert`, then the job, or the stage's failure. */
async function imported(deps: DeliverDeps, req: Record<string, unknown>, stage: 'copy' | 'convert', ms: number): Promise<{ id: string } | { result: DeliverResult }> {
  try {
    const res = await deps.call(req)
    if (!res.ok) return { result: failed(stage, res.error) }
    if (!isJob(res.data)) return { result: failed(stage, { message: '没有任务' }) }
    const ended = await deps.jobEnd(res.data, ms)
    if (!ended.ok) return { result: failed(stage, ended) }
    const id = copyIdOf(ended.job)
    if (!id) return { result: failed(stage, { message: '没有副本' }) }
    return { id }
  } catch (error) {
    return { result: failed(stage, error) }
  }
}

export async function deliverDocument(deps: DeliverDeps, req: DeliverRequest): Promise<DeliverResult> {
  const picked = await deps.showSaveDialog({
    defaultPath: path.join(deps.desktopDir(), offeredName(req.title, req.format)),
    filters: req.format === 'pdf' ? [{ name: 'PDF', extensions: ['pdf'] }] : [{ name: 'Word', extensions: ['docx'] }],
  })
  if (picked.canceled || !picked.filePath) return { ok: false, canceled: true }
  // PR-Daemon #2088 P2-2: the file is named for what it holds.
  const ext = `.${req.format}`
  const chosen = path.extname(picked.filePath).toLowerCase() === ext ? picked.filePath : `${picked.filePath}${ext}`

  const copied = await imported(deps, { op: 'deliver', documentId: req.documentId, expectRevision: req.revision }, 'copy', IMPORT_MS)
  if ('result' in copied) return copied.result
  let documentId = copied.id
  if (req.format === 'pdf') {
    const converted = await imported(deps, { op: 'convert', documentId }, 'convert', CONVERT_MS)
    if ('result' in converted) return converted.result
    documentId = converted.id
  }

  let savedTo = ''
  try {
    const res = await deps.call({ op: 'content', documentId })
    if (!res.ok) return failed('write', res.error)
    const data = res.data && typeof res.data === 'object' ? (res.data as { data?: unknown }).data : undefined
    if (typeof data !== 'string') return failed('write', { message: '没有文件内容' })
    savedTo = unusedPath(chosen, deps.exists)
    deps.writeFile(savedTo, Buffer.from(data, 'base64'))
  } catch (error) {
    // PR-Daemon #2088 P2-1: a write that failed midway leaves no part of a
    // file behind. The path was not there before (`unusedPath`, `wx`), so a
    // file there now is this write's; EEXIST is another's, never removed.
    const code = (error as { code?: unknown } | null)?.code
    if (savedTo !== '' && code !== 'EEXIST' && deps.exists(savedTo)) {
      try { deps.remove(savedTo) } catch { /* said below: nothing saved */ }
    }
    return failed('write', error)
  }

  try {
    const res = await deps.call({ op: 'receipt', documentId: req.documentId, revision: req.revision, copyDocumentId: documentId, savedTo: savedTo })
    if (!res.ok) return failed('receipt', res.error, savedTo)
    if (!isReceipt(res.data)) return failed('receipt', { message: '没有回执' }, savedTo)
    return { ok: true, savedTo: savedTo, fileName: path.basename(savedTo), receipt: res.data }
  } catch (error) {
    return failed('receipt', error, savedTo)
  }
}
