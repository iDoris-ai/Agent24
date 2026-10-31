import { useState, useRef, useEffect } from 'react'
import { formatReplySuffix, extractServerReported, type ReplyLatency } from './chat-latency'
import logoWelcome from '../assets/logo-welcome.png'
import { useT } from '../i18n'
import { chatMessages } from './Chat.i18n'
import type { components } from '@agent24/api-client'
import { askWithKnowledge, createChatSession } from './chat-knowledge'
import { grounding, type Grounding } from './knowledge-grounding'

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
  const bottomRef = useRef<HTMLDivElement>(null)
  const textareaRef = useRef<HTMLTextAreaElement>(null)
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

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [messages])

  const send = async (text: string) => {
    if (!text.trim() || requestInFlight.current || awaitingConfirmation) return
    const userMsg: Message = { role: 'user', content: text.trim(), delivery: { kind: 'sending' } }
    const request: ChatRequest = {
      messages: [...messages.filter(m => m.role === 'assistant' || m.delivery.kind === 'sent'), userMsg]
        .map(m => ({ role: m.role, content: m.content })),
      session_id: sessionId,
    }
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
    setMessages([])
    setInput('')
    setSessionId(genChatSessionId())
  }

  const setDelivery = (index: number, delivery: Delivery) => {
    setMessages(prev => prev.map((m, i) => i === index && m.role === 'user' ? { ...m, delivery } : m))
  }

  const requestReply = async (index: number, request: ChatRequest) => {
    if (requestInFlight.current) return
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
    try {
      const proxy = window.agent24.backendProxy
      const ask = async () => {
        runSession.current ??= await createChatSession(proxy)
        return runSession.current === null
          ? { kind: 'failed' as const, code: 'session_unavailable', message: 'session_unavailable' }
          : await askWithKnowledge(prompt, runSession.current, { proxy, onAwaitingApproval: setAwaitingApproval }, request.idoris_headroom_confirmed === true)
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
        setMessages(prev => [...prev, { role: 'assistant', content: turn.text || t('reply.empty'), suffix, knowledge }])
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
      <div className="chat-messages">
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
          <div key={i} className={`message ${m.role}`}>
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
        <div ref={bottomRef} />
      </div>

      <div className="chat-input-bar">
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
      </div>
    </div>
  )
}
