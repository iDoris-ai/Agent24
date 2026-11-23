// The Documents page (#705, ADR-DOC-01 D3/D5): import a PDF, JPEG or PNG and
// see the documents the Documenting OS holds. Everything goes through
// `window.agent24.documents`, by operation; never `backendProxy`.

import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from 'react'
import type { components } from '@agent24/api-client'
import { canCancel, canRetry, cancelImport, describeError, getImportState, nudge, retryImport, startImport, subscribeImport } from './importStore'
import ComparePickup from './ComparePickup'
import DocumentDetail from './DocumentDetail'
import RecentlyDeleted from './RecentlyDeleted'
import { compose } from './compose'
import { clearDocFocus, pendingDocFocus, subscribeDocFocus } from './doc-focus'
import type { TargetLang } from './doc-translate'
import { getLocale, useT } from '../../i18n'
import { documentsMessages } from './Documents.i18n'
import type { DocAgentAccess, DocumentsModuleStatus } from '../../../shared/ipc-types'

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
const OFFICE_FORMAT: Record<string, string> = {
  'application/vnd.openxmlformats-officedocument.wordprocessingml.document': 'DOCX',
  'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet': 'XLSX',
  'application/vnd.openxmlformats-officedocument.presentationml.presentation': 'PPTX',
}
const formatOf = (media: string): string => OFFICE_FORMAT[media] ?? media

