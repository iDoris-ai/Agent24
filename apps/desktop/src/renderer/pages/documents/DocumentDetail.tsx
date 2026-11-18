// One document's details and the text the OS read from its current revision
// (ADR-DOC-02 §3, §4): a page of blocks at a time, never all at once. The
// text is the user's document: it is shown, never logged. Above it, one
// page as the OS renders it, so the text can be checked against the page.

import { Fragment, useContext, useEffect, useRef, useState } from 'react'
import type { components } from '@agent24/api-client'
import { describeError } from './importStore'
import { useT } from '../../i18n'
import { documentsMessages } from './Documents.i18n'
import { CapabilitiesContext } from '../../capabilities/types'
import { askAboutDocument } from '../chat/doc-context'
import { compose, convert, pagesOk } from './compose'
import { askWithProxy, checkDocument, readAllCheckBlocks, type CheckReport, type Source } from './doc-check'
import { paragraphsOf, translateParagraphs, translationHtml, type TargetLang } from './doc-translate'

const OFFICE_TYPE = new Set([
  'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
  'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
  'application/vnd.openxmlformats-officedocument.presentationml.presentation',
])
const isOffice = (media: string): boolean => OFFICE_TYPE.has(media)

type Detail = components['schemas']['DocumentDetail']
type Block = components['schemas']['DocumentsTextBlock']

const ALERT = { color: '#e5534b', fontSize: 13, margin: '8px 0' } as const
/** Asked for so a page reads sharply on a 2x screen; the OS lowers it until
 * the PNG fits in 1 MiB (§4) and answers with the scale it used. */
const PAGE_SCALE = 2

/** A size in bytes, as people read it. */
export function readableSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

