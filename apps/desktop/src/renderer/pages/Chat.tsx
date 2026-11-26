import { useState, useRef, useEffect, useMemo, useSyncExternalStore } from 'react'
import { formatReplySuffix, extractServerReported, type ReplyLatency } from './chat-latency'
import logoWelcome from '../assets/logo-welcome.png'
import { useT } from '../i18n'
import { chatMessages } from './Chat.i18n'
import type { components } from '@agent24/api-client'
import { askWithKnowledge, createChatSession, knowledgePrompt, knowledgeQuery } from './chat-knowledge'
import { failureKind, withoutToolNames } from './chat/user-facing'
import { DocumentsOutcome, type RunDocuments } from './chat/DocumentsOutcome'
import { grounding, knowledgeFacts, type Grounding, type KnowledgeFacts as Facts } from './knowledge-grounding'
import KnowledgeFactsRow from './chat/KnowledgeFacts'
import KnowledgeCitationCard from './chat/KnowledgeCitationCard'
import { TurnRail } from './chat/TurnRail'
import { Markdown } from '../components/Markdown'
import { activeTurn, buildTurns } from './chat/turns'
import { answeredTurns, generateTitle, promptTitle, titleDue } from './chat/title'
import { CHAT_HISTORY_FORMAT, CHAT_HISTORY_VERSION, promotionDue, type ChatHistoryRecord } from '../../shared/chat-history'
import { DraftAttachments, useKnowledgeDraft } from './chat/knowledge-draft'
import { namedDocument, runDocIntent } from './chat/doc-intent-run'
import { asksDeleteDocument, asksFillForm, continuesFormFill, docAgentPrompt, extractionFields, formClaimUnbacked, formMaterial, materialHint, parseDocIntent, subjectOf, translatesDocument, wantsDocTool } from './chat/doc-intent'
import { askWithProxy, checkDocument, readAllCheckBlocks, reportMarkdown } from './documents/doc-check'
import { paragraphsOf, translateParagraphs, type TargetLang } from './documents/doc-translate'
import { convertedPdf } from './documents/compose'
import { bindPendingDocument, documentOf, documentText, pendingDocument, subscribeDocContext, unbindDocument, withDocument, type DocumentRef } from './chat/doc-context'
import { HandOver, TaskCards } from './chat/TaskCard'
import { ExtractCard } from './chat/ExtractCard'
import { DeleteChoice } from './chat/DeleteChoice'
import { RestoreCard } from './chat/RestoreCard'
import { CompareCard } from './chat/CompareCard'
import { DeliverCard } from './chat/DeliverCard'
import { asksDeliver, runPreflight, runReceipts, type DeliverCardData } from './chat/doc-deliver'
import { formEntryReply, resolveFormEntry, templateHint } from './chat/form-entry'
import { asksCompare, runCompare, type CompareCardData } from './chat/doc-compare'
import { asksRestoreVersion, restoreCardOf, restoreVersion, type RestoreCardData } from './chat/doc-version'
import { KnowledgeUploadCard } from './chat/KnowledgeUploadCard'
import {
  asksOnboarding, asksUpload, beginOnboard, nextOnboard, preflightReply, resultReply, type OnboardStep,
} from './chat/knowledge-onboard'
import { choiceText, deletePrompt, deleteTargetOf, type DeleteCandidate } from './chat/delete-target'
import { extractFields, extractText, fieldNamesFromModel, type ExtractCardData } from './documents/doc-extract'
import {
  forgetPromotion, getHistoryState, historyAvailable, loadConversation, markConversationSeen, promoteRun, registerChatPage, restoreConversation, saveConversation,
  setActiveConversation, setChatBusy, subscribeHistory, titleConversation,
} from './chat/history'
import { SkillPicker } from './chat/SkillPicker'
import { setConversationSkill, skillForConversation } from './chat/session-skill'

/** M1-T14: what `/api/v1/chat` ACTUALLY did with an explicit "记住…" turn —
 *  read off the response field, never off the model's own reply text (the
 *  local model has been observed saying "我记住了" on a turn the server
 *  skipped because memory was paused). */
type MemoryReceipt = 'saved' | 'paused_not_saved' | 'failed' | 'partial'

type ChatRequest = components['schemas']['ChatRequest']
type Delivery = { kind: 'sending' | 'sent' | 'notSent' } | { kind: 'headroom'; request: ChatRequest }
type Message = { role: 'user'; content: string; delivery: Delivery } | AssistantMessage

interface AssistantMessage {
  role: 'assistant'
  content: string
  /** ME4-desktop-model-ui: the "model · tier · 首字/总计 ms" line shown under
   *  an assistant reply. Never set on a user message or the error fallback
   *  message (§ below) — only a real, successful reply gets timed. */
  suffix?: ReplyLatency
  /** M1-T14: the system-provided receipt line shown under an assistant
   *  reply, distinct from (and below) the latency suffix. Never set on a
   *  user message or the error fallback message. */
  memoryReceipt?: MemoryReceipt
  /** Answered as a run with the Knowledge tools (chat-knowledge.ts): what
   *  the run read, which the labels under the reply say. */
  knowledge?: Grounding
  /** L7-K0: what the run's searches found, from its record. */
  facts?: Facts | null
  /** L7-K1: where the run's searches found passages, library by library. */
  cited?: NonNullable<Parameters<typeof KnowledgeCitationCard>[0]['found']>
  /** Answered without Knowledge although its switch is On: main had no
   *  binding for a run, so the turn went to `/api/v1/chat`. */
  knowledgeInactive?: boolean
  /** K2: Knowledge is On but its server did not answer the check before this
   *  turn (unreachable / unauthorized / not_found): answered without it. */
  knowledgeDown?: KnowledgeDown
  /** The run that answered, when the turn went to /runs (Knowledge). */
  runId?: string
  /** M3: a failure's internal code, shown only under 「详情」. */
  detail?: string
  /** M1: what the run's document writes came to, from the daemon's record. */
  documents?: RunDocuments | null
  /** F3: the 「抽取结果」 card; the history keeps its text only. */
  extraction?: ExtractCardData
  /** L6-F2: the documents a delete turn may mean, to pick one; text only in the history. */
  deleteChoice?: DeleteCandidate[]
  /** L6-F5: the version a 「回到上一个版本」 turn may restore; text only in the history. */
  restore?: RestoreCardData
  /** L6-S05: the summary of a comparison, a line per change; text only in the history. */
  compare?: CompareCardData
  /** L8 DOC-4: the check before sending, with the clean copy's buttons; text only in the history. */
  deliver?: DeliverCardData
  /** L7-K2: upload after onboarding or 「上传文档」; the history keeps the text only. */
  knowledgeUpload?: { uploadIds: string[] }
}

/** UI-03: the conversation as the history keeps it. `stamps[i]` is when
 *  message i arrived; the history keeps text, delivery and the latency line,
 *  not the receipt or Knowledge labels. */
