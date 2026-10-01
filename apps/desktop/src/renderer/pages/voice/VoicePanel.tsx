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
// No persistence anywhere (§7.3/§8, Q4=a): the log this panel shows lives
// only in the main process's memory (main/agentear-log.ts, review M5) —
// nothing here or there touches localStorage/sessionStorage/IndexedDB/files.
// Closing Agent24 (not just this panel) loses it, by design. Review M5 also
// moved the sequencer + the 200-event cap OUT of this component and into
// main: switching to another sidebar page and back now keeps the list,
// because it was never this component's state to lose — this component
// just pulls a snapshot on mount and mirrors new arrivals from then on.

import { Component, useEffect, useState, type ErrorInfo, type ReactNode } from 'react'
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
import type { ModelCallEnvelope } from '../../../shared/ipc-types'
import { formatMs } from '../../../shared/format-latency'
import { MAX_EVENTS_IN_MEMORY, pushCapped } from '../../../shared/agentearSequencer'

/** Mirrors main/model-call-log.ts's own cap — the main process never sends
 *  more than this many anyway, but a defensive local cap keeps this
 *  component's own state bounded too if that ever changes. */
const MAX_MODEL_CALLS_SHOWN = 5

export { MAX_EVENTS_IN_MEMORY, pushCapped }

const ATTACH_POLL_MS = 3000
const USAGE_POLL_MS = 5000
/** AgentEar command schema's `speak.text` hard limit (contracts/schema/
 *  agentear.command.v1.schema.json: maxLength 2000) — enforced client-side
 *  too so a paste-in doesn't silently get rejected server-side with no
 *  visible reason. */
const SPEAK_TEXT_MAX_LENGTH = 2000

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

// ── AgentEar event payload guards ───────────────────────────────────────────
// `AgentEarEventEnvelope.payload` is `Record<string, unknown>` — it comes
// straight off the wire from AgentEar (a separate, untrusted process; the
// kernel relays it verbatim, see ipc-types.ts) and is never schema-validated
// before it reaches this panel. Two failure modes guarded here (Codex
// follow-up review, ME4/A3-4):
//
// 1. A malicious/malformed field value that isn't actually a string (e.g.
//    `phase: {}` or `text: 123`) must never be blindly coerced with
//    `String(x)` — coercion can produce misleading output, and more
//    generally any non-string must fail closed to a safe fallback rather
//    than being displayed as if it were real data.
// 2. A string value that collides with a JS Object.prototype member name —
//    `"__proto__"`, `"constructor"`, `"toString"`, ... — used as a lookup key
//    into a plain object label table (`TABLE[key]`) resolves to the
//    INHERITED prototype member, not `undefined`. For `"__proto__"` that's
//    `Object.prototype` itself: an object, which React then throws
//    rendering as a child ("Objects are not valid as a React child"),
//    unmounting this panel's whole subtree (and, absent an error boundary,
//    the rest of the desktop UI with it). `Object.hasOwn` restricts a hit to
//    keys this file itself put in the table.
function isPlainRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}

/** Only ever returns an actual string from the field, never a coerced one. */
function safeString(v: unknown): string | null {
  return typeof v === 'string' ? v : null
}

/** Label-table lookup for an untrusted enum-like field. `raw` must be an own
 *  string key of `table` to resolve to its label; anything else (wrong type,
 *  or a string that only resolves via the prototype chain, like
 *  `"__proto__"`) falls back to `unknownLabel` — never to the raw payload
 *  text (source of truth for the fallback is the caller-supplied label, not
 *  attacker-controlled input), and the lookup itself never returns anything
 *  but a string this file defined. */
function labelFor(table: Record<string, string>, raw: unknown, unknownLabel: string): string {
  const key = safeString(raw)
  if (key !== null && Object.hasOwn(table, key)) return table[key]!
  return unknownLabel
}

const TIER_LABELS: Record<string, string> = { local: '本地', remote: '远端' }

