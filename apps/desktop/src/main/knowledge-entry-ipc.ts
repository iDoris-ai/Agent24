// KB-SOP-01a (#1669): the one IPC call behind "沉淀为知识条目". The renderer
// names a conversation by id and nothing else; main reads its own copy of
// the conversation, refuses it in plain words unless the daemon says it is
// Ordinary (knowledge-entry-admission.ts), and only then asks the user in a
// native dialog whose text main builds. The renderer gets back only whether
// the conversation was admitted and why not.
//
// What comes after an admission (redaction, the local draft) is KB-SOP-01b.

import type { IpcMain, IpcMainInvokeEvent } from 'electron'
import type { ChatHistoryRecord } from '../shared/chat-history'
import { IpcChannels, type KnowledgeEntryAdmission } from '../shared/ipc-types'
import type { IpcGuard } from './ipc/guard'
import { admit, classify, type AdmissionReader } from './knowledge-entry-admission'
import type { MainLocale } from './main-i18n'

/** The longest title the dialog shows before cutting it short. */
const TITLE_CHARS = 60

const en = {
  message: 'Turn “{title}” into a knowledge entry?',
  detail: 'Only what you and the assistant wrote in this conversation is used. It is redacted on this computer first and becomes a local draft; nothing is sent to the organisation knowledge base until you have read the draft and submit it for review. Tool results, memories and attachments are not used.',
  confirm: 'Continue',
  cancel: 'Cancel',
  untitled: 'Untitled conversation',
}
type Strings = typeof en
const zh: Strings = {
  message: '把对话「{title}」沉淀为知识条目？',
  detail: '只使用这条对话里你和助手写的正文。先在本机脱敏，生成本地草稿；你看过草稿、提交审阅之前，什么都不会发到组织知识库。工具结果、记忆和附件不会被使用。',
  confirm: '继续',
  cancel: '取消',
  untitled: '未命名对话',
}
const th: Strings = {
  message: 'ทำบทสนทนา “{title}” เป็นรายการความรู้หรือไม่',
  detail: 'ใช้เฉพาะข้อความที่คุณและผู้ช่วยเขียนในบทสนทนานี้ โดยจะลบข้อมูลที่อ่อนไหวออกในเครื่องนี้ก่อน แล้วสร้างเป็นฉบับร่างในเครื่อง จะไม่มีสิ่งใดถูกส่งไปยังฐานความรู้ขององค์กรจนกว่าคุณจะอ่านฉบับร่างแล้วส่งให้ตรวจทาน ผลลัพธ์จากเครื่องมือ ความจำ และไฟล์แนบจะไม่ถูกใช้',
  confirm: 'ดำเนินการต่อ',
  cancel: 'ยกเลิก',
  untitled: 'บทสนทนาไม่มีชื่อ',
}
const STRINGS: Record<MainLocale, Strings> = { en, zh, th }

export interface KnowledgeEntryDeps {
  /** Main's own copy of a conversation; null when there is none. May throw on a malformed id. */
  conversation: (id: string) => ChatHistoryRecord | null
  reader: AdmissionReader
  /** A native yes/no for this sender's window; true only when the user chose yes. */
  confirm: (event: IpcMainInvokeEvent, ask: { message: string; detail: string; confirm: string; cancel: string }) => Promise<boolean>
  locale: () => MainLocale
}

export function shortTitle(title: string, untitled: string): string {
  const chars = [...title.replace(/\s+/g, ' ').trim()]
  if (chars.length === 0) return untitled
  return chars.length > TITLE_CHARS ? `${chars.slice(0, TITLE_CHARS).join('')}…` : chars.join('')
}

export function knowledgeEntryAdmitter(deps: KnowledgeEntryDeps) {
  let asking = false
  const load = (id: unknown): ChatHistoryRecord | null => {
    if (typeof id !== 'string') return null
    try {
      return deps.conversation(id)
    } catch {
      return null
    }
  }
  return async (event: IpcMainInvokeEvent, id: unknown): Promise<KnowledgeEntryAdmission> => {
    const record = load(id)
    if (!record) return { ok: false, reason: 'not_found' }
    // Refused before anything is asked: the user is told why, not asked to confirm.
    const first = await classify(record, deps.reader)
    if (!first.ok) return first
    // One dialog at a time; a second request is turned away, not queued.
    if (asking) return { ok: false, reason: 'busy' }
    asking = true
    let confirmed = false
    try {
      const s = STRINGS[deps.locale()] ?? en
      confirmed = await deps.confirm(event, {
        message: s.message.replace('{title}', shortTitle(record.title, s.untitled)),
        detail: s.detail,
        confirm: s.confirm,
        cancel: s.cancel,
      })
    } catch {
      confirmed = false
    } finally {
      asking = false
    }
    // The conversation may have moved on while the dialog was open: judge it again as it is now.
    const now = load(id)
    if (!now) return { ok: false, reason: 'not_found' }
    return admit(now, deps.reader, confirmed)
  }
}

export function registerKnowledgeEntryIpc(ipc: Pick<IpcMain, 'handle'>, guard: IpcGuard, deps: KnowledgeEntryDeps): void {
  ipc.handle(IpcChannels.KnowledgeEntryAdmit, guard(knowledgeEntryAdmitter(deps)))
}