function toRecord(id: string, messages: Message[], stamps: readonly string[]): ChatHistoryRecord {
  return {
    format: CHAT_HISTORY_FORMAT,
    version: CHAT_HISTORY_VERSION,
    id,
    // Always the first prompt's: main keeps a rename or the model's title
    // already on file over it (UI-04).
    title: promptTitle(messages.find(m => m.role === 'user')?.content ?? ''),
    title_source: 'prompt',
    // Main keeps the group and the archive mark on file (UI-06).
    group_id: null,
    archived_at: null,
    // And its runs and what waits on the user (M4 A2), who started it and
    // what of it is unseen (M6 C1).
    runs: [], pending: [], initiator: 'user', unseen: [],
    created_at: stamps[0] ?? '',
    updated_at: stamps.slice(0, messages.length).reduce((a, b) => (b > a ? b : a), ''),
    messages: messages.map((m, i) => m.role === 'user'
      ? { role: 'user', content: m.content, at: stamps[i], delivered: m.delivery.kind === 'sent' }
      : { role: 'assistant', content: m.content, at: stamps[i], run_id: m.runId ?? null, latency: m.suffix ?? null }),
  }
}

/** A saved conversation back on the page. A prompt that never reached the
 *  model comes back "not sent", so it is not sent again as context. */
function fromRecord(record: ChatHistoryRecord): Message[] {
  return record.messages.map((m): Message => {
    if (m.role === 'user') return { role: 'user', content: m.content, delivery: { kind: m.delivered ? 'sent' : 'notSent' } }
    const reply: AssistantMessage = { role: 'assistant', content: m.content }
    if (m.latency) reply.suffix = { ...m.latency, tier: m.latency.tier === 'local' || m.latency.tier === 'remote' ? m.latency.tier : null }
    if (m.run_id) reply.runId = m.run_id
    return reply
  })
}

// M1-T12: one id per conversation, sent as `session_id` on every
// `/api/v1/chat` call so the daemon can key D1 personal-memory recall/retain
// to this thread. Mirrors `genCommandId` (voice/api.ts): a counter-based,
// monotonic-enough generator rather than `crypto.randomUUID`, so it never
// depends on that being available in every renderer/test environment.
let chatSessionIdCounter = 0
export function genChatSessionId(): string {
  chatSessionIdCounter += 1
  return `chat_${Date.now().toString(36)}_${chatSessionIdCounter}_${Math.random().toString(36).slice(2, 8)}`
}

type KnowledgeDown = 'unreachable' | 'unauthorized' | 'not_found'

