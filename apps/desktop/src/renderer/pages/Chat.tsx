import { useState, useRef, useEffect } from 'react'

interface Message {
  role: 'user' | 'assistant'
  content: string
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

    // Call backend LLM gateway via IPC proxy
    try {
      const res = await window.agent24.backendProxy({
        method: 'POST',
        path: '/api/llm/chat',
        body: {
          messages: [...messages, userMsg].map(m => ({ role: m.role, content: m.content })),
        },
      })
      if (!res.ok) {
        const error = (res.data as { error?: string } | null)?.error ?? `HTTP ${res.status}`
        throw new Error(error)
      }
      const reply = (res.data as { message?: { content?: string } })?.message?.content ?? '（模型未返回内容）'
      setMessages(prev => [...prev, { role: 'assistant', content: reply }])
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
            <div style={{ fontSize: 40 }}>🤖</div>
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
            <div className="message-bubble">{m.content}</div>
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
