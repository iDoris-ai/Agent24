// D2 拆分合并 PDF: a new document of pages of existing ones. The OS answers
// with the import job of the new document, which the list picks up when it
// is imported (`document.imported`); this waits for the job to say how it went.

import type { DocumentsRequest } from '../../../shared/ipc-types'
import { describeError } from './importStore'

type Api = Window['agent24']['documents']
type Parts = Extract<DocumentsRequest, { op: 'compose' }>['parts']

/** `all`, or pages and ranges like `1-3,5`, as the OS takes them. */
export function pagesOk(spec: string): boolean {
  const n = '[1-9][0-9]{0,5}'
  return spec === 'all' || new RegExp(`^${n}(-${n})?(,${n}(-${n})?)*$`).test(spec)
}

export type ComposeOutcome = { ok: true; documentId: string | null } | { ok: false; message: string }

export async function compose(api: Api, parts: Parts, title: string, sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))): Promise<ComposeOutcome> {
  const res = await api.request({ op: 'compose', parts, title })
  if (!res.ok) return { ok: false, message: describeError(res) }
  let job = res.data
  for (let i = 0; i < 120 && (job.status === 'queued' || job.status === 'running'); i++) {
    await sleep(1000)
    const next = await api.request({ op: 'job', jobId: job.job_id })
    if (!next.ok) break
    job = next.data
  }
  if (job.status === 'succeeded') return { ok: true, documentId: job.result?.document_id ?? null }
  return { ok: false, message: job.error?.message ?? job.status }
}
