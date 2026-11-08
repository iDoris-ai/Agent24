// KB-SOP-01a / KB-SOP-01b-2 (#1669): "沉淀为知识条目". The renderer only names
// the conversation: main reads its own copy, refuses it unless it is Ordinary
// and asks the user in a native dialog (main/knowledge-entry-ipc.ts). On yes,
// the user picks a message range and main drafts it on this machine.

import { useContext, useEffect, useState, useSyncExternalStore } from 'react'
import type { CapabilityId } from '../../capabilities/types'
import type { KnowledgeEntryAdmission, KnowledgeEntryDraftCreated, KnowledgeEntryMessagePreview, KnowledgeEntryModelOutcome } from '../../../shared/ipc-types'
import { CapabilitiesContext } from '../../capabilities/types'
import { defineMessages, useT, type T } from '../../i18n'
import { openDraft } from '../knowledge-entry-drafts'
import type { MenuEntry } from './HistoryMenu'

export const knowledgeEntryMessages = defineMessages({
  en: {
    'menu.entry': 'Turn into knowledge entry',
    'panel.title': 'Knowledge entry',
    'panel.checking': 'Checking this conversation…',
    'panel.drafting': 'Preparing a draft on this machine… (on-device model only)',
    'choose.heading': 'Choose the messages to organize',
    'choose.from': 'From',
    'choose.to': 'To',
    'choose.generate': 'Create draft',
    'choose.cancel': 'Cancel',
    'role.user': 'You',
    'role.assistant': 'Assistant',
    'draftFail.ticket_expired': 'This confirmation has expired. Choose “Turn into knowledge entry” again in the conversation history.',
    'draftFail.invalid_range': 'That message range is not valid. Please choose again.',
    'draftFail.too_long': 'Those messages are too long. Choose fewer.',
    'draftFail.too_many': 'There are already 50 local drafts. Withdraw some first.',
    'draftFail.busy': 'Another draft is being created. Please wait.',
    'draftFail.not_saved': 'The draft could not be saved. Try again in a moment.',
    'refused.restricted': 'This conversation used restricted material, so it cannot become a knowledge entry.',
    'refused.unknown': 'Agent24 cannot tell what this conversation was drawn from, so it cannot become a knowledge entry.',
    'refused.floor_missing': 'Agent24 has no record of where this conversation’s replies came from, so it cannot become a knowledge entry.',
    'refused.too_many_runs': 'This conversation is too long to check. Start a shorter one for the knowledge entry.',
    'refused.not_confirmed': 'Nothing was done.',
    'refused.not_found': 'This conversation is no longer here.',
    'refused.busy': 'Another conversation is waiting for your answer. Answer it first.',
    'refused.knowledge_off': 'Knowledge is off or not bound to a knowledge base. Turn it on in Settings first.',
    'refused.failed': 'This conversation could not be checked. Try again in a moment.',
    'action.close': 'Close',
  },
  zh: {
    'menu.entry': '沉淀为知识条目',
    'panel.title': '知识条目',
    'panel.checking': '正在检查这条对话…',
    'panel.drafting': '正在本机整理草稿…（只用本机模型）',
    'choose.heading': '选择要整理的消息',
    'choose.from': '从',
    'choose.to': '到',
    'choose.generate': '生成草稿',
    'choose.cancel': '取消',
    'role.user': '你',
    'role.assistant': '助手',
    'draftFail.ticket_expired': '这次确认已过期，请在历史对话里重新选「沉淀为知识条目」。',
    'draftFail.invalid_range': '选的消息范围不对，请重新选。',
    'draftFail.too_long': '选的消息太长，请少选几条。',
    'draftFail.too_many': '本地草稿已满 50 份，请先撤回一些。',
    'draftFail.busy': '另一份草稿正在生成，请稍候。',
    'draftFail.not_saved': '草稿没能保存，请稍后再试。',
    'refused.restricted': '这条对话用到了受限资料，不能整理成知识条目。',
    'refused.unknown': 'Agent24 无法确认这条对话用到了哪些资料，不能整理成知识条目。',
    'refused.floor_missing': 'Agent24 没有这条对话回复来源的记录，不能整理成知识条目。',
    'refused.too_many_runs': '这条对话太长，无法检查。请用一条较短的对话整理知识条目。',
    'refused.not_confirmed': '没有做任何操作。',
    'refused.not_found': '这条对话已经不在了。',
    'refused.busy': '另一条对话正在等你确认，请先处理那一条。',
    'refused.knowledge_off': 'Knowledge 未开启或未绑定知识库，请先在设置里开启。',
    'refused.failed': '暂时无法检查这条对话，请稍后再试。',
    'action.close': '关闭',
  },
  th: {
    'menu.entry': 'ทำเป็นรายการความรู้',
    'panel.title': 'รายการความรู้',
    'panel.checking': 'กำลังตรวจสอบบทสนทนานี้…',
    'panel.drafting': 'กำลังจัดฉบับร่างบนเครื่องนี้… (ใช้เฉพาะโมเดลในเครื่อง)',
    'choose.heading': 'เลือกข้อความที่จะจัดระเบียบ',
    'choose.from': 'จาก',
    'choose.to': 'ถึง',
    'choose.generate': 'สร้างฉบับร่าง',
    'choose.cancel': 'ยกเลิก',
    'role.user': 'คุณ',
    'role.assistant': 'ผู้ช่วย',
    'draftFail.ticket_expired': 'การยืนยันนี้หมดอายุแล้ว โปรดเลือก «ทำเป็นรายการความรู้» อีกครั้งจากประวัติบทสนทนา',
    'draftFail.invalid_range': 'ช่วงข้อความที่เลือกไม่ถูกต้อง โปรดเลือกใหม่',
    'draftFail.too_long': 'ข้อความที่เลือกยาวเกินไป โปรดเลือกให้น้อยลง',
    'draftFail.too_many': 'ฉบับร่างในเครื่องเต็ม 50 ฉบับแล้ว โปรดถอนบางฉบับก่อน',
    'draftFail.busy': 'กำลังสร้างฉบับร่างอื่นอยู่ โปรดรอสักครู่',
    'draftFail.not_saved': 'บันทึกฉบับร่างไม่สำเร็จ โปรดลองอีกครั้งในอีกสักครู่',
    'refused.restricted': 'บทสนทนานี้ใช้ข้อมูลที่จำกัดการเข้าถึง จึงทำเป็นรายการความรู้ไม่ได้',
    'refused.unknown': 'Agent24 ไม่สามารถยืนยันได้ว่าบทสนทนานี้ใช้ข้อมูลจากที่ใด จึงทำเป็นรายการความรู้ไม่ได้',
    'refused.floor_missing': 'Agent24 ไม่มีบันทึกว่าคำตอบในบทสนทนานี้มาจากที่ใด จึงทำเป็นรายการความรู้ไม่ได้',
    'refused.too_many_runs': 'บทสนทนานี้ยาวเกินกว่าจะตรวจสอบได้ โปรดใช้บทสนทนาที่สั้นกว่านี้',
    'refused.not_confirmed': 'ไม่ได้ดำเนินการใด ๆ',
    'refused.not_found': 'ไม่พบบทสนทนานี้แล้ว',
    'refused.busy': 'มีอีกบทสนทนาหนึ่งรอการยืนยันจากคุณอยู่ โปรดตอบบทสนทนานั้นก่อน',
    'refused.knowledge_off': 'Knowledge ปิดอยู่หรือยังไม่ได้เชื่อมกับฐานความรู้ โปรดเปิดในการตั้งค่าก่อน',
    'refused.failed': 'ยังตรวจสอบบทสนทนานี้ไม่ได้ โปรดลองอีกครั้งในอีกสักครู่',
    'action.close': 'ปิด',
  },
})

