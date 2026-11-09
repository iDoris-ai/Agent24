import { useState, useRef, useEffect, useMemo, useSyncExternalStore } from 'react'
import { formatReplySuffix, extractServerReported, type ReplyLatency } from './chat-latency'
import logoWelcome from '../assets/logo-welcome.png'
import { useT } from '../i18n'
import { chatMessages } from './Chat.i18n'
import type { components } from '@agent24/api-client'
import { askWithKnowledge, createChatSession } from './chat-knowledge'
import { grounding, type Grounding } from './knowledge-grounding'
import { TurnRail } from './chat/TurnRail'
import { activeTurn, buildTurns } from './chat/turns'
import { answeredTurns, generateTitle, promptTitle, titleDue } from './chat/title'
import { CHAT_HISTORY_FORMAT, CHAT_HISTORY_VERSION, promotionDue, type ChatHistoryRecord } from '../../shared/chat-history'
import { DraftAttachments, useKnowledgeDraft } from './chat/knowledge-draft'
import { HandOver, TaskCards } from './chat/TaskCard'
import {
  getHistoryState, historyAvailable, loadConversation, promoteRun, registerChatPage, restoreConversation, saveConversation,
  setActiveConversation, setChatBusy, subscribeHistory, titleConversation,
} from './chat/history'

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
  /** Answered without Knowledge although its switch is On: main had no
   *  binding for a run, so the turn went to `/api/v1/chat`. */
  knowledgeInactive?: boolean
  /** The run that answered, when the turn went to /runs (Knowledge). */
  runId?: string
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
    // And its runs and what waits on the user (M4 A2).
    runs: [], pending: [],
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
  const [currentTurn, setCurrentTurn] = useState(0)
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const draft = useKnowledgeDraft()
  const requestInFlight = useRef(false)
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
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' })
    // The page follows the newest turn; so does the rail, even when the chat
    // is too short to scroll and no scroll event comes.
    setCurrentTurn(Math.max(0, turns.length - 1))
  }, [messages, turns])

  const send = async (text: string) => {
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
    await requestReply(messages.length, request)
  }

  // Issue #1517: start a fresh conversation. Disabled while a reply is in
  // flight (the chat call is a single blocking IPC with no abort handle, so
  // the button is disabled rather than aborting) — hence no stale reply can
  // land in the new conversation.
  const newChat = () => {
    if (requestInFlight.current) return
    switchGen.current += 1
    setMessages([])
    setInput('')
    setSessionId(genChatSessionId())
    setCurrentTurn(0)
    stamps.current = []
    lastSaved.current = ''
    setActiveConversation(null)
    // Knowledge turns run in their own session (`runSession`); a new
    // conversation must not carry the old one's turns into its runs.
    runSession.current = null
  }

  // UI-03: open a saved conversation from the history list. Not while a reply
  // is in flight, for the same reason as `newChat`. It continues under its
  // own session_id; Knowledge turns start a new run session.
  const openSaved = async (id: string) => {
    if (requestInFlight.current) return
    const gen = ++switchGen.current
    // Already open: nothing to load (the page may be newer than the file);
    // the bump above still drops any other conversation's load under way.
    if (id === sessionId && messages.length > 0) return
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
    setActiveConversation(record.id)
  }
  useEffect(() => registerChatPage(id => void openSaved(id), newChat))

  const setDelivery = (index: number, delivery: Delivery) => {
    setMessages(prev => prev.map((m, i) => i === index && m.role === 'user' ? { ...m, delivery } : m))
  }

  const requestReply = async (index: number, request: ChatRequest) => {
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
    if (knowledge?.active === true) {
      const scope = JSON.stringify([knowledge.endpoint, knowledge.knowledgeBaseIds])
      if (runScope.current !== scope) {
        runSession.current = null
        runScope.current = scope
      }
      await replyWithKnowledge(index, request)
      return
    }
    // On but not in effect (no binding, e.g. a daemon started from the CLI):
    // the reply still comes, and says it did not use Knowledge.
    const knowledgeInactive = knowledge?.enabled === true

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
      setMessages(prev => [...prev, { role: 'assistant', content: reply, suffix, memoryReceipt, knowledgeInactive }])
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
  const replyWithKnowledge = async (index: number, request: ChatRequest) => {
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
          }, request.idoris_headroom_confirmed === true)
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
      if (turn.kind === 'answer') {
        const suffix: ReplyLatency = { modelName: null, tier: null, firstTokenMs: null, totalMs: performance.now() - sentAt }
        setDelivery(index, { kind: 'sent' })
        const knowledge = grounding(turn.text, turn.knowledge)
        setMessages(prev => [...prev, { role: 'assistant', content: turn.text || t('reply.empty'), suffix, knowledge, runId: turn.runId }])
      } else if (turn.kind === 'ongoing') {
        // M4 A4: the turn stopped waiting, the run goes on as this
        // conversation's task; its card shows how it does.
        // One that could not be recorded has no card: it is stopped, as
        // before A4, rather than left running unseen (Codex A4 C1).
        if (!promotionDue({ status: turn.status, replyEnded: true }) || !(await promoteRun(conversation, turn.runId))) {
          await proxy({ method: 'POST', path: `/api/v1/runs/${encodeURIComponent(turn.runId)}/cancel` }).catch(() => {})
          setDelivery(index, { kind: 'notSent' })
          setMessages(prev => [...prev, { role: 'assistant', content: t('knowledge.failed', { error: 'run_timeout' }) }])
          return
        }
        setDelivery(index, { kind: 'sent' })
        // The next turn goes to a new session: this one's run is not done,
        // and a turn beside it would not see its answer (Knowledge's review).
        runSession.current = null
        setMessages(prev => [...prev, { role: 'assistant', content: t('task.ongoing') }])
      } else {
        setDelivery(index, { kind: 'notSent' })
        setMessages(prev => [...prev, { role: 'assistant', content: t('knowledge.failed', { error: turn.message }) }])
      }
    } catch (e) {
      setDelivery(index, { kind: 'notSent' })
      setMessages(prev => [...prev, { role: 'assistant', content: t('knowledge.failed', { error: String(e) }) }])
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
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      void send(input)
    }
  }

  return (
    <div className="chat-layout">
      <div className="chat-toolbar">
        <button className="btn" disabled={loading} onClick={newChat}>{t('newChat')}</button>
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
              {m.content}
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
              {m.role === 'assistant' && m.knowledgeInactive && (
                <div style={{ fontSize: 11, color: 'var(--muted)', marginTop: 2 }}>{t('knowledge.inactive')}</div>
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
        <TaskCards conversation={sessionId} />
        <div ref={bottomRef} />
      </div>

      {!archived && <DraftAttachments draft={draft} />}
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
