// A3-4 — desktop "语音" panel (design docs/design/A3-ATTACHED-MODULE.md §7.3/§10).
//
// Scope (frozen, prototype): display AgentEar's live transcript / turn phase /
// speech (playback) state / tier usage; a `speak` input and a `stop_playback`
// button. Explicitly OUT of scope for this panel (§10/§11 Q1): the model's
// reply text (P3), and any execution of a `proposal` (P3's confirm/execute
// gate) — `builtin` proposals are shown labeled "已在 AgentEar 本地执行" and
// never get an action button; non-builtin proposals are shown labeled
// "P3 前不执行" and likewise never get one.
//
// No persistence anywhere (§7.3/§8, Q4=a): only an in-memory ring buffer of
// the most recent 200 delivered events, held in React state. Nothing here
// touches localStorage/sessionStorage/IndexedDB/files — closing the window or
// restarting Agent24 loses the log, by design.

import { useCallback, useEffect, useRef, useState } from 'react'
import { AgentEarSequencer } from './agentearSequencer'
import { truncateForDisplay } from './display'
import {
  getAgentEarAttachment,
  getAgentEarUsage,
  isBuiltinProposal,
  sendSpeak,
  sendStopPlayback,
  totalCalls,
  type AgentEarEventEnvelope,
  type AttachedView,
  type ByServedUsage,
} from './api'

export const MAX_EVENTS_IN_MEMORY = 200
const ATTACH_POLL_MS = 3000
const USAGE_POLL_MS = 5000
const GAP_TIMEOUT_CHECK_MS = 500

const SPEAK_LANGS: { value: 'zh-CN' | 'en-US' | 'th-TH'; label: string }[] = [
  { value: 'zh-CN', label: '中文' },
  { value: 'en-US', label: 'English' },
  { value: 'th-TH', label: 'ไทย' },
]

const TURN_PHASE_LABELS: Record<string, string> = {
  listening: '聆听中',
  thinking: '思考中',
  speaking: '播报中',
  idle: '空闲',
  failed: '失败',
}

const SPEECH_STATE_LABELS: Record<string, string> = {
  started: '开始播放',
  completed: '播放完成',
  stopped: '已停止',
  failed: '播放失败',
}

function isAttached(attach: AttachedView | null): boolean {
  return attach?.attach_status === 'attached'
}

/** Appends `items` to `prev`, keeping only the most recent MAX_EVENTS_IN_MEMORY
 *  (design §7.3: "最近 200 条"). Exported for the ring-buffer-cap test. */
export function pushCapped(
  prev: AgentEarEventEnvelope[],
  items: AgentEarEventEnvelope[],
  cap: number = MAX_EVENTS_IN_MEMORY,
): AgentEarEventEnvelope[] {
  if (items.length === 0) return prev
  const merged = [...prev, ...items]
  return merged.length > cap ? merged.slice(merged.length - cap) : merged
}

