// M1 (General/PM 11-21): what a run's document writes came to, said by the
// page from the daemon's record of the run (`GET /runs/{id}` `documents`),
// never from the answer's words: 0.6.82's model said 「已保存为新文档」 with
// nothing made. Every documents write goes through here (compose, fill,
// patch, and later delete or a saved version); a translation exported on the
// page says its own outcome already.

import { useEffect, useState } from 'react'
import { useT } from '../../i18n'
import { chatMessages } from '../Chat.i18n'

export interface RunDocuments { attempted: boolean; jobs: string[] }

type Made = { kind: 'made'; title: string } | { kind: 'failed' } | { kind: 'pending' }

const LINE = { fontSize: 12, marginTop: 2 } as const

/** A documents record as the daemon gives it, or null. */
export function runDocumentsOf(v: unknown): RunDocuments | null {
  const d = v as { attempted?: unknown; jobs?: unknown } | null | undefined
  if (!d || d.attempted !== true || !Array.isArray(d.jobs)) return null
  return { attempted: true, jobs: d.jobs.filter((j): j is string => typeof j === 'string') }
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
      return { kind: 'made', title: doc && doc.ok ? doc.data.title : '' }
    }
    if (job.status !== 'queued' && job.status !== 'running') return { kind: 'failed' }
    await sleep(1000)
  }
  return { kind: 'pending' }
}

/** The line under an answer: 「已生成：《…》」 for each new document, or
 * 「这次没有改动文档」 when a write was asked for and none went through. */
export function DocumentsOutcome({ documents }: { documents: RunDocuments | null | undefined }) {
  const t = useT(chatMessages)
  const [made, setMade] = useState<Made[] | null>(null)
  const jobs = documents?.jobs.join(',') ?? ''
  useEffect(() => {
    setMade(null)
    if (!jobs) return
    let live = true
    void Promise.all(jobs.split(',').map((j) => jobOutcome(window.agent24?.documents, j))).then((m) => { if (live) setMade(m) })
    return () => { live = false }
  }, [jobs])
  if (!documents?.attempted) return null
  const unchanged = documents.jobs.length === 0 || (made !== null && made.every((m) => m.kind === 'failed'))
  if (unchanged) return <div role="note" style={{ ...LINE, color: 'var(--danger, #c0392b)' }}>{t('task.documentUnchanged')}</div>
  return (
    <div role="note" style={{ ...LINE, color: 'var(--muted)' }}>
      {(made ?? documents.jobs.map(() => ({ kind: 'pending' as const }))).map((m, i) => (
        <div key={i}>
          {m.kind === 'made' ? t('task.documentMade', { title: m.title || t('task.documentUntitled') })
            : m.kind === 'failed' ? t('task.documentFailed') : t('task.documentMaking')}
        </div>
      ))}
    </div>
  )
}
