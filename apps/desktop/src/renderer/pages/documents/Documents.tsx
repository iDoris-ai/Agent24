// The Documents page (#705, ADR-DOC-01 D3/D5): import a PDF, JPEG or PNG and
// see the documents the Documenting OS holds. Everything goes through
// `window.agent24.documents`, by operation; never `backendProxy`.

import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from 'react'
import type { components } from '@agent24/api-client'
import { canCancel, canRetry, cancelImport, describeError, getImportState, nudge, retryImport, startImport, subscribeImport } from './importStore'
import DocumentDetail from './DocumentDetail'
import { useT } from '../../i18n'
import { documentsMessages } from './Documents.i18n'
import type { DocumentsModuleStatus } from '../../../shared/ipc-types'

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
const moduleDisabled = (res: { ok: boolean; error?: { code?: string } }): boolean =>
  !res.ok && res.error?.code === 'module_disabled'
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
  // The user turned the module off (503 `module_disabled`, or os.json says
  // so): said as a state, with where to turn it on. Requests are numbered as
  // they are sent, and only the newest one heard from decides, so a slow
  // capabilities reply cannot undo what a later list said, or the reverse.
  const [off, setOff] = useState(false)
  const sent = useRef(0)
  const heard = useRef(0)
  const offFrom = useCallback((stamp: number, value: boolean) => {
    if (stamp < heard.current) return
    heard.current = stamp
    setOff(value)
  }, [])
  // Each endpoint reports on its own; the notice is shown while either says
  // "no route", and never hides another endpoint's real error.
  const [capsMissing, setCapsMissing] = useState(false)
  const [listMissing, setListMissing] = useState(false)
  const notInstalled = capsMissing || listMissing
  // T1-1/T1-2: with no documents route, what is on disk says which note to
  // show: installed (the daemon mounts it once restarted, §9 Q2), installable
  // from the Modules page, or neither (a build without the module).
  const [moduleStatus, setModuleStatus] = useState<DocumentsModuleStatus | null>(null)
  /** Turned off while the daemon runs. The kernel mounts the 503
   * `module_disabled` route only at start-up, so until the next restart a
   * module stopped by Disable answers 503 `module_stopping`, the same as one
   * that crashed; os.json, read by main, tells the two apart. */
  const turnedOff = useCallback(
    async (res: { ok: boolean; status?: number; error?: { code?: string } }): Promise<boolean> => {
      if (moduleDisabled(res)) return true
      if (res.ok || res.status !== 503) return false
      const status = await api.moduleStatus?.().catch(() => null)
      return status?.turnedOff === true
    },
    [api],
  )
  useEffect(() => {
    if (!notInstalled) return
    let live = true
    api.moduleStatus?.()
      .then((s) => { if (live) setModuleStatus(s) })
      .catch(() => {})
    return () => { live = false }
  }, [api, notInstalled])
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
    const stamp = ++sent.current
    const res = await api.request({ op: 'list' })
    if (gen !== generation.current) return
    if (moduleNotInstalled(res)) {
      setListMissing(true)
      setListError(null)
      return
    }
    setListMissing(false)
    if (!res.ok) {
      // Turned off: said once, as the notice, not as a raw error here; the
      // module may have been turned off after capabilities answered.
      const isOff = await turnedOff(res)
      if (gen !== generation.current) return
      setListError(isOff ? null : describeError(res))
      if (isOff) offFrom(stamp, true)
      return
    }
    offFrom(stamp, false) // serving again (turned back on, daemon restarted)
    setListError(null)
    setDocs(res.data.documents)
    setMore(res.data.next_cursor ? { gen, cursor: res.data.next_cursor } : null)
  }, [api, turnedOff, offFrom])

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
    const stamp = ++sent.current
    void api.request({ op: 'capabilities' }).then(async (res) => {
      if (!live) return
      const isOff = await turnedOff(res)
      if (!live) return
      setCapsMissing(moduleNotInstalled(res))
      offFrom(stamp, isOff)
      if (moduleNotInstalled(res) || isOff) setUnavailable(null)
      else if (!res.ok) setUnavailable(describeError(res))
      else setUnavailable(res.data.storage.state === 'ready' ? null : STORAGE_UNAVAILABLE)
    })
    return () => {
      live = false
      generation.current++ // drop replies still in flight
    }
  }, [api, turnedOff, offFrom])

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
        moduleStatus?.installed ? (
          <div role="status" style={NOTE}>
            <div>{t('restart.title')}</div>
            {moduleStatus.externalDaemon ? (
              <>
                <div>{t('restart.guide')}</div>
                <code>{t('restart.command')}</code>
              </>
            ) : (
              <div>{t('restart.tray')}</div>
            )}
          </div>
        ) : moduleStatus?.bundled ? (
          <div role="status" style={NOTE}>
            <div>{t('notInstalled.title')}</div>
            <div>{t('notInstalled.modules')}</div>
          </div>
        ) : (
          <div role="status" style={NOTE}>
            <div>{t('notInstalled.title')}</div>
            <div>{t('notInstalled.guide')}</div>
            <code>{t('notInstalled.command')}</code>
          </div>
        )
      )}
      {(off || unavailable) && (
        <div role="alert" style={ALERT}>
          {off ? t('disabled.notice') : unavailable === STORAGE_UNAVAILABLE ? t('storage.unavailable') : unavailable}
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
      {(error || listError) && (
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