function EventRow({ envelope }: { envelope: AgentEarEventEnvelope }): JSX.Element {
  const { type, payload } = envelope

  if (type === 'transcript') {
    const text = String((payload as { text?: unknown }).text ?? '')
    const lang = String((payload as { lang?: unknown }).lang ?? 'und')
    const { shown, truncated, fullLength } = truncateForDisplay(text)
    return (
      <div className="voice-row voice-row-transcript">
        <span className="voice-row-badge">转写 · {lang}</span>
        <span className="voice-row-text">
          {shown}
          {truncated && (
            <span className="voice-row-truncated" title={`共 ${fullLength} 字，仅显示前 ${shown.length} 字`}>
              {' '}（已截断，共 {fullLength} 字）
            </span>
          )}
        </span>
      </div>
    )
  }

  if (type === 'turn') {
    const phase = String((payload as { phase?: unknown }).phase ?? '')
    return (
      <div className="voice-row voice-row-turn">
        <span className="voice-row-badge">轮次</span>
        <span className="voice-row-text">{TURN_PHASE_LABELS[phase] ?? phase}</span>
      </div>
    )
  }

  if (type === 'speech') {
    const state = String((payload as { state?: unknown }).state ?? '')
    const reason = (payload as { reason?: unknown }).reason
    return (
      <div className="voice-row voice-row-speech">
        <span className="voice-row-badge">播报</span>
        <span className="voice-row-text">
          {SPEECH_STATE_LABELS[state] ?? state}
          {typeof reason === 'string' && reason && ` · ${reason}`}
        </span>
      </div>
    )
  }

  if (type === 'error') {
    const code = String((payload as { code?: unknown }).code ?? '')
    const message = String((payload as { message?: unknown }).message ?? '')
    return (
      <div className="voice-row voice-row-error">
        <span className="voice-row-badge">错误 · {code}</span>
        <span className="voice-row-text">{message}</span>
      </div>
    )
  }

  if (type === 'proposal') {
    const text = String((payload as { text?: unknown }).text ?? '')
    const builtin = isBuiltinProposal(envelope)
    return (
      <div className="voice-row voice-row-proposal">
        <span className="voice-row-badge">指令</span>
        <span className="voice-row-text">{text}</span>
        <span className="voice-row-note">
          {builtin ? '已在 AgentEar 本地执行' : 'P3 前不执行'}
        </span>
      </div>
    )
  }

  if (type === 'confirm_reply') {
    const reply = String((payload as { reply?: unknown }).reply ?? '')
    return (
      <div className="voice-row voice-row-confirm">
        <span className="voice-row-badge">确认</span>
        <span className="voice-row-text">{reply}</span>
      </div>
    )
  }

  return (
    <div className="voice-row">
      <span className="voice-row-badge">{type}</span>
    </div>
  )
}

