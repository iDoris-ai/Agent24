// KB-SOP-01b-2 (#1669): local knowledge-entry drafts on the Knowledge page.
// Main owns the files (main/knowledge-entry-ipc.ts). This panel only shows
// them, edits the title, kind and body, and can redact or withdraw one.
// Submitting for review is KB-SOP-01c and stays disabled.

import { createContext, useContext, useEffect, useRef, useState, useSyncExternalStore, type ReactNode } from 'react'
import type {
  KnowledgeEntryDraftSaved,
  KnowledgeEntryDraftSummary,
  KnowledgeEntryDraftView,
  KnowledgeEntryFinding,
  KnowledgeEntryKind,
  KnowledgeEntryModelOutcome,
} from '../../shared/ipc-types'
import { useLeaveGuard } from '../capabilities/providers'
import { defineMessages, useT, type T } from '../i18n'

const draftMessages = defineMessages({
  en: {
    'list.aria': 'Knowledge entry drafts',
    'bar.count': 'Knowledge entry drafts: {n}',
    'bar.view': 'View',
    'kind.sop': 'Steps',
    'kind.faq': 'Q&A',
    'kind.note': 'Note',
    'badge.changed': 'Needs a new target library',
    'badge.none': 'Knowledge is off',
    'badge.findings': 'May contain sensitive content',
    'field.kind': 'Type',
    'field.title': 'Title',
    'field.body': 'Body',
    'detail.source': 'From messages {from}–{to} of the conversation',
    'detail.missing': 'This draft is no longer here.',
    'detail.targetChanged': 'Needs a new target library: Knowledge is no longer bound to the knowledge base this draft was created for, and this draft will not switch over on its own.',
    'detail.targetNone': 'Knowledge is off or not bound to a knowledge base.',
    'detail.findings': 'May contain sensitive content: {categories}. Edit it before submitting, or choose “Redact”.',
    'finding.private_key': 'private key',
    'finding.credential': 'credential',
    'finding.email': 'email',
    'finding.phone': 'phone',
    'finding.id_card': 'ID number',
    'finding.bank_card': 'bank card',
    'finding.path': 'path',
    'finding.internal_address': 'internal address',
    'model.no_model': 'No on-device model is available. An empty template is ready for you to fill in.',
    'model.needs_confirmation': 'The on-device model needs you to confirm its resource use first. An empty template is ready. You can confirm in the conversation and try again.',
    'model.busy': 'The on-device model is busy. An empty template is ready.',
    'model.failed': 'The on-device model did not answer. An empty template is ready.',
    'model.unusable': 'The on-device model’s answer could not be used. An empty template is ready.',
    'model.held_back': 'Some private details in the chosen messages could not be hidden automatically, so they were not given to the on-device model. An empty template is ready.',
    'detail.hints': 'The model suggests another look (a hint only; it does not affect submitting): {hints}',
    'action.save': 'Save',
    'action.redact': 'Redact',
    'action.withdraw': 'Withdraw',
    'action.submit': 'Submit for review',
    'action.back': 'Back to list',
    'action.close': 'Close',
    'save.invalid': 'Could not save: the title cannot be empty, and the body cannot exceed 64 KiB.',
    'save.not_saved': 'The draft could not be saved. Try again in a moment.',
    'redact.too_long': 'Redacting would make the body longer than 64 KiB. Shorten it by hand first.',
    'withdraw.confirm': 'Withdrawing deletes this draft from this machine. It cannot be restored. Withdraw it?',
    'withdraw.failed': 'Could not withdraw. Try again in a moment.',
    'submit.changed': 'Needs a new target library',
    'submit.findings': 'Handle the possible sensitive content first',
    'submit.later': 'Submitting for review will be available in a later version',
    'confirm.discard': 'There are unsaved changes. Discard them?',
    'join.sep': ', ',
  },
  zh: {
    'list.aria': '知识条目草稿',
    'bar.count': '知识条目草稿：{n} 份',
    'bar.view': '查看',
    'kind.sop': '操作步骤',
    'kind.faq': '问答',
    'kind.note': '笔记',
    'badge.changed': '需要重新选目标库',
    'badge.none': 'Knowledge 未开启',
    'badge.findings': '含疑似敏感内容',
    'field.kind': '类型',
    'field.title': '标题',
    'field.body': '正文',
    'detail.source': '来自对话第 {from}–{to} 条消息',
    'detail.missing': '这份草稿已经不在了。',
    'detail.targetChanged': '需要重新选目标库：Knowledge 现在绑定的不是建草稿时的知识库，这份草稿不会自动改投。',
    'detail.targetNone': 'Knowledge 未开启或未绑定知识库。',
    'detail.findings': '含疑似敏感内容：{categories}。提交前请删改，或点「一键脱敏」。',
    'finding.private_key': '私钥',
    'finding.credential': '凭据',
    'finding.email': '邮箱',
    'finding.phone': '电话',
    'finding.id_card': '证件号',
    'finding.bank_card': '银行卡号',
    'finding.path': '路径',
    'finding.internal_address': '内网地址',
    'model.no_model': '没有可用的本机模型，已给出空模板，请自己填写。',
    'model.needs_confirmation': '本机模型需要先确认资源占用，已给出空模板。可以在对话里确认后再试。',
    'model.busy': '本机模型正忙，已给出空模板。',
    'model.failed': '本机模型没有答复，已给出空模板。',
    'model.unusable': '本机模型的答复无法使用，已给出空模板。',
    'model.held_back': '所选消息里有无法自动脱敏的内容，没有交给本机模型，已给出空模板。',
    'detail.hints': '模型提示再看一眼（只是提示，不影响提交）：{hints}',
    'action.save': '保存',
    'action.redact': '一键脱敏',
    'action.withdraw': '撤回',
    'action.submit': '提交审阅',
    'action.back': '返回列表',
    'action.close': '关闭',
    'save.invalid': '没能保存：标题不能为空，正文不能超过 64 KiB。',
    'save.not_saved': '草稿没能保存，请稍后再试。',
    'redact.too_long': '脱敏后正文会超过 64 KiB，请先手工删减。',
    'withdraw.confirm': '撤回后这份草稿会从本机删除，不能恢复。确定撤回？',
    'withdraw.failed': '没能撤回，请稍后再试。',
    'submit.changed': '需要重新选目标库',
    'submit.findings': '请先处理疑似敏感内容',
    'submit.later': '提交审阅将在后续版本提供',
    'confirm.discard': '有未保存的修改，确定放弃？',
    'join.sep': '、',
  },
  th: {
    'list.aria': 'ฉบับร่างรายการความรู้',
    'bar.count': 'ฉบับร่างรายการความรู้: {n} ฉบับ',
    'bar.view': 'ดู',
    'kind.sop': 'ขั้นตอน',
    'kind.faq': 'ถามตอบ',
    'kind.note': 'บันทึก',
    'badge.changed': 'ต้องเลือกคลังเป้าหมายใหม่',
    'badge.none': 'Knowledge ปิดอยู่',
    'badge.findings': 'อาจมีเนื้อหาที่ละเอียดอ่อน',
    'field.kind': 'ประเภท',
    'field.title': 'ชื่อเรื่อง',
    'field.body': 'เนื้อหา',
    'detail.source': 'จากข้อความที่ {from}–{to} ของบทสนทนา',
    'detail.missing': 'ไม่พบฉบับร่างนี้แล้ว',
    'detail.targetChanged': 'ต้องเลือกคลังเป้าหมายใหม่: Knowledge ที่เชื่อมอยู่ตอนนี้ไม่ใช่คลังที่ใช้ตอนสร้างฉบับร่าง ฉบับร่างนี้จะไม่ถูกย้ายให้เอง',
    'detail.targetNone': 'Knowledge ปิดอยู่หรือยังไม่ได้เชื่อมกับฐานความรู้',
    'detail.findings': 'อาจมีเนื้อหาที่ละเอียดอ่อน: {categories} โปรดแก้ไขก่อนส่ง หรือกด «ปิดบังข้อมูล»',
    'finding.private_key': 'กุญแจส่วนตัว',
    'finding.credential': 'ข้อมูลรับรอง',
    'finding.email': 'อีเมล',
    'finding.phone': 'โทรศัพท์',
    'finding.id_card': 'เลขบัตร',
    'finding.bank_card': 'เลขบัตรธนาคาร',
    'finding.path': 'พาธ',
    'finding.internal_address': 'ที่อยู่ในเครือข่ายภายใน',
    'model.no_model': 'ไม่มีโมเดลในเครื่องที่ใช้ได้ จึงให้เทมเพลตว่างไว้ โปรดกรอกเอง',
    'model.needs_confirmation': 'โมเดลในเครื่องต้องยืนยันการใช้ทรัพยากรก่อน จึงให้เทมเพลตว่างไว้ สามารถยืนยันในบทสนทนาแล้วลองใหม่',
    'model.busy': 'โมเดลในเครื่องไม่ว่าง จึงให้เทมเพลตว่างไว้',
    'model.failed': 'โมเดลในเครื่องไม่ตอบ จึงให้เทมเพลตว่างไว้',
    'model.unusable': 'คำตอบของโมเดลในเครื่องใช้ไม่ได้ จึงให้เทมเพลตว่างไว้',
    'model.held_back': 'ข้อความที่เลือกมีข้อมูลส่วนตัวที่ปิดบังอัตโนมัติไม่ได้ จึงไม่ได้ส่งให้โมเดลในเครื่อง และให้เทมเพลตว่างไว้',
    'detail.hints': 'โมเดลแนะนำให้ดูอีกครั้ง (เป็นเพียงคำแนะนำ ไม่กระทบการส่ง): {hints}',
    'action.save': 'บันทึก',
    'action.redact': 'ปิดบังข้อมูล',
    'action.withdraw': 'ถอน',
    'action.submit': 'ส่งให้ตรวจ',
    'action.back': 'กลับไปที่รายการ',
    'action.close': 'ปิด',
    'save.invalid': 'บันทึกไม่สำเร็จ: ชื่อเรื่องต้องไม่ว่าง และเนื้อหาต้องไม่เกิน 64 KiB',
    'save.not_saved': 'บันทึกฉบับร่างไม่สำเร็จ โปรดลองอีกครั้งในอีกสักครู่',
    'redact.too_long': 'หากปิดบังข้อมูล เนื้อหาจะยาวเกิน 64 KiB โปรดตัดให้สั้นลงเองก่อน',
    'withdraw.confirm': 'การถอนจะลบฉบับร่างนี้ออกจากเครื่องนี้ และกู้คืนไม่ได้ ยืนยันที่จะถอนหรือไม่',
    'withdraw.failed': 'ถอนไม่สำเร็จ โปรดลองอีกครั้งในอีกสักครู่',
    'submit.changed': 'ต้องเลือกคลังเป้าหมายใหม่',
    'submit.findings': 'โปรดจัดการเนื้อหาที่อาจละเอียดอ่อนก่อน',
    'submit.later': 'การส่งให้ตรวจจะมีในเวอร์ชันถัดไป',
    'confirm.discard': 'มีการแก้ไขที่ยังไม่บันทึก ยืนยันที่จะทิ้งหรือไม่',
    'join.sep': ', ',
  },
})

