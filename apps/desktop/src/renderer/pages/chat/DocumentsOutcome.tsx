// M1 (General/PM 11-21): what a run's document writes came to, said by the
// page from the daemon's record of the run (`GET /runs/{id}` `documents`),
// never from the answer's words: 0.6.82's model said 「已保存为新文档」 with
// nothing made. Every documents write goes through here (compose, fill,
// patch, and later delete or a saved version); a translation exported on the
// page says its own outcome already.

import { useContext, useEffect, useState, type ReactNode } from 'react'
import { CapabilitiesContext } from '../../capabilities/types'
import { openDocument } from '../documents/doc-focus'
import { useT } from '../../i18n'
import { chatMessages } from '../Chat.i18n'

/** L6-F5: `revisions`, the writes saved as a document's next version. */
export interface RunDocuments { attempted: boolean; jobs: string[]; failed: string[]; revisions?: SavedRevision[] }
export interface SavedRevision { document_id: string; title: string; revision: number }

type Made = { kind: 'made'; title: string; documentId?: string } | { kind: 'failed' } | { kind: 'pending' }

const LINE = { fontSize: 12, marginTop: 2 } as const

/** A documents record as the daemon gives it, or null. */
export function runDocumentsOf(v: unknown): RunDocuments | null {
  const d = v as { attempted?: unknown; jobs?: unknown; failed?: unknown } | null | undefined
  if (!d || d.attempted !== true || !Array.isArray(d.jobs)) return null
  const strings = (a: unknown) => (Array.isArray(a) ? a : []).filter((j): j is string => typeof j === 'string')
  const revisions = (Array.isArray((d as { revisions?: unknown }).revisions) ? (d as { revisions: unknown[] }).revisions : [])
    .flatMap((r) => {
      const x = r as Partial<SavedRevision> | null
      return x && typeof x.document_id === 'string' && typeof x.title === 'string' && typeof x.revision === 'number'
        ? [{ document_id: x.document_id, title: x.title, revision: x.revision }] : []
    })
  return { attempted: true, jobs: strings(d.jobs), failed: strings(d.failed), ...(revisions.length > 0 ? { revisions } : {}) }
}

/** L6-F5: a document's current version, by its id or the name the model gave; null when unread. */
export async function headOf(api: Window['agent24']['documents'] | undefined, ref: string): Promise<number | null> {
  if (!api) return null
  let id = ref
  if (!/^doc_[0-9A-Z]+$/.test(ref)) {
    const listed = await api.request({ op: 'list', limit: 200 }).catch(() => null)
    const want = ref.trim().toLowerCase().replace(/\.[a-z0-9]{1,5}$/, '')
    const docs = listed && listed.ok ? listed.data.documents : []
    // The exact title first: 《合同.docx》 and its converted 《合同.pdf》 share a stem.
    const exact = docs.filter((d) => d.title.trim().toLowerCase() === ref.trim().toLowerCase())
    const hits = exact.length === 1 ? exact : docs.filter((d) => d.title.toLowerCase().replace(/\.[a-z0-9]{1,5}$/, '') === want)
    if (hits.length !== 1) return null
    id = hits[0]!.document_id
  }
  const doc = await api.request({ op: 'get', documentId: id }).catch(() => null)
  return doc && doc.ok && typeof doc.data.head_revision === 'number' ? doc.data.head_revision : null
}

/** A document a call named, by its title when the name is a document id. */
export async function documentName(api: Window['agent24']['documents'] | undefined, name: string): Promise<string> {
  if (!api || !/^doc_[0-9A-Z]+$/.test(name)) return name
  const doc = await api.request({ op: 'get', documentId: name }).catch(() => null)
  return doc && doc.ok && typeof doc.data.title === 'string' && doc.data.title ? doc.data.title : name
}