export default function DocumentDetail({ documentId, onClose, onOpen, onTranslate, autoCheck = false, autoTranslate = null }: {
  documentId: string
  onClose: () => void
  /** D3: open another document (the PDF an Office one became), checked once it shows. */
  onOpen?: (id: string, check: boolean) => void
  /** D4: open the PDF an Office document became, then translate it. */
  onTranslate?: (id: string, lang: TargetLang) => void
  /** D3: run 「总结并检查」 as soon as the text is read. */
  autoCheck?: boolean
  /** D4: run the translation as soon as the text is read, into this language. */
  autoTranslate?: TargetLang | null
}) {
  const t = useT(documentsMessages)
  const { open } = useContext(CapabilitiesContext)
  // D2: split out some pages as a new document.
  const [splitting, setSplitting] = useState<string | null>(null)
  const [splitNote, setSplitNote] = useState<{ ok: boolean; text: string } | null>(null)
  const [splitBusy, setSplitBusy] = useState(false)
  const split = async (head: Detail, pages: string) => {
    if (!pagesOk(pages)) { setSplitNote({ ok: false, text: t('split.invalid') }); return }
    setSplitBusy(true)
    setSplitNote({ ok: true, text: t('compose.working') })
    const title = t('split.title', { title: head.title ?? '', pages }).slice(0, 500)
    const out = await compose(api, [{ documentId: head.document_id, pages }], title)
    setSplitBusy(false)
    setSplitNote(out.ok ? { ok: true, text: t('compose.done') } : { ok: false, text: t('compose.failed', { error: out.message }) })
    if (out.ok) setSplitting(null)
  }
  const api = window.agent24.documents
  // D1: an Office document is stored as-is. Convert it to PDF before preview.
  const [convertBusy, setConvertBusy] = useState(false)
  const [convertNote, setConvertNote] = useState<{ ok: boolean; text: string } | null>(null)
  const [needEngine, setNeedEngine] = useState(false)
  const [translateAfterEngine, setTranslateAfterEngine] = useState<TargetLang | null>(null)
  const [downloadText, setDownloadText] = useState<string | null>(null)
  const runConvert = async (id: string, thenCheck = false, thenLang: TargetLang | null = null) => {
    setConvertBusy(true)
    setNeedEngine(false)
    setConvertNote({ ok: true, text: t('compose.working') })
    const out = await convert(api, id)
    setConvertBusy(false)
    if (out.ok) {
      setConvertNote({ ok: true, text: t('compose.done') })
      // D3: a Word document is checked as the PDF it became, whose blocks have pages to go back to.
      // D4: the same PDF is what gets translated.
      if (thenLang && out.documentId && onTranslate) onTranslate(out.documentId, thenLang)
      else if (thenCheck && out.documentId && onOpen) onOpen(out.documentId, true)
      return
    }
    if (out.engineMissing) {
      setConvertNote(null)
      setNeedEngine(true)
      setTranslateAfterEngine(thenLang)
      return
    }
    setConvertNote({ ok: false, text: t('compose.failed', { error: out.message }) })
  }
  // A download started elsewhere (another opening of this page) is shown
  // here too, and keeps 「转换为 PDF」 off until it ends.
  useEffect(() => {
    const show = (s: { state: string; received?: number; total?: number }) => {
      if (s.state !== 'installing') return
      const received = s.received ?? 0
      setDownloadText(s.total === undefined
        ? `${(received / (1024 * 1024)).toFixed(1)} MB`
        : t('office.downloading', { percent: Math.floor((received / s.total) * 100) }))
    }
    void api.officeEngineStatus?.().then(show, () => {})
    return api.onOfficeEngineChanged?.((s) => {
      if (s.state === 'installing') show(s)
      else setDownloadText(null)
    })
  }, [api])
  const downloadEngine = async (id: string, follow?: () => Promise<void>) => {
    setNeedEngine(false)
    setConvertNote(null)
    setDownloadText('')
    const stop = api.onOfficeEngineChanged((s) => {
      if (s.state !== 'installing') return
      const received = s.received ?? 0
      setDownloadText(s.total === undefined
        ? `${(received / (1024 * 1024)).toFixed(1)} MB`
        : t('office.downloading', { percent: Math.floor((received / s.total) * 100) }))
    })
    try {
      const status = await api.installOfficeEngine()
      setDownloadText(null)
      if (status.state === 'ready') {
        if (follow) await follow()
        else await runConvert(id)
        return
      }
      setConvertNote({ ok: false, text: t('office.downloadFailed', { error: status.error ?? status.state }) })
    } catch (error) {
      setDownloadText(null)
      setConvertNote({ ok: false, text: t('office.downloadFailed', { error: error instanceof Error ? error.message : String(error) }) })
    } finally {
      stop()
    }
  }
  const [detail, setDetail] = useState<Detail | null>(null)
  const [blocks, setBlocks] = useState<Block[]>([])
  // The pages read so far that were not fully parsed: a page cut short is
  // read on from its middle, so the same page can come back (§3.1).
  const [unparsed, setUnparsed] = useState<ReadonlySet<number>>(new Set())
  const [more, setMore] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  // Each document opened starts a new generation; a reply for an earlier
  // one (or after closing) is dropped.
  const generation = useRef(0)
  const heading = useRef<HTMLDivElement>(null)
  // The page shown: its number, its PNG as a data URL, and the last page once
  // a later one was found missing. Each render asked for supersedes the one
  // before, apart from the text's reads.
  const [pageNo, setPageNo] = useState(1)
  const [image, setImage] = useState<string | null>(null)
  const [lastPage, setLastPage] = useState<number | null>(null)
  const [rendering, setRendering] = useState(false)
  const [imageError, setImageError] = useState<string | null>(null)
  const shown = useRef(0)
  const [checking, setChecking] = useState(false)
  const [checkDone, setCheckDone] = useState(0)
  const [checkTotal, setCheckTotal] = useState(0)
  const [checkError, setCheckError] = useState<string | null>(null)
  const [report, setReport] = useState<CheckReport | null>(null)
  const [highlight, setHighlight] = useState<string | null>(null)
  const [lang, setLang] = useState<TargetLang>(autoTranslate ?? 'en')
  const [translating, setTranslating] = useState(false)
  const [trDone, setTrDone] = useState(0)
  const [trTotal, setTrTotal] = useState(0)
  const [trNote, setTrNote] = useState<{ ok: boolean; text: string } | null>(null)
  const [trNeedEngine, setTrNeedEngine] = useState(false)

  /** Renders page `n` of the head revision and shows it if it is still wanted. */
  const show = async (head: Detail, n: number) => {
    const ask = ++shown.current
    setRendering(true)
    const res = await api.request({
      op: 'renderPage',
      documentId: head.document_id,
      revision: head.head.revision,
      page: n,
      scale: PAGE_SCALE,
    })
    if (ask !== shown.current) return
    setRendering(false)
    if (res.ok) {
      setPageNo(n)
      setImage(`data:image/png;base64,${res.data.png}`)
      setImageError(null)
      return
    }
    // A page past the revision's last is 404 `not_found` (§4): the one shown
    // is the last. The page count is not known before that.
    if (n > 1 && res.status === 404 && res.error.code === 'not_found') {
      setLastPage(n - 1)
      setImageError(null)
      return
    }
    setImageError(describeError(res))
  }

  /** The next page of the head revision's text, appended. */
  const read = async (gen: number, head: Detail, cursor?: string) => {
    setLoading(true)
    const res = await api.request({
      op: 'readText',
      documentId: head.document_id,
      revision: head.head.revision,
      ...(cursor === undefined ? {} : { cursor }),
    })
    if (gen !== generation.current) return
    setLoading(false)
    if (!res.ok) {
      setError(describeError(res))
      return
    }
    setError(null)
    setBlocks((prev) => (cursor === undefined ? res.data.blocks : [...prev, ...res.data.blocks]))
    const pages = (res.data.unparsed_regions ?? []).flatMap((r) => (r.page === undefined ? [] : [r.page]))
    if (pages.length > 0) setUnparsed((prev) => new Set([...prev, ...pages]))
    setMore(res.data.next_cursor)
  }

  const jumpTo = (head: Detail, source: Source) => {
    if (source.page !== undefined) void show(head, source.page)
    setHighlight(source.block_id)
    requestAnimationFrame(() => {
      document.getElementById(`blk-${source.block_id}`)?.scrollIntoView({ behavior: 'smooth', block: 'center' })
    })
  }

  const runCheck = async (head: Detail) => {
    if (checking) return
    const gen = generation.current
    setChecking(true)
    setCheckDone(0)
    setCheckTotal(0)
    setCheckError(null)
    setReport(null)
    try {
      const collected: Block[] = []
      const unparsedPages = new Set<number>()
      let cursor: string | undefined
      for (let i = 0; i < 50; i++) {
        const res = await api.request({
          op: 'readText',
          documentId: head.document_id,
          revision: head.head.revision,
          ...(cursor === undefined ? {} : { cursor }),
        })
        if (gen !== generation.current) return
        if (!res.ok) throw new Error(describeError(res))
        collected.push(...res.data.blocks)
        for (const r of res.data.unparsed_regions ?? []) {
          if (r.page !== undefined) unparsedPages.add(r.page)
        }
        setBlocks([...collected])
        setUnparsed(new Set(unparsedPages))
        setMore(res.data.next_cursor)
        if (!res.data.next_cursor) break
        cursor = res.data.next_cursor
      }
      const proxy = window.agent24.backendProxy
      if (!proxy) throw new Error('backend unavailable')
      const result = await checkDocument({
        ask: (prompt) => askWithProxy(proxy, prompt),
        onProgress: (done, total) => {
          if (gen !== generation.current) return
          setCheckDone(done)
          setCheckTotal(total)
        },
      }, head.title ?? '', collected.map((b) => ({ block_id: b.block_id, page: b.page, text: b.text })))
      if (gen !== generation.current) return
      setReport(result)
    } catch (e) {
      if (gen !== generation.current) return
      setCheckError(e instanceof Error ? e.message : String(e))
    } finally {
      if (gen === generation.current) setChecking(false)
    }
  }

  const runTranslate = async (head: Detail, target: TargetLang) => {
    if (translating || checking) return
    const gen = generation.current
    setTranslating(true)
    setTrDone(0)
    setTrTotal(0)
    setTrNote(null)
    setTrNeedEngine(false)
    try {
      const collected = await readAllCheckBlocks((req) => api.request(req), head.document_id, head.head.revision)
      if (gen !== generation.current) return
      const paras = paragraphsOf(collected)
      if (paras.length === 0) {
        setTrNote({ ok: false, text: t('translate.empty') })
        return
      }
      const proxy = window.agent24.backendProxy
      if (!proxy) throw new Error('backend unavailable')
      const translated = await translateParagraphs({
        ask: (prompt) => askWithProxy(proxy, prompt),
        onProgress: (done, total) => {
          if (gen !== generation.current) return
          setTrDone(done)
          setTrTotal(total)
        },
      }, paras, target)
      if (gen !== generation.current) return
      const html = translationHtml(head.title ?? '', translated, target)
      const out = await api.exportTranslation({ title: head.title ?? '', html, lang: target })
      if (gen !== generation.current) return
      if (!out.ok) {
        if (out.code === 'engine_unavailable') {
          setTrNeedEngine(true)
          return
        }
        setTrNote({ ok: false, text: t('translate.failed', { error: out.message ?? out.code ?? '' }) })
        return
      }
      setTrNote({ ok: true, text: t('translate.done', { name: out.docxName ?? '' }) })
    } catch (e) {
      if (gen !== generation.current) return
      setTrNote({ ok: false, text: t('translate.failed', { error: e instanceof Error ? e.message : String(e) }) })
    } finally {
      if (gen === generation.current) setTranslating(false)
    }
  }

  useEffect(() => {
    const gen = ++generation.current
    setDetail(null)
    setBlocks([])
    setUnparsed(new Set())
    setMore(null)
    setError(null)
    setPageNo(1)
    setImage(null)
    setLastPage(null)
    setRendering(false)
    setImageError(null)
    setConvertBusy(false)
    setConvertNote(null)
    setNeedEngine(false)
    setDownloadText(null)
    setChecking(false)
    setCheckDone(0)
    setCheckTotal(0)
    setCheckError(null)
    setReport(null)
    setHighlight(null)
    setTranslating(false)
    setTrDone(0)
    setTrTotal(0)
    setTrNote(null)
    setTrNeedEngine(false)
    setTranslateAfterEngine(null)
    setLang(autoTranslate ?? 'en')
    heading.current?.focus()
    void api.request({ op: 'get', documentId }).then((res) => {
      if (gen !== generation.current) return
      if (!res.ok) {
        setError(describeError(res))
        return
      }
      setDetail(res.data)
      if (!isOffice(res.data.head.media_type)) {
        void read(gen, res.data)
        void show(res.data, 1)
      }
    })
    return () => {
      generation.current++
      shown.current++
    }
    // `read` and `show` check their refs; only the document matters.
  }, [api, documentId])
  // D3: opened to be checked (an Office document's PDF): once, when its text is first read.
  const autoChecked = useRef<string | null>(null)
  useEffect(() => {
    if (!autoCheck || !detail || detail.document_id !== documentId || blocks.length === 0 || loading) return
    if (autoChecked.current === documentId) return
    autoChecked.current = documentId
    void runCheck(detail)
  })
  // D4: opened to be translated (an Office document's PDF): once, when its text is first read.
  const autoTranslated = useRef<string | null>(null)
  useEffect(() => {
    if (!autoTranslate || !detail || detail.document_id !== documentId || blocks.length === 0 || loading || translating || checking) return
    if (autoTranslated.current === documentId) return
    autoTranslated.current = documentId
    void runTranslate(detail, autoTranslate)
  })

  return (
    <section aria-label={t('detail.label')} style={{ border: '1px solid var(--border)', borderRadius: 6, padding: 12, margin: '12px 0' }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
        <div role="heading" aria-level={2} tabIndex={-1} ref={heading} style={{ fontWeight: 600 }}>
          {detail?.title ?? ''}
        </div>
        <div style={{ display: 'flex', gap: 6 }}>
          {detail && !isOffice(detail.head.media_type) && (
            <button className="btn btn-primary" onClick={() => {
              // F1: a new conversation about this document, its text sent with each turn.
              askAboutDocument({ id: detail.document_id, title: detail.title ?? '', revision: detail.head.revision })
              open('assistant', 'other')
            }}>
              {t('detail.ask')}
            </button>
          )}
          {detail?.head.media_type === 'application/pdf' && (
            <button className="btn btn-ghost" onClick={() => { setSplitting(splitting === null ? '' : null); setSplitNote(null) }}>
              {t('split.button')}
            </button>
          )}
          {detail && !isOffice(detail.head.media_type) && blocks.length > 0 && (
            <button className="btn btn-ghost" disabled={checking} onClick={() => void runCheck(detail)}>
              {t('check.button')}
            </button>
          )}
          {detail && isOffice(detail.head.media_type) && onOpen && (
            <button className="btn btn-ghost" disabled={convertBusy || downloadText !== null} onClick={() => void runConvert(detail.document_id, true)}>
              {t('check.button')}
            </button>
          )}
          {detail && ((!isOffice(detail.head.media_type) && blocks.length > 0) || (isOffice(detail.head.media_type) && onTranslate)) && (
            <>
              <select
                aria-label={t('translate.lang')}
                value={lang}
                disabled={translating || convertBusy}
                onChange={(e) => setLang(e.target.value === 'zh' ? 'zh' : 'en')}
                style={{ fontSize: 13 }}
              >
                <option value="en">{t('translate.en')}</option>
                <option value="zh">{t('translate.zh')}</option>
              </select>
              <button
                className="btn btn-ghost"
                disabled={translating || checking || convertBusy || downloadText !== null}
                onClick={() => {
                  if (isOffice(detail.head.media_type)) void runConvert(detail.document_id, false, lang)
                  else void runTranslate(detail, lang)
                }}
              >
                {t(lang === 'zh' ? 'translate.buttonZh' : 'translate.button')}
              </button>
            </>
          )}
          <button className="btn btn-ghost" onClick={onClose}>
            {t('detail.close')}
          </button>
        </div>
      </div>
      {detail && (checking || checkError || report) && (
        <div style={{ margin: '8px 0' }}>
          {checking && (
            <p role="status" style={{ fontSize: 12, margin: '4px 0' }}>
              {checkTotal === 0
                ? t('check.reading')
                : checkDone >= checkTotal - 1
                  ? t('check.summing')
                  : t('check.progress', { done: String(checkDone + 1), total: String(checkTotal - 1) })}
            </p>
          )}
          {checkError && (
            <p role="alert" style={ALERT}>{checkError}</p>
          )}
          {report && (
            <CheckResult report={report} t={t} onJump={(s) => jumpTo(detail, s)} />
          )}
        </div>
      )}
      {detail && (translating || trNote || trNeedEngine) && (
        <div style={{ margin: '8px 0' }}>
          {translating && (
            <p role="status" style={{ fontSize: 12, margin: '4px 0' }}>
              {trTotal === 0
                ? t('translate.reading')
                : trDone >= trTotal
                  ? t('translate.exporting')
                  : t('translate.progress', { done: String(trDone + 1), total: String(trTotal) })}
            </p>
          )}
          {trNote && (
            <p role={trNote.ok ? 'status' : 'alert'} style={trNote.ok ? { fontSize: 12, margin: '4px 0' } : ALERT}>{trNote.text}</p>
          )}
          {!isOffice(detail.head.media_type) && convertNote && (
            <p role={convertNote.ok ? 'status' : 'alert'} style={convertNote.ok ? { fontSize: 12, margin: '4px 0' } : ALERT}>{convertNote.text}</p>
          )}
          {trNeedEngine && !isOffice(detail.head.media_type) && (
            <>
              <p>{t('office.needEngine')}</p>
              <button className="btn btn-primary" disabled={downloadText !== null} onClick={() => void downloadEngine(detail.document_id, () => runTranslate(detail, lang))}>
                {t('office.download')}
              </button>
            </>
          )}
          {downloadText && !isOffice(detail.head.media_type) ? <p role="status">{downloadText}</p> : null}
        </div>
      )}
      {detail && splitting !== null && (
        <div style={{ display: 'flex', gap: 6, alignItems: 'center', margin: '8px 0' }}>
          <input aria-label={t('split.pages')} placeholder={t('split.pages')} value={splitting}
            onChange={(e) => setSplitting(e.target.value.replace(/\s+/g, ''))} style={{ width: 160 }} />
          <button className="btn btn-primary" disabled={splitBusy || splitting === ''} onClick={() => void split(detail, splitting)}>
            {t('split.go')}
          </button>
          <button className="btn btn-ghost" disabled={splitBusy} onClick={() => setSplitting(null)}>{t('split.cancel')}</button>
        </div>
      )}
      {splitNote && <p role={splitNote.ok ? 'status' : 'alert'} style={{ fontSize: 12, margin: '4px 0' }}>{splitNote.text}</p>}
      {detail && (
        <p style={{ color: 'var(--muted)', fontSize: 12 }}>
          {t('detail.meta', {
            format: detail.head.media_type,
            size: readableSize(detail.head.size),
            revision: detail.head.revision,
            importedAt: new Date(detail.created_at).toLocaleString(),
          })}
        </p>
      )}
      {detail && isOffice(detail.head.media_type) && (
        <div style={{ margin: '8px 0' }}>
          <p>{t('office.note')}</p>
          <button className="btn btn-primary" disabled={convertBusy || downloadText !== null} onClick={() => void runConvert(detail.document_id)}>
            {t('office.convert')}
          </button>
          {needEngine && (
            <>
              <p>{t('office.needEngine')}</p>
              <button className="btn btn-primary" disabled={downloadText !== null} onClick={() => void downloadEngine(
                detail.document_id,
                translateAfterEngine ? () => runConvert(detail.document_id, false, translateAfterEngine) : undefined,
              )}>
                {t('office.download')}
              </button>
            </>
          )}
          {downloadText ? <p role="status">{downloadText}</p> : null}
          {convertNote && <p role={convertNote.ok ? 'status' : 'alert'}>{convertNote.text}</p>}
        </div>
      )}
      {detail && !isOffice(detail.head.media_type) && (
        <figure style={{ margin: '8px 0' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <button
              className="btn btn-ghost"
              onClick={() => void show(detail, pageNo - 1)}
              disabled={rendering || pageNo <= 1}
            >
              {t('detail.prevPage')}
            </button>
            <span style={{ color: 'var(--muted)', fontSize: 12 }}>{t('detail.pageShown', { page: pageNo })}</span>
            <button
              className="btn btn-ghost"
              onClick={() => void show(detail, pageNo + 1)}
              disabled={rendering || lastPage === pageNo}
            >
              {t('detail.nextPage')}
            </button>
          </div>
          {imageError && (
            <p role="status" style={ALERT}>
              {imageError}
            </p>
          )}
          {image && (
            <img
              src={image}
              alt={t('detail.pageImage', { page: pageNo })}
              style={{ display: 'block', maxWidth: '100%', maxHeight: '75vh', marginTop: 8, border: '1px solid var(--border)' }}
            />
          )}
        </figure>
      )}
      {unparsed.size > 0 && <p role="note">{t('detail.partial', { count: unparsed.size })}</p>}
      {error && (
        <div role="alert" style={ALERT}>
          {error}
        </div>
      )}
      {detail && !isOffice(detail.head.media_type) && !loading && !error && blocks.length === 0 && !more && unparsed.size === 0 && (
        <p style={{ color: 'var(--muted)' }}>{t('detail.noText')}</p>
      )}
      <div style={{ fontSize: 13, whiteSpace: 'pre-wrap' }}>
        {blocks.map((b, i) => (
          <Fragment key={b.block_id}>
            {b.page !== undefined && b.page !== blocks[i - 1]?.page && (
              <div style={{ color: 'var(--muted)', fontSize: 12, marginTop: 8 }}>{t('detail.page', { page: b.page })}</div>
            )}
            <p
              id={`blk-${b.block_id}`}
              style={{
                margin: '4px 0',
                background: highlight === b.block_id ? 'var(--accent-soft, rgba(255,200,0,0.25))' : undefined,
              }}
            >
              {b.text}
            </p>
          </Fragment>
        ))}
      </div>
      {more && detail && (
        <button className="btn btn-ghost" onClick={() => void read(generation.current, detail, more)} disabled={loading}>
          {t('detail.loadMore')}
        </button>
      )}
    </section>
  )
}

type TFn = ReturnType<typeof useT<keyof typeof documentsMessages.en>>

function SourceLinks({ sources, onJump, t }: { sources: Source[]; onJump: (s: Source) => void; t: TFn }) {
  return (
    <>
      {sources.map((s, i) => (
        <button
          key={`${s.block_id}-${i}`}
          type="button"
          className="btn btn-ghost"
          onClick={() => onJump(s)}
          style={{ marginLeft: 6, fontSize: 12 }}
        >
          {s.page !== undefined ? t('check.page', { page: s.page }) : t('check.source')}
        </button>
      ))}
    </>
  )
}

function CheckResult({ report, t, onJump }: { report: CheckReport; t: TFn; onJump: (s: Source) => void }) {
  const section = (title: string, note: string | null, rows: Array<{ text: string; sources: Source[] }>) => {
    if (!rows.length) return null
    return (
      <div style={{ marginTop: 8 }}>
        <div style={{ fontWeight: 600, fontSize: 13 }}>
          {title}{note ? ` · ${note}` : ''}
        </div>
        {rows.map((row, i) => (
          <div key={i} style={{ fontSize: 13, margin: '4px 0' }}>
            {row.text}
            <SourceLinks sources={row.sources} onJump={onJump} t={t} />
          </div>
        ))}
      </div>
    )
  }
  return (
    <div style={{ marginTop: 8 }}>
      {section(t('check.summary'), null, report.summary)}
      {section(t('check.parties'), null, report.items.filter((i) => i.kind === 'party').map((i) => ({ text: i.text, sources: [i.source] })))}
      {section(t('check.deadlines'), null, report.items.filter((i) => i.kind === 'deadline').map((i) => ({ text: i.text, sources: [i.source] })))}
      {section(t('check.amounts'), null, report.items.filter((i) => i.kind === 'amount').map((i) => ({ text: i.text, sources: [i.source] })))}
      {section(t('check.conditions'), null, report.items.filter((i) => i.kind === 'condition').map((i) => ({ text: i.text, sources: [i.source] })))}
      {section(t('check.conflicts'), null, report.conflicts)}
      {report.missing.length > 0 && (
        <div style={{ marginTop: 8 }}>
          <div style={{ fontWeight: 600, fontSize: 13 }}>
            {t('check.missing')} · {t('check.missingNote')}
          </div>
          {report.missing.map((m) => (
            <div key={m} style={{ fontSize: 13, margin: '4px 0' }}>{m}</div>
          ))}
        </div>
      )}
      {report.cut && <p style={{ fontSize: 12, color: 'var(--muted)' }}>{t('check.cut')}</p>}
    </div>
  )
}
