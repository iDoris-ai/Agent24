import { useState, useRef, useEffect } from 'react'
import { formatReplySuffix, extractServerReported } from './chat-latency'
import logoWelcome from '../assets/logo-welcome.png'

interface Message {
  role: 'user' | 'assistant'
  content: string
  /** ME4-desktop-model-ui: the "model · tier · 首字/总计 ms" line shown under
   *  an assistant reply. Never set on a user message or the error fallback
   *  message (§ below) — only a real, successful reply gets timed. */
  suffix?: string
}

const SUGGESTIONS = [
  '帮我分析一段文字',
  '用中文总结这篇文章',
  '写一段产品介绍',
  '翻译成英文',
]

export default function ChatPage() {
  const [messages, setMessages] = useState<Message[]>([])
  const [input, setInput] = useState('')
  const [loading, setLoading] = useState(false)
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
        },
      })
      if (!res.ok) {
        // v1 error envelope { error: { code, message } }
        const err = (res.data as { error?: { message?: string } } | null)?.error
        throw new Error(err?.message ?? `HTTP ${res.status}`)
      }
      const totalMs = performance.now() - sentAt
      const reply = (res.data as { message?: { content?: string } })?.message?.content ?? '（模型未返回内容）'
      const server = extractServerReported(res.data)
      // Review M3: ONLY the server's own reported model_id — never the
      // topbar's daemon-default guess. `/api/v1/chat` now (post-M3) always
      // reports the model_id the provider that actually served this call
      // gave, or null when the provider didn't say; either way this suffix
      // reflects what actually happened, never a name that might not be it.
      const suffix = formatReplySuffix({
        modelName: server.modelId,
        tier: server.tier,
        firstTokenMs: server.firstTokenMs,
        totalMs: server.totalMs ?? totalMs,
      })
      setMessages(prev => [...prev, { role: 'assistant', content: reply, suffix }])
    } catch (e) {
      setMessages(prev => [...prev, {
        role: 'assistant',
        content: `⚠️ 无法连接到后端服务。\n请确认 oMLX 或 Ollama 已启动，端口 11434 可用。\n\n错误：${String(e)}`,
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
            <p>你的本地 AI 助理，数据不离机</p>
            <div className="chat-suggestions">
              {SUGGESTIONS.map(s => (
                <button key={s} className="suggestion-chip" onClick={() => void send(s)}>
                  {s}
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
                <div style={{ fontSize: 11, color: 'var(--muted)', marginTop: 4 }}>{m.suffix}</div>
              )}
            </div>
          </div>
        ))}
        {loading && (
          <div className="message assistant">
            <div className="message-avatar">🤖</div>
            <div className="message-bubble" style={{ color: 'var(--muted)' }}>思考中…</div>
          </div>
        )}
        <div ref={bottomRef} />
      </div>

      <div className="chat-input-bar">
        <textarea
          ref={textareaRef}
          className="chat-input"
          placeholder="输入消息… (Enter 发送，Shift+Enter 换行)"
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