/** What one import job made: its document's title once it has one. */
export async function jobOutcome(
  api: Window['agent24']['documents'] | undefined,
  jobId: string,
  sleep: (ms: number) => Promise<unknown> = (ms) => new Promise((r) => setTimeout(r, ms)),
): Promise<Made> {
  if (!api) return { kind: 'pending' }
  for (let i = 0; i < 120; i++) {
    const res = await api.request({ op: 'job', jobId }).catch(() => null)
    if (!res || !res.ok) return { kind: 'pending' }
    const job = res.data
    if (job.status === 'succeeded') {
      const id = job.result?.document_id
      if (!id) return { kind: 'made', title: '' }
      const doc = await api.request({ op: 'get', documentId: id }).catch(() => null)
      return { kind: 'made', title: doc && doc.ok ? doc.data.title : '', documentId: id }
    }
    if (job.status !== 'queued' && job.status !== 'running') return { kind: 'failed' }
    await sleep(1000)
  }
  return { kind: 'pending' }
}

/** L7-K0 (FU-139): a line that names a document opens it on the documents
 * page; one that cannot say which (no id) stays a plain line. */
function DocLine({ documentId, children }: { documentId?: string; children: ReactNode }) {
  const { open } = useContext(CapabilitiesContext)
  if (!documentId) return <div>{children}</div>
  return (
    <div>
      <button type="button" className="btn btn-ghost" onClick={() => { openDocument(documentId); open('documents', 'other') }}>{children}</button>
    </div>
  )
}

/** The line under an answer: 「已生成：《…》」 for each new document, or
 * 「这次没有改动文档」 when a write was asked for and none went through. */
export function DocumentsOutcome({ documents }: { documents: RunDocuments | null | undefined }) {
  const t = useT(chatMessages)
  const [made, setMade] = useState<Made[] | null>(null)
  const [failed, setFailed] = useState<string[]>([])
  const jobs = documents?.jobs.join(',') ?? ''
  const failedKey = documents?.failed.join('\n') ?? ''
  // General 0.6.94: A made and B refused read only 「已生成：《A》」 while the
  // model said both were made; B is said too.
  useEffect(() => {
    setFailed([])
    if (!failedKey) return
    let live = true
    void Promise.all(failedKey.split('\n').map((n) => documentName(window.agent24?.documents, n))).then((n) => { if (live) setFailed(n) })
    return () => { live = false }
  }, [failedKey])
  useEffect(() => {
    setMade(null)
    if (!jobs) return
    let live = true
    void Promise.all(jobs.split(',').map((j) => jobOutcome(window.agent24?.documents, j))).then((m) => { if (live) setMade(m) })
    return () => { live = false }
  }, [jobs])
  if (!documents?.attempted) return null
  const saved = documents.revisions ?? []
  const unchanged = saved.length === 0 && (documents.jobs.length === 0 || (made !== null && made.every((m) => m.kind === 'failed')))
  if (unchanged) return <div role="note" style={{ ...LINE, color: 'var(--danger, #c0392b)' }}>{t('task.documentUnchanged')}</div>
  return (
    <div role="note" style={{ ...LINE, color: 'var(--muted)' }}>
      {saved.map((r, i) => <DocLine key={`s${i}`} documentId={r.document_id}>{t('task.documentSaved', { title: r.title || t('task.documentUntitled'), v: String(r.revision) })}</DocLine>)}
      {(made ?? documents.jobs.map(() => ({ kind: 'pending' as const }))).map((m, i) => (
        <DocLine key={i} documentId={m.kind === 'made' ? m.documentId : undefined}>
          {m.kind === 'made' ? t('task.documentMade', { title: m.title || t('task.documentUntitled') })
            : m.kind === 'failed' ? t('task.documentFailed') : t('task.documentMaking')}
        </DocLine>
      ))}
      {failed.map((title, i) => (
        <div key={`f${i}`} style={{ color: 'var(--danger, #c0392b)' }}>{t('task.documentNotChanged', { title })}</div>
      ))}
    </div>
  )
}
