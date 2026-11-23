// L6-F5: a Word document's versions on its page: each with its time and
// whether it is the original or a saved change, the current one marked.
// 「打开」 saves that version's file to Downloads and shows it in Finder (to
// open in Word or WPS); 「恢复这个版本」 saves its content as a new version over
// the version shown, so history is never rewritten and nothing anyone saved
// meanwhile is written over (the module answers revision_conflict).

import { useEffect, useState } from 'react'
import { defineMessages, useT } from '../../i18n'
import type { DocumentVersions as Versions } from '../../../shared/ipc-types'
import { localTime } from '../chat/delete-card'

type Api = Window['agent24']['documents']

export const versionMessages = defineMessages({
  en: {
    title: 'Versions', current: 'current', original: 'original', change: 'saved change', version: 'Version {n}',
    open: 'Open', restore: 'Restore this version', unread: 'Could not read the versions.', busy: 'Working…',
    restored: 'Restored: version {from} is now version {now}, the current one. Every version is kept.',
    same: 'Version {from} is the same as the current one; nothing was saved.',
    conflict: 'The document has a newer version since this list was read; nothing was restored. Look again and retry.',
    failed: 'Could not restore; the document is unchanged ({code}).', saved: 'Saved: Downloads/{name}', reveal: 'Show in Finder',
    saveFailed: 'Could not open: {error}',
  },
  zh: {
    title: '版本', current: '当前', original: '原件', change: '保存的改动', version: '第 {n} 版',
    open: '打开', restore: '恢复这个版本', unread: '没能读取版本列表。', busy: '正在处理…',
    restored: '已恢复：第 {from} 版的内容现在是第 {now} 版（当前版本），所有版本都保留。',
    same: '第 {from} 版和当前版本内容相同，没有新增版本。',
    conflict: '读取列表之后这份文档又有了新版本，这次没有恢复。请看一下最新的列表再试。',
    failed: '没能恢复，文档没有改动（{code}）。', saved: '已保存到：下载/{name}', reveal: '在 Finder 中显示',
    saveFailed: '没能打开：{error}',
  },
  th: {
    title: 'เวอร์ชัน', current: 'ปัจจุบัน', original: 'ต้นฉบับ', change: 'การแก้ไขที่บันทึก', version: 'เวอร์ชัน {n}',
    open: 'เปิด', restore: 'กู้คืนเวอร์ชันนี้', unread: 'อ่านรายการเวอร์ชันไม่ได้', busy: 'กำลังทำงาน…',
    restored: 'กู้คืนแล้ว: เวอร์ชัน {from} ตอนนี้เป็นเวอร์ชัน {now} (ปัจจุบัน) ทุกเวอร์ชันยังอยู่',
    same: 'เวอร์ชัน {from} เหมือนเวอร์ชันปัจจุบัน ไม่ได้บันทึกอะไร',
    conflict: 'เอกสารมีเวอร์ชันใหม่กว่าตั้งแต่อ่านรายการนี้ จึงไม่ได้กู้คืน ลองดูรายการล่าสุดแล้วลองใหม่',
    failed: 'กู้คืนไม่ได้ เอกสารไม่เปลี่ยน ({code})', saved: 'บันทึกแล้ว: ดาวน์โหลด/{name}', reveal: 'แสดงใน Finder',
    saveFailed: 'เปิดไม่ได้: {error}',
  },
})

/** 「合同（第 1 版）」: the name an opened version is saved under. */
export function versionFileTitle(title: string, revision: number): string {
  return `${title.replace(/\.(docx?|xlsx?|pptx?|pdf)$/i, '')}（第 ${revision} 版）`
}

export default function DocumentVersions({ api, documentId, title, onChanged }: {
  api: Api
  documentId: string
  title: string
  onChanged?: () => void
}) {
  const t = useT(versionMessages)
  const [versions, setVersions] = useState<Versions | null>(null)
  const [unread, setUnread] = useState(false)
  const [busy, setBusy] = useState(false)
  const [note, setNote] = useState<{ ok: boolean; text: string; file?: string } | null>(null)

  const load = async () => {
    const res = await api.request({ op: 'revisions', documentId }).catch(() => null)
    if (res && res.ok) { setVersions(res.data); setUnread(false) } else setUnread(true)
  }
  useEffect(() => { void load() }, [api, documentId]) // eslint-disable-line react-hooks/exhaustive-deps

  const restore = async (from: number, head: number) => {
    setBusy(true)
    setNote(null)
    try {
      const res = await api.request({ op: 'restoreRevision', documentId, revision: from, expectRevision: head }).catch(() => null)
      if (!res) setNote({ ok: false, text: t('failed', { code: 'backend_unreachable' }) })
      else if (!res.ok) setNote({ ok: false, text: res.error.code === 'revision_conflict' ? t('conflict') : t('failed', { code: res.error.code }) })
      else {
        setNote({ ok: true, text: res.data.committed ? t('restored', { from: String(from), now: String(res.data.revision) }) : t('same', { from: String(from) }) })
        if (res.data.committed) onChanged?.()
      }
      await load()
    } finally {
      setBusy(false)
    }
  }

  const open = async (revision: number) => {
    setNote(null)
    const out = await api.saveDownload(documentId, versionFileTitle(title, revision), revision)
    if (out.ok && out.docxName) {
      setNote({ ok: true, text: t('saved', { name: out.docxName }), file: out.docxName })
      void api.revealDownload(out.docxName)
    } else setNote({ ok: false, text: t('saveFailed', { error: out.message ?? out.code ?? '' }) })
  }

  if (unread) return <p role="alert">{t('unread')}</p>
  if (!versions || versions.revisions.length < 2) return null
  const head = versions.head_revision
  return (
    <section aria-label={t('title')} style={{ marginTop: 12 }}>
      <h3 style={{ fontSize: 14, margin: '4px 0' }}>{t('title')}</h3>
      <ul style={{ listStyle: 'none', padding: 0, margin: 0 }}>
        {versions.revisions.map((r) => (
          <li key={r.revision} style={{ display: 'flex', gap: 8, alignItems: 'center', margin: '4px 0' }}>
            <span>{t('version', { n: String(r.revision) })}</span>
            <span style={{ color: 'var(--muted)' }}>{localTime(r.created_at)} · {r.origin === 'import' ? t('original') : t('change')}</span>
            {r.revision === head && <strong>{t('current')}</strong>}
            <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => void open(r.revision)}>{t('open')}</button>
            {r.revision !== head && (
              <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => void restore(r.revision, head)}>{t('restore')}</button>
            )}
          </li>
        ))}
      </ul>
      {busy && <p role="status">{t('busy')}</p>}
      {note && <p role={note.ok ? 'status' : 'alert'}>{note.text}</p>}
    </section>
  )
}