/** ME4-desktop-model-ui: the one line shown for EACH of AgentEar's recent
 * `_a24/model/complete` calls — which model, local/remote, how long it took
 * (shared `formatMs` rule: "1,234 ms" once >= 1000, never rounded to
 * seconds), and a failure marker when the call itself didn't succeed.
 * Exported for its unit test. */
export function formatModelCall(call: ModelCallEnvelope): string {
  const model = call.model_id ?? '未知模型'
  const tier = call.tier ? (TIER_LABELS[call.tier] ?? call.tier) : '未知位置'
  const status = call.ok ? '' : ' · 失败'
  return `${model} · ${tier} · ${formatMs(call.latency_ms)}${status}`
}

function EventRow({ envelope }: { envelope: AgentEarEventEnvelope }): JSX.Element {
  const { type } = envelope
  // Guard against `payload` itself not even being an object (null, an
  // array, a primitive) — a malformed envelope from off the wire, not just
  // a malformed field inside an otherwise-normal payload object. Reading a
  // property off `null`/`undefined` throws before any of the per-field
  // guards below would ever run.
  const payload = isPlainRecord(envelope.payload) ? envelope.payload : {}

  if (type === 'transcript') {
    const text = safeString(payload.text) ?? ''
    const lang = safeString(payload.lang) ?? 'und'
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
    const phaseLabel = labelFor(TURN_PHASE_LABELS, payload.phase, '未知阶段')
    // AgentEar PR #102 / agent-speaker v0.26.0: an idle/failed turn may carry
    // a `timings` object — `to_first_audio_ms` ("说完到听到") is the one
    // number worth surfacing right here, next to the turn itself; the fuller
    // per-step breakdown lands in the timing ledger (GET /api/v1/timings),
    // not this row.
    const timings = payload.timings
    const toFirstAudioMs = isPlainRecord(timings) ? timings.to_first_audio_ms : undefined
    return (
      <div className="voice-row voice-row-turn">
        <span className="voice-row-badge">轮次</span>
        <span className="voice-row-text">
          {phaseLabel}
          {typeof toFirstAudioMs === 'number' && ` · 说完到听到 ${formatMs(toFirstAudioMs)}`}
        </span>
      </div>
    )
  }

  if (type === 'speech') {
    const stateLabel = labelFor(SPEECH_STATE_LABELS, payload.state, '未知状态')
    const reason = safeString(payload.reason)
    return (
      <div className="voice-row voice-row-speech">
        <span className="voice-row-badge">播报</span>
        <span className="voice-row-text">
          {stateLabel}
          {reason && ` · ${reason}`}
        </span>
      </div>
    )
  }

  if (type === 'error') {
    const code = safeString(payload.code) ?? '未知错误码'
    const message = safeString(payload.message) ?? ''
    return (
      <div className="voice-row voice-row-error">
        <span className="voice-row-badge">错误 · {code}</span>
        <span className="voice-row-text">{message}</span>
      </div>
    )
  }

  if (type === 'proposal') {
    const text = safeString(payload.text) ?? ''
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
    const reply = safeString(payload.reply) ?? ''
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

// ── Panel-level error boundary ──────────────────────────────────────────────
// Codex follow-up review: AgentEar payloads are untrusted (see the guards
// above), and until this fix a bad payload could throw during render with no
// boundary anywhere above this panel — React then unmounts the WHOLE
// desktop UI, not just 语音. This boundary is local to VoicePanel on
// purpose (§ task scope): a crash inside this panel's subtree now degrades
// to a fallback message inside 语音's own page, leaving every other sidebar
// page untouched. There's no repo-wide ErrorBoundary component to reuse yet
// (checked: no `componentDidCatch`/`getDerivedStateFromError` elsewhere in
// apps/desktop/src), so it's implemented right here, scoped to this panel.
interface VoicePanelBoundaryState {
  error: Error | null
}

class VoicePanelErrorBoundary extends Component<{ children: ReactNode }, VoicePanelBoundaryState> {
  constructor(props: { children: ReactNode }) {
    super(props)
    this.state = { error: null }
  }

  static getDerivedStateFromError(error: Error): VoicePanelBoundaryState {
    return { error }
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    // eslint-disable-next-line no-console -- best-effort diagnostics only; no telemetry pipeline here
    console.error('[VoicePanel] render error, panel degraded:', error, info.componentStack)
  }

  render(): ReactNode {
    if (this.state.error) {
      return (
        <div className="content">
          <div className="page-title">语音（AgentEar）</div>
          <div style={{ fontSize: 12, color: '#e05050', padding: '8px 0' }}>
            语音面板遇到异常数据，已停止渲染以避免影响其他页面。请重新打开本页或稍后重试。
          </div>
        </div>
      )
    }
    return this.props.children
  }
}

export default function VoicePanel(): JSX.Element {
  return (
    <VoicePanelErrorBoundary>
      <VoicePanelInner />
    </VoicePanelErrorBoundary>
  )
}

function VoicePanelInner(): JSX.Element {
  const [attach, setAttach] = useState<AttachedView | null>(null)
  const [attachChecked, setAttachChecked] = useState(false)
  const [attachError, setAttachError] = useState<string | null>(null)
  const [usage, setUsage] = useState<ByServedUsage | null>(null)
  const [events, setEvents] = useState<AgentEarEventEnvelope[]>([])
  const [modelCalls, setModelCalls] = useState<ModelCallEnvelope[]>([])
  const [speakText, setSpeakText] = useState('')
  const [speakLang, setSpeakLang] = useState<'zh-CN' | 'en-US' | 'th-TH'>('zh-CN')
  const [commandError, setCommandError] = useState<string | null>(null)
  const [lastCommandId, setLastCommandId] = useState<string | null>(null)

  // Review M5: sequencing/de-dup/the 200-cap all happen in the main process
  // now (main/agentear-log.ts) — this component just pulls whatever the log
  // already holds once on mount (so switching pages and back doesn't lose
  // it), then mirrors each NEW envelope as it's pushed, applying the same
  // cap locally too (defense in depth for a very long-lived mount).
  useEffect(() => {
    let cancelled = false
    window.agent24.agentearSnapshot()
      .then((initial) => {
        if (!cancelled) setEvents(initial as AgentEarEventEnvelope[])
      })
      .catch(() => {
        /* snapshot is best-effort — live events still arrive via the subscription below */
      })
    const unsubscribe = window.agent24.onAgentEarEvent((envelope) => {
      setEvents((prev) => pushCapped(prev, [envelope as AgentEarEventEnvelope]))
    })
    return () => {
      cancelled = true
      unsubscribe()
    }
  }, [])

  // ME4-desktop-model-ui: same pull(snapshot)+push(ongoing) shape as the
  // AgentEar event log above, for main/model-call-log.ts's ring buffer — each
  // recent call is shown (not just the latest), oldest first, capped the
  // same way the main-process log already is.
  useEffect(() => {
    let cancelled = false
    window.agent24.modelCallSnapshot()
      .then((calls) => {
        if (!cancelled) setModelCalls(calls.slice(-MAX_MODEL_CALLS_SHOWN))
      })
      .catch(() => {
        /* best-effort, same as the AgentEar snapshot above */
      })
    const unsubscribe = window.agent24.onModelCallEvent((call) => {
      setModelCalls((prev) => [...prev, call].slice(-MAX_MODEL_CALLS_SHOWN))
    })
    return () => {
      cancelled = true
      unsubscribe()
    }
  }, [])

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
        实时展示 · 最近 {MAX_EVENTS_IN_MEMORY} 条 · 关闭应用即丢，不落盘
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

      {modelCalls.length > 0 && (
        <div style={{ marginBottom: 12 }}>
          <div style={{ fontSize: 11, color: 'var(--muted)', marginBottom: 2 }}>最近模型调用</div>
          {[...modelCalls].reverse().map((c, i) => (
            <div key={i} style={{ fontSize: 11, color: 'var(--muted)' }}>
              {formatModelCall(c)}
            </div>
          ))}
        </div>
      )}

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
          maxLength={SPEAK_TEXT_MAX_LENGTH}
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
