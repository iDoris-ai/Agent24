// L8 DOC-4: the check's card. A clean copy (revisions accepted, comments
// removed, author emptied; the original not changed) is made only from its
// buttons, after the user has read what the check found; the copy is saved
// where the user picks in the system save dialog, and a receipt is kept only
// once the file is on disk (main `doc-deliver.ts`). The words are the App's.

import { useState } from 'react'
import type { DeliverResult } from '../../../shared/ipc-types'
import type { DeliverCardData } from './doc-deliver'

function outcome(res: DeliverResult): string {
  if (res.ok) {
    const again = res.receipt.again ? '（同一版本再次保存）' : ''
    return `已保存到 ${res.savedTo}，已记下回执。${again}`
  }
  if (res.canceled) return '已取消，没有保存，也没有记回执。'
  if (res.stage === 'receipt') return `文件已保存到 ${res.savedTo ?? ''}，但回执没有记下（${res.code}）。`
  const step = res.stage === 'convert' ? '转成 PDF 没有成功' : res.stage === 'write' ? '写文件没有成功' : '生成副本没有成功'
  return `${step}（${res.code}），没有保存，也没有记回执。`
}

export function DeliverCard({ card }: { card: DeliverCardData }) {
  const [busy, setBusy] = useState<'docx' | 'pdf' | null>(null)
  const [note, setNote] = useState<string | null>(null)
  const save = (format: 'docx' | 'pdf'): void => {
    if (busy) return
    setBusy(format)
    setNote(null)
    void window.agent24.documents.deliver({ documentId: card.documentId, revision: card.revision, format, title: card.title })
      .then((res) => { setNote(outcome(res)) }, () => { setNote('生成副本没有成功（failed），没有保存，也没有记回执。') })
      .finally(() => { setBusy(null) })
  }
  if (!card.cleanable) {
    return (
      <div role="group" aria-label="发送前检查" className="extract-card">
        PDF 只能检查，不能清理；要清理请用原来的 Word 文档。
      </div>
    )
  }
  const order: Array<'docx' | 'pdf'> = card.suggested === 'pdf' ? ['pdf', 'docx'] : ['docx', 'pdf']
  const label = (format: 'docx' | 'pdf'): string => (busy === format ? '正在生成副本…' : format === 'pdf' ? '存成 PDF…' : '存成 Word 副本…')
  return (
    <div role="group" aria-label="发送前检查" className="extract-card">
      {order.map((format) => (
        <button key={format} type="button" className={`btn ${format === card.suggested ? 'btn-primary' : 'btn-ghost'} extract-card-source`} disabled={busy !== null} onClick={() => { save(format) }}>
          {label(format)}
        </button>
      ))}
      <p className="extract-card-muted">另存一份接受修订、删除批注、清空作者的副本，原文档不动。保存位置由你在系统保存框里选。</p>
      {note && <p className="extract-card-muted">{note}</p>}
    </div>
  )
}