/** The Knowledge page's capability id (capabilities/registry.ts). */
export const KNOWLEDGE_PAGE: CapabilityId = 'knowledgeBase'

type DraftFailReason = Extract<KnowledgeEntryDraftCreated, { ok: false }>['reason'] | 'failed'

/** What the Knowledge page shows about the conversation last asked about. */
export type EntryAsk =
  | { id: string; state: 'checking' }
  | { id: string; state: 'done'; answer: Extract<KnowledgeEntryAdmission, { ok: false }> }
  | { id: string; state: 'failed' }
  | { id: string; state: 'choose'; ticket: string; messages: KnowledgeEntryMessagePreview[] }
  | { id: string; state: 'drafting' }
  | { id: string; state: 'draftFailed'; reason: DraftFailReason }

// Module state, so the list can hand the question to the page without App or
// Chat carrying it. The question is asked once, from the click, never from a
// render or an effect (StrictMode runs those twice). One renderer has one main
// window, so one panel state is enough; a second window would share it.
let current: EntryAsk | null = null
let asked = 0
const listeners = new Set<() => void>()

function publish(next: EntryAsk | null): void {
  current = next
  for (const listener of listeners) listener()
}

/** Asks main about conversation `id`; null closes the panel. */
export function requestEntry(id: string | null): void {
  const token = ++asked
  if (id === null) {
    publish(null)
    return
  }
  publish({ id, state: 'checking' })
  let reply: Promise<unknown>
  try {
    reply = Promise.resolve(window.agent24.knowledgeEntry.admit(id))
  } catch (err) {
    reply = Promise.reject(err)
  }
  reply.then(
    (answer) => {
      if (token !== asked) return
      if (isAdmitted(answer)) {
        publish({ id, state: 'choose', ticket: answer.ticket, messages: answer.messages })
        return
      }
      publish(isRefusal(answer) ? { id, state: 'done', answer } : { id, state: 'failed' })
    },
    () => { if (token === asked) publish({ id, state: 'failed' }) },
  )
}

