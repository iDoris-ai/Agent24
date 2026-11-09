// KB-SOP-01a (#1669): the one IPC call behind "沉淀为知识条目". The renderer
// names a conversation by id and nothing else in this call; main reads the
// conversation from the history store, refuses it in plain words unless the
// daemon says it is Ordinary (knowledge-entry-admission.ts), and only then
// asks the user in a native dialog whose text main builds. The renderer gets
// back only whether the conversation was admitted and why not.
//
// Trust: the history file is written by the renderer (ChatHistorySave), text
// and run ids alike. The classification vouches for where an honest page's
// replies came from; it cannot stop a compromised renderer that writes
// arbitrary text into a conversation, which no check of run ids could either.
//
// KB-SOP-01b-2: after an admission main hands the renderer a one-use ticket
// for the very record it judged (`judge` returns it), never a later read: the
// conversation may have moved on since. The renderer chooses a range of that
// record's messages and drafts from the ticket (knowledge-entry-extract.ts:
// rule table → local model → rule table). The drafts' IPC is here too; the
// renderer gets their text back, and never their binding.

import { randomBytes } from 'node:crypto'
import type { IpcMain, IpcMainInvokeEvent } from 'electron'
import type { ChatHistoryRecord } from '../shared/chat-history'
import {
  IpcChannels,
  type KnowledgeEntryAdmission,
  type KnowledgeEntryAdmitAnswer,
  type KnowledgeEntryDraftCreated,
  type KnowledgeEntryDraftSaved,
  type KnowledgeEntryDraftSummary,
  type KnowledgeEntryDraftView,
  type KnowledgeEntryMessagePreview,
  type KnowledgeEntryTarget,
} from '../shared/ipc-types'
import type { IpcGuard } from './ipc/guard'
import { admit, classify, type AdmissionReader } from './knowledge-entry-admission'
import { MAX_ENTRY_BODY, MAX_ENTRY_DRAFTS, MAX_ENTRY_TITLE, type EntryDraft, type createEntryDraftStore } from './knowledge-entry-draft'
import { clearFindings, extractEntry, fallbackTitle, findings, SourceTooLong, sourceMessages, type ChatCall } from './knowledge-entry-extract'
import { REDACTION_RULES_VERSION } from './knowledge-entry-redact'
import { clipUnits, ENTRY_SKILL_VERSION } from './knowledge-entry-skill'
import type { MainLocale } from './main-i18n'
import type { KnowledgePending } from './knowledge-pending'

/** The longest title the dialog shows before cutting it short. */
const TITLE_CHARS = 60

const en = {
  message: 'Turn “{title}” into a knowledge entry?',
  detail: 'Only what you and the assistant wrote in this conversation is used. It is redacted on this computer first and becomes a local draft; nothing is sent to the organisation knowledge base until you have read the draft and submit it for review. Tool results, memories and attachments are not used.',
  confirm: 'Continue',
  cancel: 'Cancel',
  untitled: 'Untitled conversation',
  untitledEntry: 'Untitled entry',
}
type Strings = typeof en
const zh: Strings = {
  message: '把对话「{title}」沉淀为知识条目？',
  detail: '只使用这条对话里你和助手写的正文。先在本机脱敏，生成本地草稿；你看过草稿、提交审阅之前，什么都不会发到组织知识库。工具结果、记忆和附件不会被使用。',
  confirm: '继续',
  cancel: '取消',
  untitled: '未命名对话',
  untitledEntry: '未命名知识条目',
}
const th: Strings = {
  message: 'ทำบทสนทนา “{title}” เป็นรายการความรู้หรือไม่',
  detail: 'ใช้เฉพาะข้อความที่คุณและผู้ช่วยเขียนในบทสนทนานี้ โดยจะลบข้อมูลที่อ่อนไหวออกในเครื่องนี้ก่อน แล้วสร้างเป็นฉบับร่างในเครื่อง จะไม่มีสิ่งใดถูกส่งไปยังฐานความรู้ขององค์กรจนกว่าคุณจะอ่านฉบับร่างแล้วส่งให้ตรวจทาน ผลลัพธ์จากเครื่องมือ ความจำ และไฟล์แนบจะไม่ถูกใช้',
  confirm: 'ดำเนินการต่อ',
  cancel: 'ยกเลิก',
  untitled: 'บทสนทนาไม่มีชื่อ',
  untitledEntry: 'รายการความรู้ไม่มีชื่อ',
}
const STRINGS: Record<MainLocale, Strings> = { en, zh, th }