type Copy = T<keyof typeof draftMessages.en>
type CreatedNote = { model: KnowledgeEntryModelOutcome; hints: string[] }
type PanelState =
  | null
  | { view: 'list' }
  | { view: 'detail'; id: string; created?: CreatedNote }

const FINDING_KEY: Record<KnowledgeEntryFinding, 'finding.private_key' | 'finding.credential' | 'finding.email' | 'finding.phone' | 'finding.id_card' | 'finding.bank_card' | 'finding.path' | 'finding.internal_address'> = {
  private_key: 'finding.private_key',
  credential: 'finding.credential',
  email: 'finding.email',
  phone: 'finding.phone',
  id_card: 'finding.id_card',
  bank_card: 'finding.bank_card',
  path: 'finding.path',
  internal_address: 'finding.internal_address',
}

let panel: PanelState = null
const listeners = new Set<() => void>()
/** Set by the open draft while it has unsaved edits: asks the user, true when leaving is fine. */
let leaveGuard: (() => boolean) | null = null

function publish(next: PanelState): void {
  panel = next
  for (const listener of listeners) listener()
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  return () => { listeners.delete(listener) }
}

/**
 * Opens one draft. `extra` is the model step from the create that just
 * finished; only that opening shows it. Another draft with unsaved edits is
 * open: the user is asked first, and on no it stays (the new draft is in the
 * list).
 */
