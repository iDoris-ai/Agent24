// One document's details and the text the OS read from its current revision
// (ADR-DOC-02 §3, §4): a page of blocks at a time, never all at once. The
// text is the user's document: it is shown, never logged. Above it, one
// page as the OS renders it, so the text can be checked against the page.

import { Fragment, useEffect, useRef, useState } from 'react'
import type { components } from '@agent24/api-client'
import { describeError } from './importStore'
import { useT } from '../../i18n'
import { documentsMessages } from './Documents.i18n'

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

export default function DocumentDetail({ documentId, onClose }: { documentId: string; onClose: () => void }) {
  const t = useT(documentsMessages)
  const api = window.agent24.documents
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
    heading.current?.focus()
    void api.request({ op: 'get', documentId }).then((res) => {
      if (gen !== generation.current) return
      if (!res.ok) {
        setError(describeError(res))
        return
      }
      setDetail(res.data)
      void read(gen, res.data)
      void show(res.data, 1)
    })
    return () => {
      generation.current++
      shown.current++
    }
    // `read` and `show` check their refs; only the document matters.
  }, [api, documentId])

  return (
    <section aria-label={t('detail.label')} style={{ border: '1px solid var(--border)', borderRadius: 6, padding: 12, margin: '12px 0' }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
        <div role="heading" aria-level={2} tabIndex={-1} ref={heading} style={{ fontWeight: 600 }}>
          {detail?.title ?? ''}
        </div>
        <button className="btn btn-ghost" onClick={onClose}>
          {t('detail.close')}
        </button>
      </div>
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
      {detail && (
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
      {detail && !loading && !error && blocks.length === 0 && !more && unparsed.size === 0 && (
        <p style={{ color: 'var(--muted)' }}>{t('detail.noText')}</p>
      )}
      <div style={{ fontSize: 13, whiteSpace: 'pre-wrap' }}>
        {blocks.map((b, i) => (
          <Fragment key={b.block_id}>
            {b.page !== undefined && b.page !== blocks[i - 1]?.page && (
              <div style={{ color: 'var(--muted)', fontSize: 12, marginTop: 8 }}>{t('detail.page', { page: b.page })}</div>
            )}
            <p style={{ margin: '4px 0' }}>{b.text}</p>
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
