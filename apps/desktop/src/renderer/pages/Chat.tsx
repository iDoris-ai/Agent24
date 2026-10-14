import { useState, useRef, useEffect } from 'react'
import { formatReplySuffix, extractServerReported, type ReplyLatency } from './chat-latency'
import logoWelcome from '../assets/logo-welcome.png'
import { useT } from '../i18n'
import { chatMessages } from './Chat.i18n'

/** M1-T14: what `/api/v1/chat` ACTUALLY did with an explicit "记住…" turn —
 *  read off the response field, never off the model's own reply text (the
 *  local model has been observed saying "我记住了" on a turn the server
 *  skipped because memory was paused). */
type MemoryReceipt = 'saved' | 'paused_not_saved' | 'failed'

interface Message {
  role: 'user' | 'assistant'
  content: string
  /** ME4-desktop-model-ui: the "model · tier · 首字/总计 ms" line shown under
   *  an assistant reply. Never set on a user message or the error fallback
   *  message (§ below) — only a real, successful reply gets timed. */
  suffix?: ReplyLatency
  /** M1-T14: the system-provided receipt line shown under an assistant
   *  reply, distinct from (and below) the latency suffix. Never set on a
   *  user message or the error fallback message. */
  memoryReceipt?: MemoryReceipt
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
  // New conversation (a fresh mount of this page) gets a new id — never
  // regenerated for the lifetime of this component instance.
  const [sessionId] = useState(() => genChatSessionId())
  const bottomRef = useRef<HTMLDivElement>(null)
  const textareaRef = useRef<HTMLTextAreaElement>(null)

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [messages])

  const send = async (text: string) => {
    if (!text.trim() || loading) return
    const userMsg: Message = { role: 'user', content: text.trim() }
    setMessages(prev => [...prev, userMsg])
    setInput('')
    setLoading(true)

    // ME4-desktop-model-ui: total wall-clock time for THIS call, timed on the
    // renderer side with performance.now() (monotonic, sub-ms) — `/api/v1/chat`
    // is a single blocking call today (no streaming), so this IS the reply's
    // whole latency; extractServerReported still gets first crack at it in
    // case a future daemon response reports its own (see chat-latency.ts).
    const sentAt = performance.now()

    // Call backend LLM gateway via IPC proxy
    try {
      const res = await window.agent24.backendProxy({
        method: 'POST',
        path: '/api/v1/chat',
        body: {
          messages: [...messages, userMsg].map(m => ({ role: m.role, content: m.content })),
          session_id: sessionId,
        },
      })
      if (!res.ok) {
        // v1 error envelope { error: { code, message } }
        const err = (res.data as { error?: { message?: string } } | null)?.error
        throw new Error(err?.message ?? `HTTP ${res.status}`)
      }
      const totalMs = performance.now() - sentAt
      const reply = (res.data as { message?: { content?: string } })?.message?.content ?? t('reply.empty')
      const server = extractServerReported(res.data)
      // M1-T14: a deterministic receipt — present only when this prompt was
      // an explicit "记住…" and session_id was set; absent otherwise.
      const memoryReceiptRaw = (res.data as { memory_receipt?: unknown } | null)?.memory_receipt
      const memoryReceipt =
        memoryReceiptRaw === 'saved' || memoryReceiptRaw === 'paused_not_saved' || memoryReceiptRaw === 'failed'
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
      setMessages(prev => [...prev, { role: 'assistant', content: reply, suffix, memoryReceipt }])
    } catch (e) {
      setMessages(prev => [...prev, {
        role: 'assistant',
        content: t('reply.connectionError', { error: String(e) }),
      }])
    } finally {
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
              {m.suffix && (
                <div style={{ fontSize: 11, color: 'var(--muted)', marginTop: 4 }}>{formatReplySuffix(m.suffix)}</div>
              )}
              {/* M1-T14: a deterministic system line, visually distinct from
                  (and never mistaken for) the model's own reply text. */}
              {m.memoryReceipt && (
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
            <div className="message-bubble" style={{ color: 'var(--muted)' }}>{t('status.thinking')}</div>
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
          disabled={!input.trim() || loading}
          onClick={() => void send(input)}
        >
          ↑
        </button>
      </div>
    </div>
  )
}
