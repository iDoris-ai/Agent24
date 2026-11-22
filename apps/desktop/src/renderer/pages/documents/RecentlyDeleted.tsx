// L6-F2: the documents deleted less than 7 days ago, at the bottom of the
// Documents page, each with 「恢复」. Read again whenever `version` changes
// (a delete on this page) and after each restore.

import { useEffect, useState } from 'react'
import { useT } from '../../i18n'
import { documentsMessages } from './Documents.i18n'
import { describeError } from './importStore'
import type { DeletedDocument } from '../../../shared/ipc-types'

const NOTE = { color: 'var(--muted)', fontSize: 12 } as const

export default function RecentlyDeleted({ version, onRestored }: { version: number; onRestored: () => void }) {
  const t = useT(documentsMessages)
  const api = window.agent24.documents
  const [docs, setDocs] = useState<DeletedDocument[]>([])
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [reread, setReread] = useState(0)
  useEffect(() => {
    let live = true
    void api.request({ op: 'recentlyDeleted' }).then((res) => {
      // An older module has no such route: nothing to show, not an error.
      if (live && res.ok && Array.isArray(res.data?.documents)) setDocs(res.data.documents)
    }).catch(() => {})
    return () => { live = false }
  }, [api, version, reread])
  const restore = async (d: DeletedDocument) => {
    setBusy(d.document_id)
    setError(null)
    const res = await api.request({ op: 'restore', documentId: d.document_id }).catch(() => null)
    setBusy(null)
    // Past its 7 days (the module answers not found): it cannot be restored,
    // and is never said to be erased; its bytes stay until a blob GC (FU-177).
    if (res && !res.ok && res.error.code === 'not_found') setError(t('trash.expired', { title: d.title }))
    else if (!res || !res.ok) setError(t('trash.restoreFailed', { title: d.title, error: res ? describeError(res) : '' }))
    else onRestored()
    setReread((n) => n + 1)
  }
  if (docs.length === 0 && !error) return null
  return (
    <section aria-label={t('trash.title')} style={{ marginTop: 24 }}>
      <div style={{ fontWeight: 600, fontSize: 13 }}>{t('trash.title')}</div>
      {error && <div role="alert" style={{ color: 'var(--danger)', fontSize: 12 }}>{error}</div>}
      <ul style={{ listStyle: 'none', padding: 0, margin: '8px 0' }}>
        {docs.map((d) => (
          <li key={d.document_id} style={{ display: 'flex', gap: 12, alignItems: 'center', padding: '4px 0' }}>
            <span>{d.title}</span>
            <span style={NOTE}>{t('trash.item', { at: new Date(d.deleted_at).toLocaleString(), until: new Date(d.restorable_until).toLocaleString() })}</span>
            <button className="btn btn-ghost" disabled={busy !== null} onClick={() => void restore(d)}>{t('trash.restore')}</button>
          </li>
        ))}
      </ul>
    </section>
  )
}