export function openDraft(id: string, extra?: CreatedNote): void {
  if (panel?.view === 'detail' && panel.id !== id && leaveGuard && !leaveGuard()) return
  publish({ view: 'detail', id, created: extra })
}

export function openDraftList(): void {
  publish({ view: 'list' })
}

export function closeDrafts(): void {
  publish(null)
}

export function useDraftsPanel(): PanelState {
  return useSyncExternalStore(subscribe, () => panel)
}

function invoke<T>(run: () => Promise<T>): Promise<T> {
  try {
    return Promise.resolve(run())
  } catch (err) {
    return Promise.reject(err)
  }
}

function isFinding(value: string): value is KnowledgeEntryFinding {
  return Object.hasOwn(FINDING_KEY, value)
}

function isKind(value: unknown): value is KnowledgeEntryKind {
  return value === 'sop' || value === 'faq' || value === 'note'
}

function isDraftView(value: unknown): value is KnowledgeEntryDraftView {
  if (typeof value !== 'object' || value === null) return false
  const v = value as Partial<KnowledgeEntryDraftView>
  return typeof v.id === 'string'
    && isKind(v.kind)
    && typeof v.title === 'string'
    && typeof v.body === 'string'
    && (v.target === 'same' || v.target === 'changed' || v.target === 'none')
    && Array.isArray(v.findings)
    && v.findings.every((item) => typeof item === 'string')
    && typeof v.message_from === 'number'
    && typeof v.message_to === 'number'
}