export default function VoicePanel(): JSX.Element {
  const [attach, setAttach] = useState<AttachedView | null>(null)
  const [attachChecked, setAttachChecked] = useState(false)
  const [attachError, setAttachError] = useState<string | null>(null)
  const [usage, setUsage] = useState<ByServedUsage | null>(null)
  const [events, setEvents] = useState<AgentEarEventEnvelope[]>([])
  const [speakText, setSpeakText] = useState('')
  const [speakLang, setSpeakLang] = useState<'zh-CN' | 'en-US' | 'th-TH'>('zh-CN')
  const [commandError, setCommandError] = useState<string | null>(null)
  const [lastCommandId, setLastCommandId] = useState<string | null>(null)

  const sequencer = useRef(new AgentEarSequencer())

  const appendDelivered = useCallback((delivered: AgentEarEventEnvelope[]) => {
    if (delivered.length === 0) return
    setEvents((prev) => pushCapped(prev, delivered))
  }, [])

  // Live events: main process pushes raw agentear.event/1 objects (already
  // unwrapped from the WS module envelope — see main/agentear-events.ts);
  // this sequencer call is the ONLY place they're de-duped/ordered/validated.
  useEffect(() => {
    const unsubscribe = window.agent24.onAgentEarEvent((raw) => {
      const { delivered } = sequencer.current.ingest(raw)
      appendDelivered(delivered)
    })
    const gapTimer = setInterval(() => {
      appendDelivered(sequencer.current.checkGapTimeouts())
    }, GAP_TIMEOUT_CHECK_MS)
    return () => {
      unsubscribe()
      clearInterval(gapTimer)
    }
  }, [appendDelivered])

  // Attach status poll (GET /api/v1/attached).
  useEffect(() => {
    let cancelled = false
    const poll = () => {
      getAgentEarAttachment()
        .then((view) => {
          if (cancelled) return
          setAttach(view)
          setAttachError(null)
          setAttachChecked(true)
        })
        .catch((e: unknown) => {
          if (cancelled) return
          setAttachError(e instanceof Error ? e.message : String(e))
          setAttachChecked(true)
        })
    }
    poll()
    const timer = setInterval(poll, ATTACH_POLL_MS)
    return () => {
      cancelled = true
      clearInterval(timer)
    }
  }, [])

  // Tier usage poll (GET /api/v1/usage?module=agentear).
  useEffect(() => {
    let cancelled = false
    const poll = () => {
      getAgentEarUsage()
        .then((by) => {
          if (!cancelled) setUsage(by)
        })
        .catch(() => {
          /* usage is a nice-to-have display — never block the panel on it */
        })
    }
    poll()
    const timer = setInterval(poll, USAGE_POLL_MS)
    return () => {
      cancelled = true
      clearInterval(timer)
    }
  }, [])

  const attached = isAttached(attach)

  const onSpeak = async (): Promise<void> => {
    const text = speakText.trim()
    if (!text) return
    try {
      const id = await sendSpeak({ text, lang: speakLang })
      setLastCommandId(id)
      setCommandError(null)
      setSpeakText('')
    } catch (e: unknown) {
      setCommandError(e instanceof Error ? e.message : String(e))
    }
  }

  const onStop = async (): Promise<void> => {
    try {
      const id = await sendStopPlayback()
      setLastCommandId(id)
      setCommandError(null)
    } catch (e: unknown) {
      setCommandError(e instanceof Error ? e.message : String(e))
    }
  }

  const statusLabel = !attachChecked
    ? '检测中…'
    : attach === null
      ? '未附着（尚未配对）'
      : attach.attach_status === 'attached'
        ? '已附着'
        : attach.attach_status === 'disabled'
          ? '已停用'
          : '未附着'

  return (
    <div className="content">
      <div className="page-title">语音（AgentEar）</div>
      <div className="page-sub">
        实时展示 · 不持久化（最近 {MAX_EVENTS_IN_MEMORY} 条，关窗即丢）
      </div>

      <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 12 }}>
        <span
          className={`status-dot ${attached ? 'online' : attach?.attach_status === 'disabled' ? '' : 'offline'}`}
        />
        <span style={{ fontSize: 12 }}>附着状态：{statusLabel}</span>
        {attach?.generation !== undefined && (
          <span style={{ fontSize: 11, color: 'var(--muted)' }}>generation {attach.generation}</span>
        )}
        {usage && (
          <span style={{ fontSize: 11, color: 'var(--muted)', marginLeft: 'auto' }}>
            本地 {totalCalls(usage.local)} 次 · 远端 {totalCalls(usage.remote)} 次
          </span>
        )}
      </div>

      {attachError && (
        <div style={{ color: 'var(--muted)', fontSize: 12, marginBottom: 8 }}>
          {attachError}（AgentEar 可能尚未配对）
        </div>
      )}
      {commandError && (
        <div style={{ color: '#e05050', fontSize: 12, marginBottom: 8 }}>{commandError}</div>
      )}

      <div style={{ display: 'flex', gap: 6, marginBottom: 12, alignItems: 'center' }}>
        <select
          aria-label="语言"
          value={speakLang}
          onChange={(e) => setSpeakLang(e.target.value as typeof speakLang)}
          style={{ fontSize: 12 }}
          disabled={!attached}
        >
          {SPEAK_LANGS.map((l) => (
            <option key={l.value} value={l.value}>{l.label}</option>
          ))}
        </select>
        <input
          aria-label="让它说"
          placeholder="让 AgentEar 说一句…"
          value={speakText}
          onChange={(e) => setSpeakText(e.target.value)}
          disabled={!attached}
          style={{
            flex: 1,
            fontSize: 12,
            padding: '4px 8px',
            background: 'var(--surface2)',
            border: '1px solid var(--border)',
            borderRadius: 6,
            color: 'var(--text)',
          }}
        />
        <button
          className="btn btn-primary"
          style={{ fontSize: 11 }}
          disabled={!attached || speakText.trim() === ''}
          onClick={() => void onSpeak()}
        >
          让它说
        </button>
        <button
          className="btn"
          style={{ fontSize: 11 }}
          disabled={!attached}
          onClick={() => void onStop()}
        >
          停止播放
        </button>
      </div>

      {lastCommandId && (
        <div style={{ fontSize: 10, color: 'var(--muted)', marginBottom: 8 }}>
          最近命令：<code>{lastCommandId}</code>
        </div>
      )}

      <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
        {events.length === 0 && (
          <div style={{ fontSize: 12, color: 'var(--muted)', padding: '8px 0' }}>
            暂无事件
          </div>
        )}
        {events.map((e) => (
          <EventRow key={`${e.session_id}:${e.seq}:${e.event_id}`} envelope={e} />
        ))}
      </div>
    </div>
  )
}
