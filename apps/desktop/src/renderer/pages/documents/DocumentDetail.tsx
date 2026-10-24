// One document's details and the text the OS read from its current revision
// (ADR-DOC-02 §3, §4): a page of blocks at a time, never all at once. The
// text is the user's document: it is shown, never logged.

import { Fragment, useEffect, useRef, useState } from 'react'
import type { components } from '@agent24/api-client'
import { describeError } from './importStore'
import { useT } from '../../i18n'
import { documentsMessages } from './Documents.i18n'

type Detail = components['schemas']['DocumentDetail']
type Block = components['schemas']['DocumentsTextBlock']

const ALERT = { color: '#e5534b', fontSize: 13, margin: '8px 0' } as const

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
    heading.current?.focus()
    void api.request({ op: 'get', documentId }).then((res) => {
      if (gen !== generation.current) return
      if (!res.ok) {
        setError(describeError(res))
        return
      }
      setDetail(res.data)
      void read(gen, res.data)
    })
    return () => {
      generation.current++
    }
    // `read` checks the generation through the ref; only the document matters.
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
