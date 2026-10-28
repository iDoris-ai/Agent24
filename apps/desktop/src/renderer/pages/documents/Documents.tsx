// The Documents page (#705, ADR-DOC-01 D3/D5): import a PDF, JPEG or PNG and
// see the documents the Documenting OS holds. Everything goes through
// `window.agent24.documents`, by operation; never `backendProxy`.

import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from 'react'
import type { components } from '@agent24/api-client'
import { canCancel, canRetry, cancelImport, describeError, getImportState, nudge, retryImport, startImport, subscribeImport } from './importStore'
import DocumentDetail from './DocumentDetail'
import { useT } from '../../i18n'
import { documentsMessages } from './Documents.i18n'

type Doc = components['schemas']['Document']

const STATUS_LABELS: Record<string, 'status.queued' | 'status.running' | 'status.cancelling' | 'status.succeeded' | 'status.failed' | 'status.cancelled' | 'status.interrupted'> = {
  queued: 'status.queued',
  running: 'status.running',
  cancelling: 'status.cancelling',
  succeeded: 'status.succeeded',
  failed: 'status.failed',
  cancelled: 'status.cancelled',
  interrupted: 'status.interrupted',
}
const STORAGE_UNAVAILABLE = 'storage-unavailable'
const NOTE = { color: 'var(--muted)', fontSize: 13, margin: '8px 0' } as const
/** The daemon has no documents route at all: the module is not
 * installed (it ships outside the installer, ADR-DOC-01 D6). That is a state,
 * not a failure; any other 404 (a document that is gone) stays an error. */
const moduleNotInstalled = (res: { ok: boolean; status?: number; error?: { code?: string } }): boolean =>
  !res.ok && res.status === 404 && res.error?.code === 'not_found'
const ALERT = { color: '#e5534b', fontSize: 13, margin: '8px 0' } as const
const CELL = { padding: '6px 12px 6px 0', borderBottom: '1px solid var(--border)', textAlign: 'left' } as const

