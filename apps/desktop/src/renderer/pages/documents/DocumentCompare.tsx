// L6-S05: two documents, or two versions of one, compared by their text.
// One column of changes: a deleted word struck through, an added one marked,
// a line above it saying how many of each, and that only text is compared.
// The facts are the module's `changes`; nothing here is worked out again.

import { useEffect, useRef, useState } from 'react'
import { defineMessages, useT } from '../../i18n'
import type { CompareBlockRef, DocumentChange, DocumentComparison } from '../../../shared/ipc-types'
import { focusDocument } from './doc-focus'

type Api = Window['agent24']['documents']

export interface CompareSource { documentId: string; revision?: number; label: string }

export const compareMessages = defineMessages({
  en: {
    summary: '{total} changes: {removed} deleted, {added} added, {changed} edited, {unaligned} not aligned · text only',
    same: 'No difference in the text · text only', close: 'Close', more: 'Show the next changes', loading: 'Comparing…',
    unaligned: 'Could not be aligned (moved or reshaped); not counted as an edit',
    failed: 'Could not compare ({code}).', cross: 'A Word document and a PDF cannot be compared with each other.',
    left: 'Original: {at}', right: 'New: {at}', para: 'paragraph {n}', page: 'page {n}', title: 'Comparing {a} with {b}',
    removed: 'Deleted', added: 'Added', changed: 'Edited',
  },
  zh: {
    summary: '共 {total} 处改动：删 {removed} 处、加 {added} 处、改 {changed} 处、无法对齐 {unaligned} 处 · 只比较文字',
    same: '没有文字差异 · 只比较文字', close: '关闭对比', more: '再看后面的改动', loading: '正在对比…',
    unaligned: '无法对齐（可能是整段挪了位或改了结构），不算作改动',
    failed: '没能对比（{code}）。', cross: 'Word 文档和 PDF 不能互相对比。',
    left: '原文：{at}', right: '新版：{at}', para: '第 {n} 段', page: '第 {n} 页', title: '{a} 与 {b} 的对比',
    removed: '删除', added: '新增', changed: '修改',
  },
  th: {
    summary: 'ทั้งหมด {total} จุด: ลบ {removed}, เพิ่ม {added}, แก้ {changed}, จัดแนวไม่ได้ {unaligned} · เทียบเฉพาะข้อความ',
    same: 'ข้อความไม่ต่างกัน · เทียบเฉพาะข้อความ', close: 'ปิด', more: 'ดูการเปลี่ยนแปลงถัดไป', loading: 'กำลังเปรียบเทียบ…',
    unaligned: 'จัดแนวไม่ได้ (อาจย้ายหรือเปลี่ยนโครงสร้าง) ไม่นับเป็นการแก้',
    failed: 'เปรียบเทียบไม่ได้ ({code})', cross: 'เอกสาร Word กับ PDF เปรียบเทียบกันไม่ได้',
    left: 'ต้นฉบับ: {at}', right: 'ใหม่: {at}', para: 'ย่อหน้า {n}', page: 'หน้า {n}', title: 'เปรียบเทียบ {a} กับ {b}',
    removed: 'ลบ', added: 'เพิ่ม', changed: 'แก้',
  },
})

const isBlock = (r: CompareBlockRef): r is { block_id: string; page: number } => 'block_id' in r

function Marked({ change }: { change: DocumentChange }) {
  if (change.kind === 'removed') return <del className="compare-del">{change.left?.text}</del>
  if (change.kind === 'added') return <ins className="compare-ins">{change.right?.text}</ins>
  if (change.kind === 'unaligned') return <span>{(change.left ?? change.right)?.text}</span>
  return (
    <>
      {change.spans.map((s, i) => s.op === 'delete' ? <del key={i} className="compare-del">{s.text}</del>
        : s.op === 'insert' ? <ins key={i} className="compare-ins">{s.text}</ins> : <span key={i}>{s.text}</span>)}
    </>
  )
}

export default function DocumentCompare({ api, left, right, focusId, onClose }: {
  api: Api
  left: CompareSource
  right: CompareSource
  /** A change to scroll to (a line of the summary in the conversation). */
  focusId?: string
  onClose: () => void
}) {
  const t = useT(compareMessages)
  const [data, setData] = useState<DocumentComparison | null>(null)
  const [changes, setChanges] = useState<DocumentChange[]>([])
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(true)
  const asked = useRef(0)

  const load = async (offset?: number) => {
    const me = ++asked.current
    setBusy(true)
    const sides = { left: { documentId: left.documentId, revision: left.revision }, right: { documentId: right.documentId, revision: right.revision } }
    const clean = (s: { documentId: string; revision?: number }) => (s.revision === undefined ? { documentId: s.documentId } : s)
    const res = await api.request({ op: 'compare', left: clean(sides.left), right: clean(sides.right), ...(offset ? { offset } : {}) }).catch(() => null)
    if (me !== asked.current) return
    setBusy(false)
    if (res && res.ok) {
      setData(res.data)
      setChanges((prev) => (offset ? [...prev, ...res.data.changes] : res.data.changes))
      setError(null)
    } else setError(!res ? t('failed', { code: 'backend_unreachable' }) : res.error.code === 'unsupported_format' ? t('cross') : t('failed', { code: res.error.code }))
  }
  useEffect(() => { void load() }, [api, left.documentId, left.revision, right.documentId, right.revision])
  useEffect(() => { if (focusId) document.getElementById(`compare-${focusId}`)?.scrollIntoView?.({ block: 'center' }) }, [focusId, changes.length])

  const where = (key: 'left' | 'right', side: NonNullable<DocumentChange['left']>, documentId: string) => {
    const ref = side.block_ref
    const at = isBlock(ref) ? t('page', { n: String(ref.page) }) : t('para', { n: String(ref.paragraph_index + 1) })
    const label = t(key, { at })
    return isBlock(ref)
      ? <button type="button" className="btn btn-ghost" onClick={() => focusDocument({ documentId, page: ref.page, blockId: ref.block_id })}>{label}</button>
      : <span className="compare-where">{label}</span>
  }

  const s = data?.summary
  return (
    <section className="compare-view" aria-label={t('title', { a: left.label, b: right.label })}>
      <h3 style={{ fontSize: 14, margin: '4px 0' }}>{t('title', { a: left.label, b: right.label })}</h3>
      {error && <p role="alert">{error}</p>}
      {busy && !data && <p role="status">{t('loading')}</p>}
      {data && s && <p role="status">{data.identical ? t('same') : t('summary', { total: String(s.total), removed: String(s.removed), added: String(s.added), changed: String(s.changed), unaligned: String(s.unaligned) })}</p>}
      <ol className="compare-list">
        {changes.map((c) => (
          <li key={c.id} id={`compare-${c.id}`} className={`compare-change compare-${c.kind}`}>
            <div><Marked change={c} /></div>
            {c.kind === 'unaligned' && <div className="compare-note">{t('unaligned')}</div>}
            <div className="compare-wheres">
              {c.left && where('left', c.left, data?.left.document_id ?? left.documentId)}
              {c.right && where('right', c.right, data?.right.document_id ?? right.documentId)}
            </div>
          </li>
        ))}
      </ol>
      {data?.truncated && data.next_offset !== null && (
        <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => void load(data.next_offset ?? undefined)}>{t('more')}</button>
      )}
      <button type="button" className="btn btn-ghost" onClick={onClose}>{t('close')}</button>
    </section>
  )
}