function isSummary(value: unknown): value is KnowledgeEntryDraftSummary {
  if (typeof value !== 'object' || value === null) return false
  const v = value as Partial<KnowledgeEntryDraftSummary>
  return typeof v.id === 'string'
    && isKind(v.kind)
    && typeof v.title === 'string'
    && (v.target === 'same' || v.target === 'changed' || v.target === 'none')
    && Array.isArray(v.findings)
    && v.findings.every((item) => typeof item === 'string')
}

function isSaved(value: unknown): value is KnowledgeEntryDraftSaved {
  if (typeof value !== 'object' || value === null) return false
  const v = value as { ok?: unknown; reason?: unknown; draft?: unknown }
  if (v.ok === true) return isDraftView(v.draft)
  return v.ok === false && (v.reason === 'not_found' || v.reason === 'invalid' || v.reason === 'not_saved' || v.reason === 'too_long')
}

function kindLabel(t: Copy, kind: KnowledgeEntryKind): string {
  if (kind === 'sop') return t('kind.sop')
  if (kind === 'faq') return t('kind.faq')
  return t('kind.note')
}

function itemLabel(t: Copy, item: KnowledgeEntryDraftSummary): string {
  const badges: string[] = []
  if (item.target === 'changed') badges.push(t('badge.changed'))
  if (item.target === 'none') badges.push(t('badge.none'))
  if (item.findings.length > 0) badges.push(t('badge.findings'))
  const kind = kindLabel(t, item.kind)
  return badges.length > 0 ? `${item.title} ${kind} ${badges.join(' ')}` : `${item.title} ${kind}`
}

function modelLine(t: Copy, model: KnowledgeEntryModelOutcome): string | null {
  switch (model) {
    case 'drafted': return null
    case 'no_model': return t('model.no_model')
    case 'needs_confirmation': return t('model.needs_confirmation')
    case 'busy': return t('model.busy')
    case 'failed': return t('model.failed')
    case 'unusable': return t('model.unusable')
    case 'held_back': return t('model.held_back')
  }
}

function Shell({ children }: { children?: ReactNode }): JSX.Element {
  const t = useT(draftMessages)
  const hidden = useContext(HiddenContext)
  // `.knowledge-confirm` sets `display: flex`, which beats the `[hidden]`
  // default, so `hidden` alone leaves the panel on screen and focusable.
  // The inline `display: none` is what takes it away.
  return (
    <section
      className="knowledge-confirm"
      role="dialog"
      aria-modal="false"
      aria-label={t('list.aria')}
      hidden={hidden}
      style={hidden ? { display: 'none' } : undefined}
    >
      {children}
    </section>
  )
}

function DraftList(): JSX.Element {
  const t = useT(draftMessages)
  const [items, setItems] = useState<KnowledgeEntryDraftSummary[] | null>(null)
  useEffect(() => {
    let live = true
    const list = window.agent24?.knowledgeEntry?.listDrafts
    if (typeof list !== 'function') {
      setItems([])
      return
    }
    invoke(() => list()).then(
      (value) => { if (live) setItems(Array.isArray(value) ? value.filter(isSummary) : []) },
      () => { if (live) setItems([]) },
    )
    return () => { live = false }
  }, [])
  if (items === null) return <Shell />
  return (
    <Shell>
      <p className="knowledge-confirm-count">{t('bar.count', { n: items.length })}</p>
      <ul className="knowledge-confirm-titles">
        {items.map((item) => (
          <li key={item.id}>
            <button type="button" onClick={() => openDraft(item.id)}>{itemLabel(t, item)}</button>
          </li>
        ))}
      </ul>
      <button className="btn" type="button" onClick={() => closeDrafts()}>{t('action.close')}</button>
    </Shell>
  )
}