const CREATE_REASONS = new Set<string>(['ticket_expired', 'invalid_range', 'too_long', 'too_many', 'knowledge_off', 'busy', 'not_saved'])
const DRAFT_MODELS = new Set<KnowledgeEntryModelOutcome>(['drafted', 'no_model', 'needs_confirmation', 'busy', 'failed', 'unusable'])

function isMessage(value: unknown): value is KnowledgeEntryMessagePreview {
  if (typeof value !== 'object' || value === null) return false
  const v = value as { role?: unknown; preview?: unknown }
  return (v.role === 'user' || v.role === 'assistant') && typeof v.preview === 'string'
}

/** Admitted only with the ticket and the messages main actually hands over. */
function isAdmitted(value: unknown): value is { ok: true; ticket: string; messages: KnowledgeEntryMessagePreview[] } {
  if (typeof value !== 'object' || value === null) return false
  const v = value as { ok?: unknown; ticket?: unknown; messages?: unknown }
  return v.ok === true && typeof v.ticket === 'string' && Array.isArray(v.messages) && v.messages.every(isMessage)
}

/** A refusal the panel has words for; anything else reads as a failure. */
function isRefusal(value: unknown): value is Extract<KnowledgeEntryAdmission, { ok: false }> {
  if (typeof value !== 'object' || value === null) return false
  const v = value as { ok?: unknown; reason?: unknown }
  return v.ok === false && typeof v.reason === 'string' && Object.hasOwn(knowledgeEntryMessages.en, `refused.${v.reason}`)
}

function isCreatedDraft(value: unknown): boolean {
  if (typeof value !== 'object' || value === null) return false
  const v = value as { id?: unknown; kind?: unknown; title?: unknown; body?: unknown; target?: unknown; findings?: unknown; message_from?: unknown; message_to?: unknown }
  return typeof v.id === 'string'
    && (v.kind === 'sop' || v.kind === 'faq' || v.kind === 'note')
    && typeof v.title === 'string'
    && typeof v.body === 'string'
    && (v.target === 'same' || v.target === 'changed' || v.target === 'none')
    && Array.isArray(v.findings)
    && v.findings.every((item) => typeof item === 'string')
    && typeof v.message_from === 'number'
    && typeof v.message_to === 'number'
}

/** createDraft's reply as the panel can show it; anything else reads as a failure. */
function isDraftCreated(value: unknown): value is KnowledgeEntryDraftCreated {
  if (typeof value !== 'object' || value === null) return false
  const v = value as { ok?: unknown; reason?: unknown; draft?: unknown; model?: unknown; hints?: unknown }
  if (v.ok === false) return typeof v.reason === 'string' && CREATE_REASONS.has(v.reason)
  if (v.ok !== true) return false
  return isCreatedDraft(v.draft)
    && typeof v.model === 'string' && DRAFT_MODELS.has(v.model as KnowledgeEntryModelOutcome)
    && Array.isArray(v.hints) && v.hints.every((hint) => typeof hint === 'string')
}

/** One shot from the 「生成草稿」 click. A later cancel invalidates `token`. */
function beginDraft(from: number, to: number): void {
  const ask = current
  if (ask?.state !== 'choose' || from > to) return
  const token = ++asked
  const { id, ticket } = ask
  publish({ id, state: 'drafting' })
  let reply: Promise<unknown>
  try {
    reply = Promise.resolve(window.agent24.knowledgeEntry.createDraft(ticket, { from, to }))
  } catch (err) {
    reply = Promise.reject(err)
  }
  reply.then(
    (answer) => {
      if (token !== asked) return
      if (!isDraftCreated(answer)) {
        publish({ id, state: 'draftFailed', reason: 'failed' })
        return
      }
      if (!answer.ok) {
        publish({ id, state: 'draftFailed', reason: answer.reason })
        return
      }
      publish(null)
      openDraft(answer.draft.id, { model: answer.model, hints: answer.hints })
    },
    () => { if (token === asked) publish({ id, state: 'draftFailed', reason: 'failed' }) },
  )
}

function draftFailedText(t: T<keyof typeof knowledgeEntryMessages.en>, reason: DraftFailReason): string {
  switch (reason) {
    case 'knowledge_off': return t('refused.knowledge_off')
    case 'failed': return t('refused.failed')
    case 'ticket_expired': return t('draftFail.ticket_expired')
    case 'invalid_range': return t('draftFail.invalid_range')
    case 'too_long': return t('draftFail.too_long')
    case 'too_many': return t('draftFail.too_many')
    case 'busy': return t('draftFail.busy')
    case 'not_saved': return t('draftFail.not_saved')
  }
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  return () => { listeners.delete(listener) }
}