export default function ChatPage() {
  const t = useT(chatMessages)
  const [messages, setMessages] = useState<Message[]>([])
  const [input, setInput] = useState('')
  const [loading, setLoading] = useState(false)
  // One id per conversation. ChatPage stays mounted for the whole app
  // lifetime (M1-T14), so a new conversation is started explicitly via
  // `newChat` below, which swaps in a fresh id.
  const [sessionId, setSessionId] = useState(() => genChatSessionId())
  // UI-06 D7: the open conversation is archived. It is shown, not continued:
  // the composer gives way to a note and a way to restore it.
  const history = useSyncExternalStore(subscribeHistory, getHistoryState)
  const archived = history.archiving.includes(sessionId)
    || history.items.some(item => item.id === sessionId && item.archived_at !== null)
  const archivedRef = useRef(archived)
  archivedRef.current = archived
  const bottomRef = useRef<HTMLDivElement>(null)
  // The turn rail: each turn's prompt element, keyed by message index, and
  // the turn being read, followed from the scroll position.
  const messagesRef = useRef<HTMLDivElement>(null)
  const promptRefs = useRef(new Map<number, HTMLDivElement>())
  const turns = useMemo(() => buildTurns(messages), [messages])
  // UI polish 07: runs whose answer is in the conversation; their task
  // cards do not show it a second time.
  const answeredRuns = useMemo(() => new Set(messages.flatMap(m => (m.role === 'assistant' && m.runId ? [m.runId] : []))), [messages])
  const [currentTurn, setCurrentTurn] = useState(0)
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const draft = useKnowledgeDraft()
  const requestInFlight = useRef(false)
  // L7-K2: onboarding in progress for this chat, keyed by session. Not the model.
  const onboard = useRef(new Map<string, OnboardStep>())
  /** L9-1b: the template each conversation's form fill uses, for the turns that answer it. */
  const formTemplate = useRef(new Map<string, DocumentRef>())
  // Knowledge on: turns go to /runs, in a real session created once.
  const [awaitingApproval, setAwaitingApproval] = useState(false)
  const runSession = useRef<string | null>(null)
  // The endpoint and knowledge bases the session's turns searched: a session
  // carries its earlier Knowledge results into later runs, which may send
  // them back to the Knowledge server (decision
  // 2026-10-30-knowledge-same-source-egress), so another server gets a new session.
  const runScope = useRef<string | null>(null)
  const awaitingConfirmation = messages.some(m => m.role === 'user' && m.delivery.kind === 'headroom')
  // UI-03: when each message arrived, and the record last written, so an
  // unchanged conversation (e.g. one just opened) is not written again.
  const stamps = useRef<string[]>([])
  const lastSaved = useRef('')
  // Bumped by every switch of conversation (new chat, opening one): a load
  // that finishes after a newer switch is dropped.
  const switchGen = useRef(0)
  // UI-04 T2: conversations whose title the model has been asked for (or
  // will not be: one opened from the history), so it is asked once at most.
  const titleAsked = useRef(new Set<string>())

  useEffect(() => { setChatBusy(loading) }, [loading])

  // Every change to a conversation with a prompt in it is saved, memory
  // paused or not (David 11-08).
  useEffect(() => {
    if (!messages.some(m => m.role === 'user')) return
    const now = new Date().toISOString()
    for (let i = 0; i < messages.length; i++) stamps.current[i] ??= now
    const record = toRecord(sessionId, messages, stamps.current)
    const body = JSON.stringify(record)
    if (body === lastSaved.current) return
    lastSaved.current = body
    // The page, not the save's completion, says which conversation is open.
    setActiveConversation(sessionId)
    void saveConversation(record).then(ok => {
      // Failed: write it again on the next change.
      if (!ok && lastSaved.current === body) lastSaved.current = ''
    })
  }, [messages, sessionId])

  // UI-04 T2: once a new conversation's first turn is answered (or its
  // second or third, when the prompts so far were too short to say much),
  // the model is asked for its title, once, in the background. It goes to
  // `/api/v1/chat` without `session_id`: no memory, local models only. A
  // failure leaves the first prompt's title; a rename always wins.
  useEffect(() => {
    if (loading || titleAsked.current.has(sessionId) || !historyAvailable()) return
    const turns = answeredTurns(messages)
    if (!titleDue(turns)) return
    titleAsked.current.add(sessionId)
    const id = sessionId
    if (getHistoryState().items.find(item => item.id === id)?.title_source === 'user') return
    void generateTitle(window.agent24.backendProxy, turns).then(title => {
      if (title) void titleConversation(id, title)
    })
  }, [messages, sessionId, loading])

  useEffect(() => {
    // Only the message list scrolls: scrollIntoView would also scroll every
    // ancestor that overflows, and once moved the window's own layout up.
    const box = messagesRef.current
    if (box) {
      if (typeof box.scrollTo === 'function') box.scrollTo({ top: box.scrollHeight, behavior: 'smooth' })
      else box.scrollTop = box.scrollHeight
    }
    // The page follows the newest turn; so does the rail, even when the chat
    // is too short to scroll and no scroll event comes.
    setCurrentTurn(Math.max(0, turns.length - 1))
  }, [messages, turns])

  const ONBOARD_APP_ERROR = '开通没有成功（App 出错），稍后再试。'

  /** A knowledge onboarding or upload turn. Null when this text is not one. */
  const knowledgeTurn = async (said: string): Promise<AssistantMessage | null> => {
    const api = window.agent24.knowledge
    const ongoing = onboard.current.get(sessionId)
    if (ongoing) {
      const step = nextOnboard(ongoing, said)
      if (step.run) {
        // The flow ends here whatever the run answers: a failed run is not read as the next answer.
        onboard.current.delete(sessionId)
        const run = api?.onboardRun
        if (!run) return { role: 'assistant', content: ONBOARD_APP_ERROR }
        const result = await run(step.run)
        return {
          role: 'assistant',
          content: resultReply(result, ongoing.types),
          ...(result.ok ? { knowledgeUpload: { uploadIds: [] } } : {}),
        }
      }
      if (step.state) onboard.current.set(sessionId, step.state)
      else onboard.current.delete(sessionId)
      return { role: 'assistant', content: step.reply }
    }
    if (asksOnboarding(said)) {
      const preflight = api?.onboardPreflight
      if (!preflight || !api?.onboardRun) return { role: 'assistant', content: ONBOARD_APP_ERROR }
      const blocked = preflightReply(await preflight())
      if (blocked) return { role: 'assistant', content: blocked }
      const started = beginOnboard(said)
      onboard.current.set(sessionId, started.state)
      return { role: 'assistant', content: started.reply }
    }
    if (asksUpload(said)) {
      return { role: 'assistant', content: '选择要放进知识库的文档：', knowledgeUpload: { uploadIds: [] } }
    }
    return null
  }

  /** `answer`: the model answers, never the extraction (the card's 「让助手回答」). */
  /** `chosen`: L6-F2, the document picked on a delete choice card. */
  const send = async (text: string, answer = false, chosen: { id: string; title: string } | null = null) => {
    if (!text.trim() || requestInFlight.current || awaitingConfirmation || archivedRef.current) return
    const userMsg: Message = { role: 'user', content: text.trim(), delivery: { kind: 'sending' } }
    const request: ChatRequest = {
      messages: [...messages.filter(m => m.role === 'assistant' || m.delivery.kind === 'sent'), userMsg]
        .map(m => ({ role: m.role, content: m.content })),
      session_id: sessionId,
    }
    draft.apply(request)
    setMessages(prev => [...prev, userMsg])
    setInput('')
    // L7-K2: onboarding and upload stay in the chat and never go to the model.
    // Not when `answer` is set (the extraction card's 「让助手回答」).
    if (!answer) {
      const said = text.trim()
      if (chosen === null && (onboard.current.has(sessionId) || asksOnboarding(said) || asksUpload(said))) {
        const index = messages.length
        requestInFlight.current = true
        setLoading(true)
        let reply: AssistantMessage
        try {
          reply = await knowledgeTurn(said) ?? { role: 'assistant', content: ONBOARD_APP_ERROR }
        } catch {
          reply = { role: 'assistant', content: ONBOARD_APP_ERROR }
        } finally {
          requestInFlight.current = false
          setLoading(false)
        }
        setDelivery(index, { kind: 'sent' })
        setMessages(prev => [...prev, reply])
        return
      }
    }
    // F1: a conversation about a document carries its text with each turn.
    const doc = documentOf(sessionId)
    // L6-F5: 「回到上一个版本」 — the App finds the version and offers it on a
    // card; it is restored only when the user clicks, never by the model.
    const restoreAsk = answer ? null : asksRestoreVersion(text)
    // No document bound and none named: the one the conversation last named
    // (the user's 「…《合同.docx》…存到原文件」 a turn ago), newest first.
    const lastNamed = (): string | null => {
      for (let i = messages.length - 1; i >= 0; i--) {
        const found = /《([^》]{1,200})》/.exec(messages[i]!.content)?.[1]?.trim()
        if (found) return found
      }
      return null
    }
    // L8 DOC-4: 「发给对方前帮我检查一下」「去掉修订和批注，导出一份 PDF 放桌面」
    // 「我上次发出去的是哪个版本？」 — the App checks and reads the receipts; a
    // clean copy is made only from the card's button, never by the model.
    const deliverAsk = answer ? null : asksDeliver(text, doc !== null, lastNamed() !== null)
    if (deliverAsk !== null && window.agent24.documents) {
      const api = window.agent24.documents
      const named = /《([^》]{1,200})》/.exec(text)?.[1]?.trim() ?? (deliverAsk.kind === 'receipts' ? null : lastNamed())
      requestInFlight.current = true
      setLoading(true)
      const index = messages.length
      let reply: AssistantMessage
      try {
        const subject = doc ?? (named ? await namedDocument(api, named).catch(() => '没能读取文档列表。') : null)
        if (typeof subject === 'string') reply = { role: 'assistant', content: subject }
        else if (deliverAsk.kind === 'receipts') reply = { role: 'assistant', content: await runReceipts(api, subject ? { id: subject.id, title: subject.title } : null) }
        else if (!subject) reply = { role: 'assistant', content: '请先打开要发送的文档，或用《文件名》说明是哪一份。' }
        else {
          const out = await runPreflight(api, { id: subject.id, title: subject.title }, deliverAsk)
          reply = { role: 'assistant', content: out.text, ...(out.card ? { deliver: out.card } : {}) }
        }
      } catch (e) {
        reply = { role: 'assistant', content: '没能完成检查，文档没有改动。', detail: e instanceof Error ? e.message : String(e) }
      } finally {
        requestInFlight.current = false
        setLoading(false)
      }
      setDelivery(index, { kind: 'sent' })
      setMessages(prev => [...prev, reply])
      return
    }
    const restoreOf = restoreAsk === null ? null : doc ? '' : /《([^》]{1,200})》/.exec(text)?.[1]?.trim() ?? lastNamed()
    if (restoreAsk !== null && restoreOf !== null && window.agent24.documents) {
      const api = window.agent24.documents
      requestInFlight.current = true
      setLoading(true)
      const index = messages.length
      let reply: AssistantMessage
      try {
        const subject = doc ?? await namedDocument(api, restoreOf).catch(() => "没能读取文档列表，这次没有恢复。")
        if (typeof subject === 'string') reply = { role: 'assistant', content: subject }
        else {
          const found = await restoreCardOf(api, subject, restoreAsk)
          reply = { role: 'assistant', content: found.text, ...(found.card ? { restore: found.card } : {}) }
        }
      } finally {
        requestInFlight.current = false
        setLoading(false)
      }
      setDelivery(index, { kind: 'sent' })
      setMessages(prev => [...prev, reply])
      return
    }
    // L6-S05: 「对比一下这两份」「这一版和上一版改了哪些地方」 — the App judges
    // which two, asks the module, and writes the summary from its changes.
    const compareAsk = answer ? null : asksCompare(text)
    if (compareAsk !== null && window.agent24.documents) {
      const api = window.agent24.documents
      requestInFlight.current = true
      setLoading(true)
      const index = messages.length
      const named: string[] = []
      // The turn's own titles first, then the conversation's, newest first.
      for (const said of [text, ...messages.map(m => m.content).reverse()]) {
        for (const m of [...said.matchAll(/《([^》]{1,200})》/g)]) {
          const title = m[1]!.trim()
          if (title && !named.includes(title) && named.length < 2) named.push(title)
        }
      }
      let reply: AssistantMessage
      try {
        const out = await runCompare(api, compareAsk, doc ? { id: doc.id, title: doc.title } : null, named)
        reply = { role: 'assistant', content: out.text, ...(out.card ? { compare: out.card } : {}) }
      } finally {
        requestInFlight.current = false
        setLoading(false)
      }
      setDelivery(index, { kind: 'sent' })
      setMessages(prev => [...prev, reply])
      return
    }
    // F3: 「把这份合同的甲方、乙方、金额、期限抽出来」 — the documents OS's extract
    // job reads the document; the model at most names the fields.
    const asked = answer ? null : extractionFields(text, doc !== null)
    const extractFrom = asked === null ? null : doc ? '' : /《([^》]{1,200})》/.exec(text)?.[1]?.trim() ?? null
    if (asked !== null && extractFrom !== null && window.agent24.documents) {
      const api = window.agent24.documents
      requestInFlight.current = true
      setLoading(true)
      const index = messages.length
      let reply: AssistantMessage
      try {
        const subject = doc ?? await namedDocument(api, extractFrom)
        const names = asked.length > 0 || typeof subject === 'string'
          ? asked
          : await fieldNamesFromModel((p) => askWithProxy(window.agent24.backendProxy, p), text).catch(() => [])
        if (typeof subject === 'string') reply = { role: 'assistant', content: subject }
        else if (names.length === 0) reply = { role: 'assistant', content: '请说要抽取哪些字段，例如「把甲方、乙方、金额抽出来」。' }
        else {
          const card = await extractFields(api, subject.id, names)
          reply = 'error' in card
            ? { role: 'assistant', content: card.pending ? card.error : `没能完成抽取：${card.error}`, ...(card.detail ? { detail: card.detail } : {}) }
            : { role: 'assistant', content: (subject.note ? `${subject.note}\n\n` : '') + extractText(card), extraction: card }
        }
      } catch (e) {
        reply = { role: 'assistant', content: '没能完成抽取。', detail: e instanceof Error ? e.message : String(e) }
      } finally {
        requestInFlight.current = false
        setLoading(false)
      }
      setDelivery(index, { kind: 'sent' })
      setMessages(prev => [...prev, reply])
      return
    }
    // D2b 正式版: granted documents tools go to the Agent; otherwise the
    // lightweight compose path below still handles a named split/merge.
    // E1b/D5a: 「按《模板》填写…」「把第三条改成…，用修订」 go the same way.
    // L8-S02: an answer to what a form fill asked goes back to it, with the form hint.
    const earlier = messages.filter((m) => m.role === 'user').map((m) => m.content)
    const form = asksFillForm(text, doc !== null) || continuesFormFill(earlier, text, doc !== null)
    // L9-1b (FU-252/253): the App tells the template from the material, and
    // answers a PDF form or an unclear template itself, before any model.
    let formSaid: string | null = null
    // An answer turn that itself asks for another documents operation (「把《X》删了」)
    // is that operation, not the form's (Codex L9-1b C1).
    // A title is never the operation (0.7.125: the answer 「《入职登记表模板.docx》」
    // has 模板 in it): judged with each 《…》 as 《》.
    const asksForm = asksFillForm(text, doc !== null)
    const otherOp = wantsDocTool(text.replace(/《[^》]{1,200}》/g, '《》'), doc !== null)
    if (form && chosen === null && (asksForm || !otherOp) && window.agent24.documents) {
      requestInFlight.current = true
      setLoading(true)
      let direct: string | null = null
      try {
        const entry = await resolveFormEntry(window.agent24.documents, {
          doc, text: text.trim(), earlier, asks: asksForm, remembered: formTemplate.current.get(sessionId) ?? null,
        })
        direct = formEntryReply(entry)
        if (entry.kind === 'fill') {
          formTemplate.current.set(sessionId, entry.template)
          formSaid = templateHint(entry.template) + (entry.material ? materialHint(entry.material) : '')
        }
      } finally {
        requestInFlight.current = false
        setLoading(false)
      }
      if (direct !== null) {
        setDelivery(messages.length, { kind: 'sent' })
        setMessages(prev => [...prev, { role: 'assistant', content: direct }])
        return
      }
    }
    if ((chosen !== null || form || wantsDocTool(text, doc !== null)) && window.agent24.documents?.agentAccess) {
      // Busy from before the wait: no second turn, no switch of conversation
      // while the grants are brought up to date (PR-Daemon #1909 ④).
      requestInFlight.current = true
      setLoading(true)
      const access = await window.agent24.documents.agentAccess('sync').catch(() => null)
      if (access?.enabled && access.ok !== false) {
        const last = request.messages[request.messages.length - 1]
        // L6-F2: the App names the one document a delete is for, never the model.
        const target = chosen ? { kind: 'one' as const, ...chosen }
          : asksDeleteDocument(text, doc !== null) ? await deleteTargetOf(window.agent24.documents, doc, text) : null
        if (target && target.kind !== 'one') {
          const index = messages.length
          setDelivery(index, { kind: 'sent' })
          setMessages(prev => [...prev, target.kind === 'none'
            ? { role: 'assistant', content: target.text }
            : { role: 'assistant', content: choiceText(target.name, target.total, target.candidates.length), deleteChoice: target.candidates }])
          requestInFlight.current = false
          setLoading(false)
          return
        }
        // 监工 0.7.123: the material the user named goes in by its id.
        const named = form && !target && formSaid === null ? formMaterial(earlier, text) : null
        const material = named ? await namedDocument(window.agent24.documents, named).catch(() => null) : null
        const said = formSaid ?? (material && typeof material !== 'string' ? materialHint(material) : '')
        if (last?.role === 'user') last.content = target ? deletePrompt(target, text.trim()) : docAgentPrompt(doc, text.trim(), form, said)
        await replyWithKnowledge(messages.length, request, false)
        return
      }
      requestInFlight.current = false
      setLoading(false)
    }
    // D3: 「总结/检查这份文档」 — same check as the document page, as a text report.
    if (/总结|摘要|检查|审查|核对/.test(text) && parseDocIntent(text) === null && window.agent24.documents) {
      const named = doc ? null : /[《「“"]([^》」”"]{1,200})[》」”"]/.exec(text)?.[1]?.trim() ?? null
      if (doc || named) {
        const api = window.agent24.documents
        requestInFlight.current = true
        setLoading(true)
        const index = messages.length
        try {
          const subject = doc ?? await namedDocument(api, named ?? '')
          if (typeof subject === 'string') {
            setDelivery(index, { kind: 'sent' })
            setMessages(prev => [...prev, { role: 'assistant', content: subject }])
          } else {
            let blocks = await readAllCheckBlocks((req) => api.request(req), subject.id, subject.revision)
            // A Word/Excel/PPT document has no text of its own: it is checked as the PDF it becomes.
            if (blocks.length === 0) {
              const pdf = await convertedPdf(api, subject.id, subject.title)
              if (pdf.ok && pdf.documentId) blocks = await readAllCheckBlocks((req) => api.request(req), pdf.documentId, 1)
            }
            const report = await checkDocument(
              { ask: (prompt) => askWithProxy(window.agent24.backendProxy, prompt) },
              subject.title,
              blocks,
            )
            setDelivery(index, { kind: 'sent' })
            setMessages(prev => [...prev, { role: 'assistant', content: (subject.note ? `${subject.note}\n\n` : '') + reportMarkdown(subject.title, report) }])
          }
        } catch (e) {
          setDelivery(index, { kind: 'sent' })
          setMessages(prev => [...prev, { role: 'assistant', content: `没能完成检查：${e instanceof Error ? e.message : String(e)}` }])
        } finally {
          requestInFlight.current = false
          setLoading(false)
        }
        return
      }
    }
    // D4: 「把这份翻译成英文」「翻译《X》」 — same translation as the document page.
    if (translatesDocument(text, doc !== null) && window.agent24.documents) {
      const named = doc ? null : /[《「“"]([^》」”"]{1,200})[》」”"]/.exec(text)?.[1]?.trim() ?? null
      if (doc || named) {
        const api = window.agent24.documents
        const lang: TargetLang = /中文|汉语/.test(text) && !/英文|英语/.test(text) ? 'zh' : 'en'
        const langName = lang === 'zh' ? '中文' : '英文'
        requestInFlight.current = true
        setLoading(true)
        const index = messages.length
        try {
          const subject = doc ?? await namedDocument(api, named ?? '')
          if (typeof subject === 'string') {
            setDelivery(index, { kind: 'sent' })
            setMessages(prev => [...prev, { role: 'assistant', content: subject }])
          } else {
            let blocks = await readAllCheckBlocks((req) => api.request(req), subject.id, subject.revision)
            if (blocks.length === 0) {
              const pdf = await convertedPdf(api, subject.id, subject.title)
              if (!pdf.ok) {
                const why = pdf.engineMissing
                  ? '需要先在文档页下载转换器（约 340 MB，只需一次）。'
                  : `没能完成翻译：${pdf.message}`
                setDelivery(index, { kind: 'sent' })
                setMessages(prev => [...prev, { role: 'assistant', content: why }])
                return
              }
              if (pdf.documentId) blocks = await readAllCheckBlocks((req) => api.request(req), pdf.documentId, 1)
            }
            const paras = paragraphsOf(blocks)
            if (paras.length === 0) {
              setDelivery(index, { kind: 'sent' })
              setMessages(prev => [...prev, { role: 'assistant', content: '这份文档没有读出可以翻译的文字。' }])
            } else {
              const translated = await translateParagraphs(
                { ask: (prompt) => askWithProxy(window.agent24.backendProxy, prompt) },
                paras,
                lang,
              )
              const out = await api.exportTranslation({
                title: subject.title,
                paragraphs: translated,
                lang,
              })
              const reply = out.ok
                ? `已翻译《${subject.title}》为${langName}：下载/${out.docxName}；PDF 已加入文档库。`
                : out.code === 'engine_unavailable'
                  ? '需要先在文档页下载转换器（约 340 MB，只需一次）。'
                  : `没能完成翻译：${out.message ?? out.code ?? ''}`
              setDelivery(index, { kind: 'sent' })
              setMessages(prev => [...prev, { role: 'assistant', content: (subject.note ? `${subject.note}\n\n` : '') + reply }])
            }
          }
        } catch (e) {
          setDelivery(index, { kind: 'sent' })
          setMessages(prev => [...prev, { role: 'assistant', content: `没能完成翻译：${e instanceof Error ? e.message : String(e)}` }])
        } finally {
          requestInFlight.current = false
          setLoading(false)
        }
        return
      }
    }
    // D2b: 「拆出第 2-4 页」「和《X》合并」 about that document are done here,
    // not asked of the model (which does not call tools reliably).
    // Outside a document's conversation, the one the turn names in 《》.
    const intent = parseDocIntent(text)
    const named = !doc && intent && intent.kind !== 'unclear' ? subjectOf(text, intent) : null
    if ((doc || named) && intent && window.agent24.documents) {
      const api = window.agent24.documents
      requestInFlight.current = true
      setLoading(true)
      const index = messages.length
      try {
        const subject = doc ?? await namedDocument(api, named ?? '')
        const reply = typeof subject === 'string'
          ? subject
          : (subject.note ? `${subject.note}\n\n` : '') + await runDocIntent(api, subject, intent)
        setDelivery(index, { kind: 'sent' })
        setMessages(prev => [...prev, { role: 'assistant', content: reply }])
      } finally {
        requestInFlight.current = false
        setLoading(false)
      }
      return
    }
    const read = window.agent24.documents?.request
    // Loop 3 FU: a question outside a document's conversation that names a
    // document in 《》 is asked about that document, as F1 asks about this one.
    const titled = !doc && window.agent24.documents ? /[《「“"]([^》」”"]{1,200})[》」”"]/.exec(text)?.[1]?.trim() ?? null : null
    if ((doc || titled) && read && window.agent24.documents) {
      // PR-Daemon #1909 ④: busy while the document is read, page by page;
      // released with no await between here and requestReply, which takes it again.
      requestInFlight.current = true
      setLoading(true)
      let about = doc
      let body: Awaited<ReturnType<typeof documentText>> | null = null
      try {
        if (!about && titled) {
          // Not a document of the library (or several are): asked as it was.
          const named = await namedDocument(window.agent24.documents, titled).catch(() => null)
          about = named && typeof named !== 'string' ? named : null
        }
        if (about) body = await documentText(about, read as Parameters<typeof documentText>[1]).catch(() => null)
      } finally {
        requestInFlight.current = false
      }
      const last = request.messages[request.messages.length - 1]
      if (about && body && body.text.trim() && last?.role === 'user') last.content = withDocument(about, body, last.content)
    }
    // M1: a turn sent as the user said it (no document) may get the Knowledge hint.
    const said = request.messages[request.messages.length - 1]
    await requestReply(messages.length, request, said?.role === 'user' && said.content === text.trim())
  }

  // Issue #1517: start a fresh conversation. Disabled while a reply is in
  // flight (the chat call is a single blocking IPC with no abort handle, so
  // the button is disabled rather than aborting) — hence no stale reply can
  // land in the new conversation.
  // A skill is chosen only for a conversation that has no turns yet, so
  // every turn of a skill conversation goes to /runs from the first one.
  const startNewChat = (skill: string | null) => {
    if (requestInFlight.current) return
    switchGen.current += 1
    setMessages([])
    setInput('')
    const fresh = genChatSessionId()
    if (skill) setConversationSkill(fresh, skill)
    setSessionId(fresh)
    setCurrentTurn(0)
    stamps.current = []
    lastSaved.current = ''
    setActiveConversation(null)
    // Knowledge turns run in their own session (`runSession`); a new
    // conversation must not carry the old one's turns into its runs.
    runSession.current = null
  }
  const newChat = () => startNewChat(null)

  // UI-03: open a saved conversation from the history list. Not while a reply
  // is in flight, for the same reason as `newChat`. It continues under its
  // own session_id; Knowledge turns start a new run session.
  const openSaved = async (id: string) => {
    if (requestInFlight.current) return
    const gen = ++switchGen.current
    // Already open: nothing to load (the page may be newer than the file);
    // the bump above still drops any other conversation's load under way.
    // Asked to open it, the user sees it: what is unseen is marked seen, also
    // after a mark that failed while it was open (M6 C2, Codex C2 R1 C1).
    if (id === sessionId && messages.length > 0) {
      void markConversationSeen(id)
      return
    }
    const record = await loadConversation(id)
    if (!record || requestInFlight.current || gen !== switchGen.current) return
    const restored = fromRecord(record)
    // Its title was settled when it was new; continuing it asks for none.
    titleAsked.current.add(record.id)
    stamps.current = record.messages.map(m => m.at)
    lastSaved.current = JSON.stringify(toRecord(record.id, restored, stamps.current))
    setMessages(restored)
    setInput('')
    setSessionId(record.id)
    runSession.current = null
    // A reopened conversation continues without a skill: its earlier turns
    // are not in a run session, so a skill run could not see them. To use a
    // skill, start a new conversation.
    setConversationSkill(record.id, null)
    setActiveConversation(record.id)
    // M6 C1: opened, so nothing of it is unseen any more.
    void markConversationSeen(record.id)
  }
  useEffect(() => registerChatPage(id => void openSaved(id), newChat))

  // F1: asked from a document's page: a new conversation about it (this one
  // when it is still empty).
  const askedDocument = useSyncExternalStore(subscribeDocContext, pendingDocument)
  const boundDocument = useSyncExternalStore(subscribeDocContext, () => documentOf(sessionId))
  useEffect(() => {
    if (!askedDocument || requestInFlight.current) return
    if (messages.length > 0) startNewChat(null)
    else bindPendingDocument(sessionId)
  }, [askedDocument, sessionId, messages.length])

  const setDelivery = (index: number, delivery: Delivery) => {
    setMessages(prev => prev.map((m, i) => i === index && m.role === 'user' ? { ...m, delivery } : m))
  }

  const requestReply = async (index: number, request: ChatRequest, plain = false) => {
    // An archived conversation is read-only (UI-06 D7): nothing is sent, a
    // retry neither, until it is restored.
    if (requestInFlight.current || archivedRef.current) return
    requestInFlight.current = true
    setDelivery(index, { kind: 'sending' })
    setLoading(true)

    // ME4-desktop-model-ui: total wall-clock time for THIS call, timed on the
    // renderer side with performance.now() (monotonic, sub-ms) — `/api/v1/chat`
    // is a single blocking call today (no streaming), so this IS the reply's
    // whole latency; extractServerReported still gets first crack at it in
    // case a future daemon response reports its own (see chat-latency.ts).
    const sentAt = performance.now()

    // Asked of main on every turn: it may have turned On since the page opened.
    const knowledge = await window.agent24.knowledge?.status().then((status) => status, () => null)
    // K2: a server the last check could not reach is asked once more; still
    // down, the turn is answered without Knowledge and says so, never as if
    // the knowledge bases had nothing (the demo stack, 11-17: 25 hours unseen).
    let knowledgeDown: KnowledgeDown | null = null
    const down = (state: string | undefined): state is KnowledgeDown => state === 'unreachable' || state === 'unauthorized' || state === 'not_found'
    if (knowledge?.enabled === true && down(knowledge.reachability?.state)) {
      const fresh = await window.agent24.knowledge?.check?.().then((s) => s, () => null)
      const state = fresh?.reachability?.state ?? knowledge.reachability?.state
      if (down(state)) knowledgeDown = state
    }
    // A chosen skill rides on RunCreate, which `/api/v1/chat` does not have:
    // such a turn goes to the run path with Knowledge on or off.
    const skill = skillForConversation(sessionId)
    if ((knowledge?.active === true && knowledgeDown === null) || skill) {
      const scope = knowledge?.active === true ? JSON.stringify([knowledge.endpoint, knowledge.knowledgeBaseIds]) : 'no-knowledge'
      if (runScope.current !== scope) {
        runSession.current = null
        runScope.current = scope
      }
      const last = request.messages[request.messages.length - 1]
      // PR-Daemon #1976: 「系统已先查过」 only on a turn that is searched first.
      const asked = knowledge?.active === true && plain && last?.role === 'user' && knowledgeQuery(last.content) !== null
        ? { ...request, messages: [...request.messages.slice(0, -1), { ...last, content: knowledgePrompt(last.content) }] }
        : request
      // M1 retrieval first: a plain turn (no document, no document tool, not
      // 「记住…」) is searched with the user's words before the model is asked.
      const query = knowledge?.active === true && plain && last?.role === 'user' ? knowledgeQuery(last.content) : null
      await replyWithKnowledge(index, asked, knowledge?.active === true, query)
      return
    }
    // On but not in effect (no binding, e.g. a daemon started from the CLI):
    // the reply still comes, and says it did not use Knowledge.
    const knowledgeInactive = knowledge?.enabled === true && knowledgeDown === null

    // Call backend LLM gateway via IPC proxy
    try {
      const res = await window.agent24.backendProxy({
        method: 'POST',
        path: '/api/v1/chat',
        body: request,
      })
      if (!res.ok) {
        // v1 error envelope { error: { code, message } }
        const err = (res.data as { error?: { code?: string; message?: string } } | null)?.error
        if (err?.code === 'idoris_headroom_confirmation_required') {
          setDelivery(index, { kind: 'headroom', request })
          return
        }
        if (err?.code === 'idoris_supervisor_busy') {
          setDelivery(index, { kind: 'notSent' })
          setMessages(prev => [...prev, { role: 'assistant', content: t('reply.supervisorBusy') }])
          return
        }
        // The catch below picks its message by `code`. `proxyToBackend`
        // answers `backend_not_ready` (503) whenever `daemonConnection()`
        // is null, i.e. the daemon's ready line never arrived or left no
        // credential (apps/desktop/src/main/ipc/index.ts). That is a daemon-startup
        // failure, not an unreachable model service, so it must not read
        // like one — 2026-10-16 install acceptance followed the wrong one.
        const failure = new Error(err?.message ?? `HTTP ${res.status}`) as Error & { code?: string }
        failure.code = err?.code
        throw failure
      }
      const totalMs = performance.now() - sentAt
      const reply = (res.data as { message?: { content?: string } })?.message?.content ?? t('reply.empty')
      const server = extractServerReported(res.data)
      // M1-T14: a deterministic receipt — present only when this prompt was
      // an explicit "记住…" and session_id was set; absent otherwise.
      const memoryReceiptRaw = (res.data as { memory_receipt?: unknown } | null)?.memory_receipt
      const memoryReceipt =
        memoryReceiptRaw === 'saved' || memoryReceiptRaw === 'paused_not_saved' || memoryReceiptRaw === 'failed' || memoryReceiptRaw === 'partial'
          ? memoryReceiptRaw
          : undefined
      // Review M3: ONLY the server's own reported model_id — never the
      // topbar's daemon-default guess. `/api/v1/chat` now (post-M3) always
      // reports the model_id the provider that actually served this call
      // gave, or null when the provider didn't say; either way this suffix
      // reflects what actually happened, never a name that might not be it.
      const suffix: ReplyLatency = {
        modelName: server.modelId,
        tier: server.tier,
        firstTokenMs: server.firstTokenMs,
        totalMs: server.totalMs ?? totalMs,
      }
      setDelivery(index, { kind: 'sent' })
      setMessages(prev => [...prev, { role: 'assistant', content: reply, suffix, memoryReceipt, knowledgeInactive, ...(knowledgeDown ? { knowledgeDown } : {}) }])
    } catch (e) {
      setDelivery(index, { kind: 'notSent' })
      // `failure.code` above is the only input: `backend_not_ready` gets the
      // daemon-startup message, everything else the generic request failure.
      const content = (e as { code?: string } | null)?.code === 'backend_not_ready'
        ? t('reply.backendNotReady', { error: String(e) })
        : t('reply.connectionError', { error: String(e) })
      setMessages(prev => [...prev, { role: 'assistant', content }])
    } finally {
      requestInFlight.current = false
      setLoading(false)
    }
  }

  // The turn as a run: only the new prompt goes. The run sees earlier turns
  // only through its session's memory (agent24-agent `session_context`): none
  // while memory is off, and never the turns that went to /api/v1/chat.
  const replyWithKnowledge = async (index: number, request: ChatRequest, withKnowledge: boolean, query: string | null = null) => {
    const prompt = request.messages[request.messages.length - 1]?.content ?? ''
    const sentAt = performance.now()
    const conversation = sessionId
    let runId: string | null = null
    try {
      const proxy = window.agent24.backendProxy
      const ask = async () => {
        runSession.current ??= await createChatSession(proxy)
        return runSession.current === null
          ? { kind: 'failed' as const, code: 'session_unavailable', message: 'session_unavailable' }
          : await askWithKnowledge(prompt, runSession.current, {
            proxy,
            onRunStarted: id => { runId = id },
            // M4 A2: a run that waits on an approval is a task of this
            // conversation from then on.
            onAwaitingApproval: waiting => {
              setAwaitingApproval(waiting)
              if (runId && promotionDue({ status: waiting ? 'awaiting_approval' : 'running', replyEnded: false })) void promoteRun(conversation, runId)
            },
          }, request.idoris_headroom_confirmed === true, skillForConversation(conversation), query)
      }
      let turn = await ask()
      // The daemon ties a session to one activation of the binding; after a
      // change it refuses the old session, so the turn goes to a new one.
      if (turn.kind === 'failed' && turn.code === 'knowledge_session_rebound') {
        runSession.current = null
        turn = await ask()
      }
      // Same inline confirmation as a /api/v1/chat turn; confirming sends
      // the turn again with the flag set.
      if (turn.kind === 'failed' && turn.code === 'idoris_headroom_confirmation_required') {
        setDelivery(index, { kind: 'headroom', request })
        return
      }
      // The skill left the active list under the user: stop naming it.
      if (turn.kind === 'failed' && turn.code === 'skill_not_active') {
        const gone = skillForConversation(conversation)
        setConversationSkill(conversation, null)
        setDelivery(index, { kind: 'notSent' })
        setMessages(prev => [...prev, { role: 'assistant', content: t('skill.notActive', { name: gone ?? '' }) }])
        return
      }
      if (turn.kind === 'answer') {
        const suffix: ReplyLatency = { modelName: null, tier: null, firstTokenMs: null, totalMs: performance.now() - sentAt }
        setDelivery(index, { kind: 'sent' })
        const checked = withKnowledge ? grounding(turn.text, turn.knowledge) : undefined
        // UI polish 03 (David): 「没有检索知识库」 only under a turn the system
        // searches first; writing, 「记住…」 and a document's rewrite or split
        // are not, so the line said nothing there. Unread sources still show.
        const facts = withKnowledge ? knowledgeFacts(turn.knowledge) : null
        // A refused search says so itself; 「没有检索知识库」 beside it would say it twice.
        const knowledge = checked?.kind === 'notSearched' && (query === null || facts?.kind === 'refused') && checked.unread.length === 0 ? undefined : checked
        setMessages(prev => [...prev, { role: 'assistant', content: turn.text || t('reply.empty'), suffix, knowledge, runId: turn.runId, documents: formClaimUnbacked(prompt, turn.text, turn.documents), facts, cited: withKnowledge ? turn.knowledge?.found : undefined }])
      } else if (turn.kind === 'ongoing') {
        // M4 A4: the turn stopped waiting, the run goes on as this
        // conversation's task; its card shows how it does.
        // One that could not be recorded has no card: it is stopped, as
        // before A4, rather than left running unseen (Codex A4 C1).
        if (!promotionDue({ status: turn.status, replyEnded: true }) || !(await promoteRun(conversation, turn.runId))) {
          forgetPromotion(conversation, turn.runId)
          await proxy({ method: 'POST', path: `/api/v1/runs/${encodeURIComponent(turn.runId)}/cancel` }).catch(() => {})
          setDelivery(index, { kind: 'notSent' })
          setMessages(prev => [...prev, { role: 'assistant', content: t('fail.timeout'), detail: 'run_timeout' }])
          return
        }
        setDelivery(index, { kind: 'sent' })
        // The next turn goes to a new session: this one's run is not done,
        // and a turn beside it would not see its answer (Knowledge's review).
        runSession.current = null
        setMessages(prev => [...prev, { role: 'assistant', content: t('task.ongoing') }])
      } else {
        // M3: the user's words, the code under 「详情」. A turn the user
        // stopped was sent: it is not 「未发送」 (General, 0.6.73).
        const kind = failureKind(turn.code)
        // General 0.6.93: a turn the daemon took and ran was sent, whatever
        // came of it (the model unavailable, a timeout, stopped); only one
        // that never started is 「未发送」.
        setDelivery(index, { kind: kind === 'notStarted' ? 'notSent' : 'sent' })
        setMessages(prev => [...prev, { role: 'assistant', content: t(`fail.${kind}`), detail: turn.code === turn.message ? turn.code : `${turn.code}: ${turn.message}` }])
      }
    } catch (e) {
      setDelivery(index, { kind: 'notSent' })
      setMessages(prev => [...prev, { role: 'assistant', content: t('fail.other'), detail: String(e) }])
    } finally {
      setAwaitingApproval(false)
      requestInFlight.current = false
      setLoading(false)
    }
  }

  const followScroll = () => {
    const box = messagesRef.current
    if (!box) return
    const tops = turns.map(turn => promptRefs.current.get(turn.messageIndex)?.offsetTop ?? 0)
    setCurrentTurn(activeTurn(tops, box.scrollTop, box.clientHeight, box.scrollHeight))
  }

  const jumpToTurn = (turn: number) => {
    const target = turns[turn]
    if (!target) return
    promptRefs.current.get(target.messageIndex)?.scrollIntoView({ behavior: 'smooth', block: 'start' })
    setCurrentTurn(turn)
  }

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Loop 3 FU: an Enter that ends an input method's composition (pinyin, or
    // macOS committing a suggested word) is that, not 「send」: half an
    // English sentence went out while typing it (General 11-19).
    if (e.nativeEvent.isComposing || e.keyCode === 229) return
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      // Loop 4: what the box holds now, not the last render's state: keys
      // typed fast (the computer-use tool, a paste then Enter) can reach this
      // Enter before React has rendered the text before it.
      const typed = e.currentTarget.value
      if (typed !== input) setInput(typed)
      void send(typed)
    }
  }

  return (
    <div className="chat-layout">
      <div className="chat-toolbar">
        <button className="btn" disabled={loading} onClick={newChat}>{t('newChat')}</button>
        <SkillPicker conversation={sessionId} hasMessages={messages.length > 0} busy={loading} onStartWithSkill={startNewChat} />
      </div>
      {turns.length >= 2 && <TurnRail turns={turns} active={currentTurn} onSelect={jumpToTurn} />}
      <div className="chat-messages" ref={messagesRef} onScroll={followScroll}>
        {messages.length === 0 && (
          <div className="chat-empty">
            <img className="chat-empty-logo" src={logoWelcome} alt="" />
            <h2>Agent24</h2>
            <p>{t('welcome.subtitle')}</p>
            <div className="chat-suggestions">
              {(['suggestion.analyze', 'suggestion.summarize', 'suggestion.introduce', 'suggestion.translate'] as const).map(key => (
                <button key={key} className="suggestion-chip" onClick={() => void send(t(key))}>
                  {t(key)}
                </button>
              ))}
            </div>
          </div>
        )}
        {messages.map((m, i) => (
          <div key={i} className={`message ${m.role}`} ref={m.role === 'user'
            ? (el: HTMLDivElement | null) => { if (el) promptRefs.current.set(i, el); else promptRefs.current.delete(i) }
            : undefined}>
            <div className="message-avatar">
              {m.role === 'user' ? '👤' : '🤖'}
            </div>
            <div className="message-bubble">
              {m.role === 'assistant' && m.extraction ? <ExtractCard card={m.extraction} onAnswer={loading || archived ? undefined : () => {
                const asked = messages.slice(0, i).reverse().find(p => p.role === 'user')
                if (asked) void send(asked.content, true)
              }} />
                : m.role === 'assistant' && m.compare ? <><Markdown text={m.content} /><CompareCard card={m.compare} /></>
                : m.role === 'assistant' && m.deliver ? <><Markdown text={m.content} /><DeliverCard card={m.deliver} /></>
                : m.role === 'assistant' && m.restore ? <><Markdown text={m.content} /><RestoreCard card={m.restore} onRestore={loading || archived ? undefined : () => {
                  const card = m.restore!
                  setMessages(prev => prev.map((p, j) => (j === i && p.role === 'assistant' ? { ...p, restore: undefined } : p)))
                  void restoreVersion(window.agent24.documents, card).then(said => setMessages(prev => [...prev, { role: 'assistant', content: said }]))
                }} /></>
                : m.role === 'assistant' && m.deleteChoice ? <><Markdown text={m.content} /><DeleteChoice candidates={m.deleteChoice} onPick={loading || archived ? undefined : (c, n) => {
                  void send(`删除第 ${n} 份：《${c.title}》（导入于 ${c.importedAt}）`, false, { id: c.id, title: c.title })
                }} /></>
                : m.role === 'assistant' && m.knowledgeUpload ? <><Markdown text={m.content} /><KnowledgeUploadCard uploadIds={m.knowledgeUpload.uploadIds} onPicked={(ids) => {
                  if (ids.length === 0) return
                  setMessages(prev => prev.map((p, j) => (j === i && p.role === 'assistant' && p.knowledgeUpload
                    ? { ...p, knowledgeUpload: { uploadIds: [...p.knowledgeUpload.uploadIds, ...ids] } }
                    : p)))
                }} /></>
                : m.role === 'assistant' ? <Markdown text={withoutToolNames(m.content)} /> : m.content}
              {m.role === 'assistant' && m.detail && (
                <details className="message-detail"><summary>{t('fail.detail')}</summary>{m.detail}</details>
              )}
              {m.role === 'user' && m.delivery.kind === 'headroom' && (
                <div role="group" aria-label={t('headroom.explanation')} style={{ marginTop: 8 }}>
                  <p role="status">{t('headroom.explanation')}</p>
                  <button className="btn" disabled={loading || m.delivery.request.idoris_headroom_confirmed === true}
                    onClick={() => {
                      if (m.delivery.kind === 'headroom' && !m.delivery.request.idoris_headroom_confirmed) {
                        void requestReply(i, { ...m.delivery.request, idoris_headroom_confirmed: true })
                      }
                    }}>{t('headroom.confirm')}</button>
                  <button className="btn" disabled={loading} onClick={() => setDelivery(i, { kind: 'notSent' })}>{t('headroom.cancel')}</button>
                </div>
              )}
              {m.role === 'user' && m.delivery.kind === 'notSent' && <div role="status">{t('status.notSent')}</div>}
              {m.role === 'user' && m.delivery.kind === 'sent' && !archived && !loading && <HandOver conversation={sessionId} prompt={m.content} />}
              {m.role === 'assistant' && m.suffix && (
                <div style={{ fontSize: 11, color: 'var(--muted)', marginTop: 4 }}>{formatReplySuffix(m.suffix)}</div>
              )}
              {/* Lines the daemon's record of the run backs, not the reply:
                  the reply may name sources the run never read. */}
              {m.role === 'assistant' && m.knowledge && (
                <div style={{ fontSize: 11, color: 'var(--muted)', marginTop: 2 }}>
                  {m.knowledge.kind === 'read' ? t('knowledge.via') : t(`knowledge.${m.knowledge.kind}`)}
                </div>
              )}
              {m.role === 'assistant' && m.facts && <KnowledgeFactsRow facts={m.facts} />}
              {m.role === 'assistant' && m.cited && m.cited.length > 0 && <KnowledgeCitationCard found={m.cited} />}
              {m.role === 'assistant' && m.knowledgeInactive && (
                <div style={{ fontSize: 11, color: 'var(--muted)', marginTop: 2 }}>{t('knowledge.inactive')}</div>
              )}
              {m.role === 'assistant' && m.documents && <DocumentsOutcome documents={m.documents} />}
              {m.role === 'assistant' && m.knowledgeDown && (
                <div role="note" style={{ fontSize: 12, color: 'var(--danger, #c0392b)', marginTop: 2 }}>{t(`knowledge.down.${m.knowledgeDown}`)}</div>
              )}
              {m.role === 'assistant' && m.knowledge && m.knowledge.unread.length > 0 && (
                <div style={{ fontSize: 11, color: 'var(--muted)', marginTop: 2 }}>
                  {t('knowledge.unread', { ids: m.knowledge.unread.join(', ') })}
                </div>
              )}
              {/* M1-T14: a deterministic system line, visually distinct from
                  (and never mistaken for) the model's own reply text. */}
              {m.role === 'assistant' && m.memoryReceipt && (
                <div style={{ fontSize: 11, color: 'var(--muted)', marginTop: 2 }}>
                  {t(`memory.${m.memoryReceipt}`)}
                </div>
              )}
            </div>
          </div>
        ))}
        {loading && (
          <div className="message assistant">
            <div className="message-avatar">🤖</div>
            <div className="message-bubble" style={{ color: 'var(--muted)' }}>
              {awaitingApproval ? t('knowledge.awaitingApproval') : t('status.thinking')}
            </div>
          </div>
        )}
        <TaskCards conversation={sessionId} answered={answeredRuns} />
        <div ref={bottomRef} />
      </div>

      {!archived && <DraftAttachments draft={draft} />}
      {!archived && boundDocument && (
        <div className="chat-draft">
          <div className="chat-draft-chip" title={t('doc.context')}>
            {'📄 '}{boundDocument.title}{' · '}{t('doc.context')}
            <button className="chat-draft-remove" type="button" aria-label={t('doc.remove')} onClick={() => unbindDocument(sessionId)}>{'×'}</button>
          </div>
        </div>
      )}
      {archived ? (
        <div className="chat-input-bar chat-archived-bar" role="status">
          <span className="chat-archived-notice">{t('archived.notice')}</span>
          {/* Not while main is still archiving it: the page leaves it then. */}
          <button className="btn" disabled={history.archiving.includes(sessionId)}
            onClick={() => void restoreConversation(sessionId)}>{t('archived.restore')}</button>
        </div>
      ) : <div className="chat-input-bar">
        <textarea
          ref={textareaRef}
          className="chat-input"
          placeholder={t('input.placeholder')}
          value={input}
          onChange={e => setInput(e.target.value)}
          onKeyDown={handleKeyDown}
          rows={1}
        />
        <button
          className="send-btn"
          disabled={!input.trim() || loading || awaitingConfirmation}
          onClick={() => void send(input)}
        >
          ↑
        </button>
      </div>}
    </div>
  )
}