function DraftDetail({ id, created }: { id: string; created?: CreatedNote }): JSX.Element {
  const t = useT(draftMessages)
  const [draft, setDraft] = useState<KnowledgeEntryDraftView | null | undefined>(undefined)
  const [kind, setKind] = useState<KnowledgeEntryKind>('note')
  const [title, setTitle] = useState('')
  const [body, setBody] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const flight = useRef(false)
  const mounted = useRef(true)
  // Set again on every setup: StrictMode runs setup, cleanup, setup.
  useEffect(() => {
    mounted.current = true
    return () => { mounted.current = false }
  }, [])

  useEffect(() => {
    let live = true
    const get = window.agent24?.knowledgeEntry?.getDraft
    if (typeof get !== 'function') {
      setDraft(null)
      return
    }
    invoke(() => get(id)).then(
      (value) => {
        if (!live) return
        if (!isDraftView(value)) { setDraft(null); return }
        setKind(value.kind)
        setTitle(value.title)
        setBody(value.body)
        setError(null)
        setDraft(value)
      },
      () => { if (live) setDraft(null) },
    )
    return () => { live = false }
  }, [id])

  const applySaved = (value: unknown, fallback: string): void => {
    if (!isSaved(value)) { setError(fallback); return }
    if (!value.ok) {
      setError(value.reason === 'invalid' ? t('save.invalid')
        : value.reason === 'not_found' ? t('detail.missing')
          : value.reason === 'too_long' ? t('redact.too_long')
            : t('save.not_saved'))
      return
    }
    setKind(value.draft.kind)
    setTitle(value.draft.title)
    setBody(value.draft.body)
    setDraft(value.draft)
    setError(null)
  }

  const run = (task: () => Promise<unknown>, done: (value: unknown) => void, fail: () => void): void => {
    if (flight.current) return
    flight.current = true
    setBusy(true)
    setError(null)
    invoke(task).then(
      (value) => {
        flight.current = false
        if (!mounted.current) return
        setBusy(false)
        done(value)
      },
      () => {
        flight.current = false
        if (!mounted.current) return
        setBusy(false)
        fail()
      },
    )
  }

  const dirty = draft ? kind !== draft.kind || title !== draft.title || body !== draft.body : false
  // Someone outside the panel (a new draft opening) asks before these edits go.
  useEffect(() => {
    leaveGuard = dirty ? () => window.confirm(t('confirm.discard')) : null
    return () => { leaveGuard = null }
  }, [dirty, t])
  // M4 A7: going to another page asks too, once, with the same words.
  useLeaveGuard(dirty, t('confirm.discard'))

  if (draft === undefined) return <Shell />
  if (draft === null) {
    return (
      <Shell>
        <p role="status">{t('detail.missing')}</p>
        <button className="btn" type="button" onClick={() => openDraftList()}>{t('action.back')}</button>
        <button className="btn" type="button" onClick={() => closeDrafts()}>{t('action.close')}</button>
      </Shell>
    )
  }

  const categories = draft.findings.filter(isFinding).map((finding) => t(FINDING_KEY[finding])).join(t('join.sep'))
  const createdModel = created ? modelLine(t, created.model) : null
  const createdHints = created && created.hints.length > 0
    ? t('detail.hints', { hints: created.hints.join(t('join.sep')) })
    : null
  const submitReason = draft.target === 'changed'
    ? t('submit.changed')
    : draft.findings.length > 0
      ? t('submit.findings')
      : t('submit.later')

  const leave = (go: () => void): void => {
    if (dirty && !window.confirm(t('confirm.discard'))) return
    go()
  }

  const onSave = (): void => {
    if (busy || !dirty) return
    const current = draft
    run(
      () => window.agent24.knowledgeEntry.updateDraft(current.id, { kind, title, body }),
      (value) => applySaved(value, t('save.not_saved')),
      () => setError(t('save.not_saved')),
    )
  }

  const onRedact = (): void => {
    if (busy) return
    // Main redacts what it has stored: edits not saved yet would be lost.
    if (dirty && !window.confirm(t('confirm.discard'))) return
    const current = draft
    run(
      () => window.agent24.knowledgeEntry.redactDraft(current.id),
      (value) => applySaved(value, t('save.not_saved')),
      () => setError(t('save.not_saved')),
    )
  }

  const onWithdraw = (): void => {
    if (busy) return
    if (!window.confirm(t('withdraw.confirm'))) return
    const current = draft
    run(
      () => window.agent24.knowledgeEntry.withdrawDraft(current.id),
      (value) => {
        if (value === true) openDraftList()
        else setError(t('withdraw.failed'))
      },
      () => setError(t('withdraw.failed')),
    )
  }

  return (
    <Shell>
      <label htmlFor="knowledge-entry-draft-kind">{t('field.kind')}</label>
      <select id="knowledge-entry-draft-kind" value={kind} disabled={busy} onChange={(e) => { if (isKind(e.target.value)) setKind(e.target.value) }}>
        <option value="sop">{t('kind.sop')}</option>
        <option value="faq">{t('kind.faq')}</option>
        <option value="note">{t('kind.note')}</option>
      </select>
      <label htmlFor="knowledge-entry-draft-title">{t('field.title')}</label>
      <input id="knowledge-entry-draft-title" type="text" value={title} disabled={busy} onChange={(e) => setTitle(e.target.value)} />
      <label htmlFor="knowledge-entry-draft-body">{t('field.body')}</label>
      <textarea id="knowledge-entry-draft-body" rows={12} value={body} disabled={busy} onChange={(e) => setBody(e.target.value)} />
      <p>{t('detail.source', { from: draft.message_from + 1, to: draft.message_to + 1 })}</p>
      {draft.target === 'changed' && <p>{t('detail.targetChanged')}</p>}
      {draft.target === 'none' && <p>{t('detail.targetNone')}</p>}
      {draft.findings.length > 0 && <p>{t('detail.findings', { categories })}</p>}
      {createdModel && <p>{createdModel}</p>}
      {createdHints && <p>{createdHints}</p>}
      {error && <p role="status">{error}</p>}
      <button className="btn btn-primary" type="button" disabled={busy || !dirty} onClick={onSave}>{t('action.save')}</button>
      {draft.findings.length > 0 && (
        <button className="btn" type="button" disabled={busy} onClick={onRedact}>{t('action.redact')}</button>
      )}
      <button className="btn" type="button" disabled={busy} onClick={onWithdraw}>{t('action.withdraw')}</button>
      <button className="btn" type="button" disabled>{t('action.submit')}</button>
      <p>{submitReason}</p>
      <button className="btn" type="button" onClick={() => leave(openDraftList)}>{t('action.back')}</button>
      <button className="btn" type="button" onClick={() => leave(closeDrafts)}>{t('action.close')}</button>
    </Shell>
  )
}

