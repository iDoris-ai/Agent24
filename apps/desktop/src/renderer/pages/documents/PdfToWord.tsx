// L12-3 PDF → Word: 「转换为 Word」 on a PDF document's page. The documents OS
// converts with the a24-pdf2docx engine (rust/apps/agent24-documents/src/pdf_word.rs),
// downloaded on demand like LibreOffice (main/pdf-word-engine.ts); the new DOCX
// document is imported next to the PDF. A scan (no text layer) is refused.
// L12-4: the new document has one version, so its page offers no way out of
// the app; 「保存到下载」 here writes it to Downloads (named after the PDF, as
// .docx) and shows it in Finder.

import { useState } from 'react'
import { useT } from '../../i18n'
import { convert } from './compose'
import { documentsMessages } from './Documents.i18n'

type Api = Window['agent24']['documents']

type Note =
  | { kind: 'working' }
  | { kind: 'done'; documentId: string | null }
  | { kind: 'need' }
  | { kind: 'downloadFailed'; error: string }
  | { kind: 'scanned' }
  | { kind: 'failed'; error: string }

export default function PdfToWord(props: { api: Api; documentId: string; title: string; onAdded?: () => void }) {
  const { api, documentId, title, onAdded } = props
  const t = useT(documentsMessages)
  const [busy, setBusy] = useState(false)
  const [downloading, setDownloading] = useState(false)
  const [downloadText, setDownloadText] = useState<string | null>(null)
  const [note, setNote] = useState<Note | null>(null)
  const [saved, setSaved] = useState<{ ok: boolean; text: string } | null>(null)

  const run = async () => {
    setBusy(true)
    setNote({ kind: 'working' })
    setSaved(null)
    const out = await convert(api, documentId)
    setBusy(false)
    if (out.ok) {
      setNote({ kind: 'done', documentId: out.documentId })
      onAdded?.()
      return
    }
    if (out.engineMissing && out.engine === 'pdf2docx') {
      setNote({ kind: 'need' })
      return
    }
    if (out.code === 'unsupported_format') {
      setNote({ kind: 'scanned' })
      return
    }
    setNote({ kind: 'failed', error: out.message })
  }

  const save = async (wordId: string) => {
    setSaved(null)
    const out = await api.saveDownload(wordId, title)
    if (out.ok && out.docxName) {
      setSaved({ ok: true, text: t('word.saved', { name: out.docxName }) })
      void api.revealDownload(out.docxName)
    } else setSaved({ ok: false, text: t('word.saveFailed', { error: out.message ?? out.code ?? '' }) })
  }

  const download = async () => {
    setDownloading(true)
    setNote(null)
    setDownloadText('')
    const stop = api.onPdfWordEngineChanged((s) => {
      if (s.state !== 'installing') return
      const received = s.received ?? 0
      setDownloadText(s.total === undefined
        ? `${(received / (1024 * 1024)).toFixed(1)} MB`
        : t('word.downloading', { percent: Math.floor((received / s.total) * 100) }))
    })
    try {
      const status = await api.installPdfWordEngine()
      setDownloadText(null)
      if (status.state === 'ready') {
        await run()
        return
      }
      setNote({ kind: 'downloadFailed', error: status.error ?? status.state })
    } catch (error) {
      setDownloadText(null)
      setNote({ kind: 'downloadFailed', error: error instanceof Error ? error.message : String(error) })
    } finally {
      setDownloading(false)
      stop()
    }
  }

  return (
    <div style={{ margin: '8px 0' }}>
      <button type="button" className="btn btn-primary" disabled={busy || downloading} onClick={() => { void run() }}>
        {t('word.convert')}
      </button>
      {note?.kind === 'working' && <p role="status">{t('word.working')}</p>}
      {note?.kind === 'done' && <p role="status">{t('word.done')}</p>}
      {note?.kind === 'done' && note.documentId && (
        <button type="button" className="btn btn-ghost" onClick={() => { void save(note.documentId as string) }}>
          {t('word.save')}
        </button>
      )}
      {saved && <p role={saved.ok ? 'status' : 'alert'}>{saved.text}</p>}
      {note?.kind === 'need' && (
        <>
          <p>{t('word.needEngine')}</p>
          <button type="button" className="btn btn-primary" disabled={downloading} onClick={() => { void download() }}>
            {t('word.download')}
          </button>
        </>
      )}
      {downloadText ? <p role="status">{downloadText}</p> : null}
      {note?.kind === 'downloadFailed' && <p role="alert">{t('word.downloadFailed', { error: note.error })}</p>}
      {note?.kind === 'scanned' && <p role="alert">{t('word.scanned')}</p>}
      {note?.kind === 'failed' && <p role="alert">{t('word.failed', { error: note.error })}</p>}
    </div>
  )
}