export interface KnowledgeEntryDeps {
  /** Main's own copy of a conversation; null when there is none. May throw on a malformed id. */
  conversation: (id: string) => ChatHistoryRecord | null
  reader: AdmissionReader
  /** A native yes/no for this sender's window; true only when the user chose yes. */
  confirm: (event: IpcMainInvokeEvent, ask: { message: string; detail: string; confirm: string; cancel: string }) => Promise<boolean>
  locale: () => MainLocale
  /** Knowledge is on and bound to a knowledge base right now. */
  bound: () => boolean
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
  /** The admission, and on success the very record it judged. */
  const judge = async (event: IpcMainInvokeEvent, id: unknown): Promise<{ admission: KnowledgeEntryAdmission; record: ChatHistoryRecord | null }> => {
    const admission = await decide(event, id)
    return admission.answer.ok ? { admission: admission.answer, record: admission.record } : { admission: admission.answer, record: null }
  }
  const decide = async (event: IpcMainInvokeEvent, id: unknown): Promise<{ answer: KnowledgeEntryAdmission; record: ChatHistoryRecord | null }> => {
    const answer = (value: KnowledgeEntryAdmission, record: ChatHistoryRecord | null = null) => ({ answer: value, record })
    // The list offers the action only while bound; this holds it to that.
    if (!deps.bound()) return answer({ ok: false, reason: 'knowledge_off' })
    const record = load(id)
    if (!record) return answer({ ok: false, reason: 'not_found' })
    // Refused before anything is asked: the user is told why, not asked to confirm.
    const first = await classify(record, deps.reader)
    if (!first.ok) return answer(first)
    // One dialog at a time; a second request is turned away, not queued.
    if (asking) return answer({ ok: false, reason: 'busy' })
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
    // The conversation may have moved on while the dialog was open: judge it
    // again as it is now. The dialog named the conversation, not its messages,
    // so the record judged here is the one drafted; the user reads that draft
    // before anything is submitted (KB-SOP-01b).
    if (!deps.bound()) return answer({ ok: false, reason: 'knowledge_off' })
    const now = load(id)
    if (!now) return answer({ ok: false, reason: 'not_found' })
    return answer(await admit(now, deps.reader, confirmed), now)
  }
  return { judge, admit: async (event: IpcMainInvokeEvent, id: unknown): Promise<KnowledgeEntryAdmission> => (await judge(event, id)).admission }
}

type DraftStore = Pick<ReturnType<typeof createEntryDraftStore>, 'create' | 'update' | 'get' | 'list' | 'withdraw'>

export interface KnowledgeEntryDraftDeps {
  store: DraftStore
  /** POST /api/v1/chat through main's own daemon connection. */
  chat: ChatCall
  /** What Knowledge is bound to now (`entryTarget`), or null while it is off or not bound. */
  target: () => string | null
  locale: () => MainLocale
  now?: () => number
  /** M4 A7: a draft waits in the conversation it came from until it is withdrawn; none without it. */
  pending?: KnowledgePending
}

/** How long a ticket stays good: long enough to choose a range. */
export const TICKET_TTL_MS = 15 * 60 * 1000
/** Tickets held at once; the oldest goes first. */
export const MAX_TICKETS = 8
/** Characters of each message shown when choosing the range. */
export const PREVIEW_CHARS = 80

export function messagePreviews(record: ChatHistoryRecord): KnowledgeEntryMessagePreview[] {
  return record.messages.map((m) => {
    const chars = [...m.content.replace(/\s+/g, ' ').trim()]
    return { role: m.role, preview: chars.length > PREVIEW_CHARS ? `${chars.slice(0, PREVIEW_CHARS).join('')}…` : chars.join('') }
  })
}

function isRange(value: unknown, length: number): value is { from: number; to: number } {
  if (typeof value !== 'object' || value === null) return false
  const { from, to } = value as { from?: unknown; to?: unknown }
  return Number.isInteger(from) && Number.isInteger(to) && (from as number) >= 0 && (from as number) <= (to as number) && (to as number) < length
}