const HiddenContext = createContext(false)

/**
 * The drafts list, or one draft. Renders nothing while the panel is closed.
 * `hidden` keeps it mounted under another panel, so unsaved edits stay.
 */
export function KnowledgeEntryDraftsPanel({ hidden = false }: { hidden?: boolean }): JSX.Element | null {
  const state = useDraftsPanel()
  if (!state) return null
  return (
    <HiddenContext.Provider value={hidden}>
      {/* A new detail per draft: the panel stays mounted now, and nothing of
          one draft (fields, busy, a reply still on its way) may carry over to
          the next. */}
      {state.view === 'list' ? <DraftList /> : <DraftDetail key={state.id} id={state.id} created={state.created} />}
    </HiddenContext.Provider>
  )
}

/**
 * One line on the Knowledge page. Asks for the list on mount and whenever
 * this panel closes. Nothing to show when there are no drafts, or no bridge.
 */
export function KnowledgeEntryDraftsBar(): JSX.Element | null {
  const t = useT(draftMessages)
  const open = useDraftsPanel() !== null
  const [count, setCount] = useState<number | null>(null)
  useEffect(() => {
    if (open) return
    let live = true
    const list = window.agent24?.knowledgeEntry?.listDrafts
    if (typeof list !== 'function') {
      setCount(0)
      return
    }
    invoke(() => list()).then(
      (value) => { if (live) setCount(Array.isArray(value) ? value.filter(isSummary).length : 0) },
      () => { if (live) setCount(0) },
    )
    return () => { live = false }
  }, [open])
  if (open || !count) return null
  return (
    <p>
      {t('bar.count', { n: count })}
      {' '}
      <button className="btn" type="button" onClick={() => openDraftList()}>{t('bar.view')}</button>
    </p>
  )
}