function accessUntilDate(until: string | null): string {
  if (!until) return ''
  const d = new Date(until)
  if (Number.isNaN(d.getTime())) return until
  const locale = getLocale()
  const tag = locale === 'zh' ? 'zh-CN' : locale === 'th' ? 'th-TH' : 'en'
  return d.toLocaleDateString(tag, { year: 'numeric', month: 'short', day: 'numeric' })
}

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
  const [agentAccess, setAgentAccess] = useState<DocAgentAccess | null>(null)
  const [agentAccessBusy, setAgentAccessBusy] = useState(false)
  const [agentAccessNote, setAgentAccessNote] = useState<string | null>(null)
  useEffect(() => {
    const fn = api.agentAccess
    if (!fn) return
    let live = true
    void fn('state').then((s) => { if (live) setAgentAccess(s) }).catch(() => {})
    return () => { live = false }
  }, [api])
  const [open, setOpen] = useState<string | null>(null)
  // F3: a source clicked in the conversation opens its document at that block.
  const docFocus = useSyncExternalStore(subscribeDocFocus, pendingDocFocus)
  const [focus, setFocus] = useState(docFocus)
  useEffect(() => {
    if (!docFocus) return
    clearDocFocus()
    setFocus(docFocus)
    setOpen(docFocus.documentId)
  }, [docFocus])
  // D3: the document opened to be checked straight away (an Office one's PDF).
  const [autoCheck, setAutoCheck] = useState<string | null>(null)
  // D4: the PDF opened to be translated straight away, and into which language.
  const [autoTranslate, setAutoTranslate] = useState<{ id: string; lang: TargetLang } | null>(null)
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
  // D2: PDFs ticked for merging, in the order ticked, and how the last split or merge went.
  const [picked, setPicked] = useState<string[]>([])
  const [composing, setComposing] = useState<{ working: boolean; message: string | null; ok: boolean }>({ working: false, message: null, ok: true })
  const merge = async () => {
    const chosen = picked.map((id) => docs.find((d) => d.document_id === id)).filter((d) => d !== undefined)
    setComposing({ working: true, message: null, ok: true })
    const title = t('merge.title', { names: chosen.map((d) => d.title).join(' + ') }).slice(0, 500)
    const out = await compose(api, chosen.map((d) => ({ documentId: d.document_id })), title)
    setComposing({ working: false, ok: out.ok, message: out.ok ? t('compose.done') : t('compose.failed', { error: out.message }) })
    if (out.ok) { setPicked([]); void reload() }
  }
  // L6-F2: delete one document (soft: 「最近删除」 below restores it for 7 days).
  const [trashVersion, setTrashVersion] = useState(0)
  const [deleteNote, setDeleteNote] = useState<string | null>(null)
  const remove = async (d: Doc) => {
    if (!window.confirm(t('delete.confirm', { title: d.title }))) return
    const res = await api.request({ op: 'delete', documentId: d.document_id }).catch(() => null)
    if (!res || !res.ok) {
      setDeleteNote(t('delete.failed', { title: d.title, error: res ? describeError(res) : '' }))
      return
    }
    setDeleteNote(null)
    if (open === d.document_id) setOpen(null)
    setPicked((p) => p.filter((x) => x !== d.document_id))
    setTrashVersion((v) => v + 1)
    void reload()
  }
  return (
    <div className="content">
      <div className="page-title" role="heading" aria-level={1} tabIndex={-1} ref={pageTitle}>
        {t('title')}
      </div>
      <p style={{ color: 'var(--muted)', fontSize: 12 }}>{t('subtitle')}</p>
      <ComparePickup api={api} />
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
        {api.agentAccess && (
          <button
            className="btn"
            disabled={agentAccessBusy}
            onClick={() => {
              const fn = api.agentAccess
              if (!fn || agentAccessBusy) return
              setAgentAccessBusy(true)
              const turningOff = agentAccess?.enabled === true
              setAgentAccessNote(null)
              void fn(turningOff ? 'disable' : 'enable')
                .then((s) => {
                  setAgentAccess(s)
                  if (s.ok === false) setAgentAccessNote(t(turningOff ? 'agentAccess.offFailed' : 'agentAccess.onFailed', { code: s.code ?? '' }))
                })
                .catch(() => {})
                .finally(() => setAgentAccessBusy(false))
            }}
          >
            {agentAccess?.enabled
              ? t('agentAccess.on', { date: accessUntilDate(agentAccess.until) })
              : t('agentAccess.off')}
          </button>
        )}
        {agentAccessNote && <span role="alert" style={{ fontSize: 12, color: 'var(--danger, #c0392b)' }}>{agentAccessNote}</span>}
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
        {picked.length >= 2 && (
          <button className="btn" onClick={() => void merge()} disabled={composing.working}>
            {t('merge.button', { count: picked.length })}
          </button>
        )}
        {(composing.working || composing.message) && (
          <span role={composing.ok ? 'status' : 'alert'}>{composing.working ? t('compose.working') : composing.message}</span>
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
          focus={focus?.documentId === open ? focus : null}
          autoCheck={autoCheck === open}
          onAdded={() => void reload()}
          autoTranslate={autoTranslate?.id === open ? autoTranslate.lang : null}
          onOpen={(id, check) => { setAutoCheck(check ? id : null); setAutoTranslate(null); setOpen(id) }}
          onTranslate={(id, lang) => { setAutoCheck(null); setAutoTranslate({ id, lang }); setOpen(id) }}
          onClose={() => {
            setAutoTranslate(null)
            setFocus(null)
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
              <th style={CELL} />
              <th style={CELL}>{t('list.title')}</th>
              <th style={CELL}>{t('list.format')}</th>
              <th style={CELL}>{t('list.importedAt')}</th>
              <th style={CELL} />
            </tr>
          </thead>
          <tbody>
            {docs.map((d) => (
              <tr key={d.document_id}>
                <td style={CELL}>
                  {d.media_type === 'application/pdf' && (
                    <input type="checkbox" aria-label={t('merge.select', { title: d.title })}
                      checked={picked.includes(d.document_id)}
                      onChange={(e) => setPicked((p) => e.target.checked ? [...p, d.document_id] : p.filter((x) => x !== d.document_id))} />
                  )}
                </td>
                <td style={CELL}>
                  <button className="btn btn-ghost" onClick={(e) => { opener.current = e.currentTarget; setAutoTranslate(null); setFocus(null); setOpen(d.document_id) }} aria-pressed={open === d.document_id}>
                    {d.title}
                  </button>
                </td>
                <td style={CELL}>{formatOf(d.media_type)}</td>
                <td style={CELL}>{new Date(d.created_at).toLocaleString()}</td>
                <td style={CELL}>
                  <button className="btn btn-ghost" aria-label={t('delete.label', { title: d.title })} onClick={() => void remove(d)}>
                    {t('delete.button')}
                  </button>
                </td>
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
      {deleteNote && <div role="alert" style={ALERT}>{deleteNote}</div>}
      <RecentlyDeleted version={trashVersion} onRestored={() => void reload()} />
    </div>
  )
}
