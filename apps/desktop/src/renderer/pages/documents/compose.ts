// D2 拆分合并 PDF: a new document of pages of existing ones. The OS answers
// with the import job of the new document, which the list picks up when it
// is imported (`document.imported`); this waits for the job to say how it went.

import type { DocumentsRequest, DocumentsResponse } from '../../../shared/ipc-types'
import { describeError } from './importStore'

type Api = Window['agent24']['documents']
type Parts = Extract<DocumentsRequest, { op: 'compose' }>['parts']
type Job = Extract<DocumentsResponse<'compose'>, { ok: true }>['data']
type Sleep = (ms: number) => Promise<unknown>

/** `all`, or pages and ranges like `1-3,5` or `2-`, as the OS takes them. */
export function pagesOk(spec: string): boolean {
  const n = '[1-9][0-9]{0,5}'
  // `2-`: to the last page (Loop 4).
  return spec === 'all' || new RegExp(`^${n}(-(${n})?)?(,${n}(-(${n})?)?)*$`).test(spec)
}

export type ComposeOutcome = { ok: true; documentId: string | null } | { ok: false; message: string; engineMissing?: true }

async function waitJob(api: Api, job: Job, sleep: Sleep): Promise<ComposeOutcome> {
  // An Office conversion runs up to 120 s before its import (PR-Daemon #1909 ③): wait up to 5 minutes.
  for (let i = 0; i < 300 && (job.status === 'queued' || job.status === 'running'); i++) {
    await sleep(1000)
    const next = await api.request({ op: 'job', jobId: job.job_id })
    if (!next.ok) break
    job = next.data
  }
  if (job.status === 'succeeded') return { ok: true, documentId: job.result?.document_id ?? null }
  return { ok: false, message: job.error?.message ?? job.status }
}

export async function compose(api: Api, parts: Parts, title: string, sleep: Sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))): Promise<ComposeOutcome> {
  const res = await api.request({ op: 'compose', parts, title })
  if (!res.ok) return { ok: false, message: describeError(res) }
  return waitJob(api, res.data, sleep)
}

/** E1b/D5a: a DOCX filled or patched into a new document; waits for its import. */
export async function makeDocx(
  api: Api,
  req: Extract<DocumentsRequest, { op: 'fill' | 'patch' }>,
  sleep: Sleep = (ms: number) => new Promise((r) => setTimeout(r, ms)),
): Promise<ComposeOutcome> {
  const res = await api.request(req)
  if (!res.ok) return { ok: false, message: describeError(res) }
  return waitJob(api, res.data as Job, sleep)
}

/** Office → PDF. `engineMissing` is LibreOffice not installed yet. */
export async function convert(api: Api, documentId: string, title?: string, sleep: Sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))): Promise<ComposeOutcome> {
  const res = await api.request(title === undefined ? { op: 'convert', documentId } : { op: 'convert', documentId, title })
  if (!res.ok) {
    const message = describeError(res)
    if (res.error.code === 'engine_unavailable' && res.error.details?.engine === 'libreoffice') {
      return { ok: false, message, engineMissing: true }
    }
    return { ok: false, message }
  }
  return waitJob(api, res.data, sleep)
}

/** The title of the PDF a document is converted to for checking or translating.
 * L6-F5: version 2 on has its own (「<名>（第 2 版，转换）.pdf」), so a later
 * version is never read from an earlier version's PDF; version 1 keeps the
 * title it always had, so its conversions made before are still found. */
export function convertedTitle(title: string, revision = 1): string {
  const version = revision > 1 ? `第 ${revision} 版，` : ''
  return `${title.replace(/\.(docx?|xlsx?|pptx?)$/i, '')}（${version}转换）.pdf`.slice(0, 500)
}

/** D3/D4: the PDF of an Office document's current version, the one
 * converted before if there is one (newest), else converted now under
 * {@link convertedTitle}, so neither the original's own PDF nor a heap of
 * same-named copies is mistaken for it. */
export async function convertedPdf(api: Api, documentId: string, title: string): Promise<ComposeOutcome> {
  const got = await api.request({ op: 'get', documentId })
  const want = convertedTitle(title, got.ok ? got.data.head_revision : 1)
  const listed = await api.request({ op: 'list', limit: 200 })
  if (listed.ok) {
    const done = listed.data.documents
      .filter((d) => d.title === want && d.media_type === 'application/pdf')
      .sort((a, b) => b.created_at.localeCompare(a.created_at))[0]
    if (done) return { ok: true, documentId: done.document_id }
  }
  return convert(api, documentId, want)
}