function isPatch(value: unknown): value is { kind: unknown; title: unknown; body: unknown } {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/**
 * The drafts behind the Knowledge page. Every view is read from the store
 * and judged again now: the target against what Knowledge is bound to (QA
 * N11), the text by the post-scan. Neither is stored; submitting (KB-SOP-01c)
 * judges both again.
 */
export function knowledgeEntryDrafts(deps: KnowledgeEntryDraftDeps) {
  const now = deps.now ?? Date.now
  const tickets = new Map<string, { record: ChatHistoryRecord; sender: number; expires: number }>()
  let drafting = false

  const strings = (): Strings => STRINGS[deps.locale()] ?? en

  const targetOf = (binding: string): KnowledgeEntryTarget => {
    const current = deps.target()
    if (current === null) return 'none'
    return current === binding ? 'same' : 'changed'
  }

  const summary = (draft: EntryDraft): KnowledgeEntryDraftSummary => ({
    id: draft.id,
    kind: draft.kind,
    title: draft.title,
    updated_at: draft.updated_at,
    target: targetOf(draft.binding),
    findings: findings(draft.title, draft.body),
  })

  const view = (draft: EntryDraft): KnowledgeEntryDraftView => ({
    ...summary(draft),
    body: draft.body,
    conversation_id: draft.source.conversation_id,
    message_from: draft.source.message_from,
    message_to: draft.source.message_to,
    created_at: draft.created_at,
    content_sha256: draft.content_sha256,
  })

  const issue = (record: ChatHistoryRecord, sender: number): string => {
    const at = now()
    for (const [key, held] of tickets) if (held.expires <= at) tickets.delete(key)
    while (tickets.size >= MAX_TICKETS) tickets.delete(tickets.keys().next().value as string)
    const ticket = randomBytes(16).toString('base64url')
    tickets.set(ticket, { record, sender, expires: at + TICKET_TTL_MS })
    return ticket
  }

  const saved = (run: () => EntryDraft): KnowledgeEntryDraftSaved => {
    try {
      return { ok: true, draft: view(run()) }
    } catch {
      return { ok: false, reason: 'invalid' }
    }
  }

  const changeable = (id: unknown): EntryDraft | 'not_found' | 'not_saved' => {
    try {
      return deps.store.get(id) ?? 'not_found'
    } catch {
      return 'not_saved'
    }
  }

  return {
    issue,

    async create(event: IpcMainInvokeEvent, ticket: unknown, range: unknown): Promise<KnowledgeEntryDraftCreated> {
      const held = typeof ticket === 'string' ? tickets.get(ticket) : undefined
      if (!held || held.sender !== event.sender.id || held.expires <= now()) {
        if (typeof ticket === 'string' && held && held.expires <= now()) tickets.delete(ticket)
        return { ok: false, reason: 'ticket_expired' }
      }
      const target = deps.target()
      if (target === null) return { ok: false, reason: 'knowledge_off' }
      if (!isRange(range, held.record.messages.length)) return { ok: false, reason: 'invalid_range' }
      if (drafting) return { ok: false, reason: 'busy' }
      let count: number
      try {
        count = deps.store.list().length
      } catch {
        return { ok: false, reason: 'not_saved' }
      }
      if (count >= MAX_ENTRY_DRAFTS) return { ok: false, reason: 'too_many' }
      let source
      try {
        source = sourceMessages(held.record, range.from, range.to)
      } catch (err) {
        return { ok: false, reason: err instanceof SourceTooLong ? 'too_long' : 'not_saved' }
      }
      // From here on the ticket is spent, whatever the model does.
      tickets.delete(ticket as string)
      drafting = true
      try {
        const s = strings()
        const extraction = await extractEntry(source, fallbackTitle(held.record.title, s.untitledEntry), deps.chat)
        let draft: EntryDraft
        try {
          draft = deps.store.create({
            binding: target,
            source: { conversation_id: held.record.id, message_from: range.from, message_to: range.to },
            floor: 'ordinary',
            skill_version: ENTRY_SKILL_VERSION,
            redaction_rules_version: REDACTION_RULES_VERSION,
            kind: extraction.kind,
            title: extraction.title,
            body: extraction.body,
          })
        } catch {
          return { ok: false, reason: 'not_saved' }
        }
        deps.pending?.set(draft.source.conversation_id, draft.id, draft.title)
        // Hints are only words of the body already (parseModelDraft), and the store keeps the body as given.
        return { ok: true, draft: view(draft), model: extraction.model, hints: extraction.hints }
      } finally {
        drafting = false
      }
    },

    list(): KnowledgeEntryDraftSummary[] {
      try {
        return deps.store.list().map(summary)
      } catch {
        return []
      }
    },

    get(id: unknown): KnowledgeEntryDraftView | null {
      try {
        const draft = deps.store.get(id)
        return draft ? view(draft) : null
      } catch {
        return null
      }
    },

    update(id: unknown, patch: unknown): KnowledgeEntryDraftSaved {
      const current = changeable(id)
      if (typeof current === 'string') return { ok: false, reason: current }
      if (!isPatch(patch)) return { ok: false, reason: 'invalid' }
      return saved(() => deps.store.update(current.id, { kind: patch.kind, title: patch.title, body: patch.body }))
    },

    /** Replaces everything the post-scan finds in the title and the body with placeholders ("一键脱敏"). */
    redact(id: unknown): KnowledgeEntryDraftSaved {
      const current = changeable(id)
      if (typeof current === 'string') return { ok: false, reason: current }
      // A placeholder can be longer than what it replaces. The title is cut to
      // its limit; the body is not cut, the user is told instead.
      const body = clearFindings(current.body)
      if (Buffer.byteLength(body, 'utf8') > MAX_ENTRY_BODY) return { ok: false, reason: 'too_long' }
      return saved(() => {
        const title = clipUnits(clearFindings(current.title), MAX_ENTRY_TITLE).trim()
        return deps.store.update(current.id, { title: title.length > 0 ? title : strings().untitledEntry, body })
      })
    },

    withdraw(id: unknown): boolean {
      // A draft that cannot be read is still withdrawn; only its pending item is left.
      let draft: EntryDraft | null = null
      try {
        draft = deps.store.get(id)
      } catch {
        draft = null
      }
      try {
        const withdrawn = deps.store.withdraw(id)
        if (withdrawn && draft) deps.pending?.clear(draft.source.conversation_id, draft.id)
        return withdrawn
      } catch {
        return false
      }
    },
  }
}

export function registerKnowledgeEntryIpc(ipc: Pick<IpcMain, 'handle'>, guard: IpcGuard, deps: KnowledgeEntryDeps, drafting: KnowledgeEntryDraftDeps): void {
  const admitter = knowledgeEntryAdmitter(deps)
  const drafts = knowledgeEntryDrafts(drafting)
  ipc.handle(IpcChannels.KnowledgeEntryAdmit, guard(async (event: IpcMainInvokeEvent, id: unknown): Promise<KnowledgeEntryAdmitAnswer> => {
    const { admission, record } = await admitter.judge(event, id)
    if (!admission.ok) return admission
    if (!record) return { ok: false, reason: 'not_found' }
    return { ok: true, ticket: drafts.issue(record, event.sender.id), messages: messagePreviews(record) }
  }))
  ipc.handle(IpcChannels.KnowledgeEntryDraftCreate, guard((event: IpcMainInvokeEvent, ticket: unknown, range: unknown) => drafts.create(event, ticket, range)))
  ipc.handle(IpcChannels.KnowledgeEntryDraftList, guard(() => drafts.list()))
  ipc.handle(IpcChannels.KnowledgeEntryDraftGet, guard((_event: IpcMainInvokeEvent, id: unknown) => drafts.get(id)))
  ipc.handle(IpcChannels.KnowledgeEntryDraftUpdate, guard((_event: IpcMainInvokeEvent, id: unknown, patch: unknown) => drafts.update(id, patch)))
  ipc.handle(IpcChannels.KnowledgeEntryDraftRedact, guard((_event: IpcMainInvokeEvent, id: unknown) => drafts.redact(id)))
  ipc.handle(IpcChannels.KnowledgeEntryDraftWithdraw, guard((_event: IpcMainInvokeEvent, id: unknown) => drafts.withdraw(id)))
}