export function usePendingEntry(): EntryAsk | null {
  return useSyncExternalStore(subscribe, () => current)
}

/**
 * Whether Knowledge is on and bound now. Main says so only on request, so it
 * is asked again on mount, when the window comes back, and on every press
 * that may open a history menu (an open menu takes the change in place).
 * Not known yet, or the question failed: off.
 */
function useKnowledgeBound(): boolean {
  const [bound, setBound] = useState(false)
  useEffect(() => {
    let live = true
    const refresh = (): void => {
      // Some pages and tests run without the preload bridge: that is off too.
      const status = window.agent24?.knowledge?.status?.()
      if (!status) { setBound(false); return }
      status.then((s) => { if (live) setBound(s.active === true) }, () => { if (live) setBound(false) })
    }
    refresh()
    window.addEventListener('focus', refresh)
    document.addEventListener('pointerdown', refresh, true)
    document.addEventListener('contextmenu', refresh, true)
    return () => {
      live = false
      window.removeEventListener('focus', refresh)
      document.removeEventListener('pointerdown', refresh, true)
      document.removeEventListener('contextmenu', refresh, true)
    }
  }, [])
  return bound
}

/** The history list's one new action, for any conversation, archived ones
 *  included; none at all while Knowledge is off or not bound. */
export function useEntryMenu(): (id: string) => MenuEntry[] {
  const t = useT(knowledgeEntryMessages)
  const { open } = useContext(CapabilitiesContext)
  const bound = useKnowledgeBound()
  return (id) => (!bound ? [] : [{
    kind: 'item',
    key: 'knowledge-entry',
    label: t('menu.entry'),
    // The panel and the native dialog take the focus.
    focuses: true,
    onSelect: () => {
      requestEntry(id)
      open(KNOWLEDGE_PAGE, 'other')
    },
  }])
}

function optionLabel(t: T<keyof typeof knowledgeEntryMessages.en>, message: KnowledgeEntryMessagePreview, index: number): string {
  const role = t(message.role === 'user' ? 'role.user' : 'role.assistant')
  return `${index + 1}. ${role}：${message.preview}`
}

function ChooseRange({ ask }: { ask: Extract<EntryAsk, { state: 'choose' }> }): JSX.Element {
  const t = useT(knowledgeEntryMessages)
  const [from, setFrom] = useState(0)
  const [to, setTo] = useState(ask.messages.length - 1)
  return (
    <section className="knowledge-confirm" role="dialog" aria-modal="false" aria-label={t('panel.title')}>
      <p className="knowledge-confirm-count">{t('choose.heading')}</p>
      <label htmlFor="knowledge-entry-from">{t('choose.from')}</label>
      <select id="knowledge-entry-from" value={from} onChange={(e) => setFrom(Number(e.target.value))}>
        {ask.messages.map((message, index) => (
          <option key={index} value={index}>{optionLabel(t, message, index)}</option>
        ))}
      </select>
      <label htmlFor="knowledge-entry-to">{t('choose.to')}</label>
      <select id="knowledge-entry-to" value={to} onChange={(e) => setTo(Number(e.target.value))}>
        {ask.messages.map((message, index) => (
          <option key={index} value={index}>{optionLabel(t, message, index)}</option>
        ))}
      </select>
      <button className="btn btn-primary" type="button" disabled={from > to} onClick={() => beginDraft(from, to)}>{t('choose.generate')}</button>
      <button className="btn" type="button" onClick={() => requestEntry(null)}>{t('choose.cancel')}</button>
    </section>
  )
}

/** The panel on the Knowledge page while a conversation is asked about. */
export function KnowledgeEntryPanel({ ask }: { ask: EntryAsk }): JSX.Element {
  const t = useT(knowledgeEntryMessages)
  if (ask.state === 'choose') return <ChooseRange ask={ask} />
  if (ask.state === 'drafting') {
    return (
      <section className="knowledge-confirm" role="dialog" aria-modal="false" aria-label={t('panel.title')}>
        <p role="status">{t('panel.drafting')}</p>
      </section>
    )
  }
  const text = ask.state === 'checking'
    ? t('panel.checking')
    : ask.state === 'failed'
      ? t('refused.failed')
      : ask.state === 'draftFailed'
        ? draftFailedText(t, ask.reason)
        : t(`refused.${ask.answer.reason}`)
  return (
    <section className="knowledge-confirm" role="dialog" aria-modal="false" aria-label={t('panel.title')}>
      <p role="status">{text}</p>
      {ask.state !== 'checking' && (
        <button className="btn" type="button" onClick={() => requestEntry(null)}>{t('action.close')}</button>
      )}
    </section>
  )
}