export default function DocumentsPage() {
  const t = useT(documentsMessages)
  const api = window.agent24.documents
  const imp = useSyncExternalStore(subscribeImport, getImportState)
  const [docs, setDocs] = useState<Doc[]>([])
  // The next page's cursor, with the generation of the list it continues.
  const [more, setMore] = useState<{ gen: number; cursor: string } | null>(null)
  const [loadingMore, setLoadingMore] = useState(false)
  const [listError, setListError] = useState<string | null>(null)
  const [unavailable, setUnavailable] = useState<string | null>(null)
  const [notInstalled, setNotInstalled] = useState(false)
  const [open, setOpen] = useState<string | null>(null)
  // The title that opened the details, given focus back when they close;
  // the page's title if a reload has since taken its row away (a document
  // from a later page is not on the new first one).
  const opener = useRef<HTMLButtonElement | null>(null)
  const pageTitle = useRef<HTMLDivElement>(null)
  // Each reload starts a new generation; a reply from an older one (or from
  // an unmounted page) is dropped, so pages never mix or repeat.
  const generation = useRef(0)

  /** The first page, replacing what is shown. */
  const reload = useCallback(async () => {
    const gen = ++generation.current
    setLoadingMore(false)
    setMore(null) // no paging until the new first page is here
    const res = await api.request({ op: 'list' })
    if (gen !== generation.current) return
    if (moduleNotInstalled(res)) {
      setNotInstalled(true)
      setListError(null)
      return
    }
    if (!res.ok) {
      setListError(describeError(res))
      return
    }
    setNotInstalled(false)
    setListError(null)
    setDocs(res.data.documents)
    setMore(res.data.next_cursor ? { gen, cursor: res.data.next_cursor } : null)
  }, [api])

  /** The next page, appended; one at a time, and only to its own list. */
  const loadMore = async () => {
    const gen = generation.current
    if (!more || more.gen !== gen || loadingMore) return
    setLoadingMore(true)
    const res = await api.request({ op: 'list', cursor: more.cursor })
    if (gen !== generation.current) return
    setLoadingMore(false)
    if (!res.ok) {
      setListError(describeError(res))
      return
    }
    setDocs((prev) => [...prev, ...res.data.documents])
    setMore(res.data.next_cursor ? { gen, cursor: res.data.next_cursor } : null)
  }

  useEffect(() => {
    let live = true
    void api.request({ op: 'capabilities' }).then((res) => {
      if (!live) return
      if (moduleNotInstalled(res)) setNotInstalled(true)
      else if (!res.ok) setUnavailable(describeError(res))
      else setUnavailable(res.data.storage.state === 'ready' ? null : STORAGE_UNAVAILABLE)
    })
    return () => {
      live = false
      generation.current++ // drop replies still in flight
    }
  }, [api])

  // The OS's events are hints (ADR-DOC-02 §7): a job event reads that job
  // now, a new document reloads the list (an import from anywhere).
  useEffect(
    () =>
      api.onEvent((e) => {
        if (e.kind === 'job.finished' || e.kind === 'job.progress') nudge(e.payload.job_id)
        else if (e.kind === 'document.imported') void reload()
      }),
    [api, reload],
  )

  // On mount and after each successful import (even one finished while the
  // page was away).
  useEffect(() => {
    void reload()
  }, [reload, imp.imported])

  const { importing, progress, job, error } = imp
  return (
    <div className="content">
      <div className="page-title" role="heading" aria-level={1} tabIndex={-1} ref={pageTitle}>
        {t('title')}
      </div>
      <p style={{ color: 'var(--muted)', fontSize: 12 }}>{t('subtitle')}</p>
      {notInstalled && (
        <div role="status" style={NOTE}>
          <div>{t('notInstalled.title')}</div>
          <div>{t('notInstalled.guide')}</div>
          <code>{t('notInstalled.command')}</code>
        </div>
      )}
      {!notInstalled && unavailable && (
        <div role="alert" style={ALERT}>
          {unavailable === STORAGE_UNAVAILABLE ? t('storage.unavailable') : unavailable}
        </div>
      )}
      <div style={{ display: 'flex', gap: 12, alignItems: 'center', margin: '12px 0' }}>
        <button className="btn btn-primary" onClick={() => void startImport(api)} disabled={importing || notInstalled}>
          {t('import.button')}
        </button>
        {importing && !job && progress && progress.total > 0 && (
          <span role="status">{t('import.uploading', { percent: Math.floor((progress.sent / progress.total) * 100) })}</span>
        )}
        {job && (
          <span role="status">
            {job.error
              ? t('status.error', { status: STATUS_LABELS[job.status] ? t(STATUS_LABELS[job.status]) : job.status, error: job.error.message })
              : STATUS_LABELS[job.status] ? t(STATUS_LABELS[job.status]) : job.status}
          </span>
        )}
        {canCancel(imp) && (
          <button className="btn btn-ghost" onClick={() => void cancelImport(api)} disabled={imp.acting}>
            {t('import.cancel')}
          </button>
        )}
        {canRetry(imp) && (
          <button className="btn btn-ghost" onClick={() => void retryImport(api)} disabled={imp.acting}>
            {t('import.retry')}
          </button>
        )}
      </div>
      {!notInstalled && (error || listError) && (
        <div role="alert" style={ALERT}>
          {error ?? listError}
        </div>
      )}
      {open && (
        <DocumentDetail
          documentId={open}
          onClose={() => {
            setOpen(null)
            ;(opener.current?.isConnected ? opener.current : pageTitle.current)?.focus()
          }}
        />
      )}
      {docs.length === 0 ? (
        <p style={{ color: 'var(--muted)' }}>{t('list.empty')}</p>
      ) : (
        <table style={{ borderCollapse: 'collapse', width: '100%', fontSize: 13 }}>
          <thead>
            <tr>
              <th style={CELL}>{t('list.title')}</th>
              <th style={CELL}>{t('list.format')}</th>
              <th style={CELL}>{t('list.importedAt')}</th>
            </tr>
          </thead>
          <tbody>
            {docs.map((d) => (
              <tr key={d.document_id}>
                <td style={CELL}>
                  <button className="btn btn-ghost" onClick={(e) => { opener.current = e.currentTarget; setOpen(d.document_id) }} aria-pressed={open === d.document_id}>
                    {d.title}
                  </button>
                </td>
                <td style={CELL}>{d.media_type}</td>
                <td style={CELL}>{new Date(d.created_at).toLocaleString()}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {more && (
        <button className="btn btn-ghost" onClick={() => void loadMore()} disabled={loadingMore}>
          {t('list.loadMore')}
        </button>
      )}
    </div>
  )
}
