// Chat page's per-reply latency suffix ("Qwen3.6-35B-A3B · 本地 · 首字 0.4 秒
// · 用时 1.8 秒") — split out of Chat.tsx so it's trivial to unit test
// without React, same reasoning as voice/display.ts.
//
// Review M3: `/api/v1/chat` (agent24-protocol's `ChatResponse`) now reports
// the server-measured `model_id`/`tier`/`latency_ms` for the call that
// actually served it (rust/apps/agent24d/src/routes.rs's `post_chat`) — this
// is the ONLY source `formatReplySuffix`'s `modelName` may come from. When
// the provider didn't report a `model_id`, `extractServerReported` returns
// `null` and the suffix shows no model name at all — Chat.tsx must NOT
// substitute a guess (e.g. the topbar's daemon-default model) in its place;
// that would show a model that may not be the one that actually answered.
// `firstTokenMs` stays `null` today (no streaming yet), so only `用时` shows.

import { translate } from '../i18n'
import { chatMessages } from './Chat.i18n'

export interface ReplyLatency {
  /** `null` when neither the server nor the topbar has a model name to show. */
  modelName: string | null
  tier: 'local' | 'remote' | null
  /** `null`, or equal to `totalMs`, when there is no separate first-token
   *  measurement (today: always, since `/api/v1/chat` doesn't stream) — either
   *  way, only `总计` is shown, never a redundant identical `首字`. */
  firstTokenMs: number | null
  totalMs: number
}

/** UI polish 02 (David): a time as people say it, never 「132,315 ms」:
 *  under 10 s to a tenth, then whole seconds, from a minute on 「2 分 12 秒」. */
export function formatDuration(ms: number): string {
  const n = Number.isFinite(ms) ? Math.max(0, ms) : 0
  if (n < 9_950) return translate(chatMessages, 'latency.seconds', { s: (n / 1000).toFixed(1) })
  const s = Math.round(n / 1000)
  return s < 60
    ? translate(chatMessages, 'latency.seconds', { s: String(s) })
    : translate(chatMessages, 'latency.minutes', { m: String(Math.floor(s / 60)), s: String(s % 60) })
}

/** The one line shown under an assistant reply. Never throws on a `null`
 *  modelName/tier — it just omits that segment. */
export function formatReplySuffix(latency: ReplyLatency): string {
  const parts: string[] = []
  if (latency.modelName) parts.push(latency.modelName)
  if (latency.tier) parts.push(translate(chatMessages, `latency.${latency.tier}`))
  if (latency.firstTokenMs !== null && latency.firstTokenMs !== latency.totalMs) {
    parts.push(translate(chatMessages, 'latency.firstToken', { time: formatDuration(latency.firstTokenMs) }))
  }
  parts.push(translate(chatMessages, 'latency.total', { time: formatDuration(latency.totalMs) }))
  return parts.join(' · ')
}

export interface ServerReportedLatency {
  modelId: string | null
  tier: 'local' | 'remote' | null
  totalMs: number | null
  firstTokenMs: number | null
}

/** Reads whatever `/api/v1/chat`'s response body happens to carry, tolerating
 *  its current (fieldless) shape entirely — every field is optional and a
 *  wrong type is treated as absent, never thrown. */
export function extractServerReported(data: unknown): ServerReportedLatency {
  const d = (data ?? null) as Record<string, unknown> | null
  const modelId = typeof d?.['model_id'] === 'string' ? (d['model_id'] as string) : null
  const tier = d?.['tier'] === 'local' || d?.['tier'] === 'remote' ? (d['tier'] as 'local' | 'remote') : null
  const totalMs = typeof d?.['latency_ms'] === 'number' ? (d['latency_ms'] as number) : null
  const firstTokenMs = typeof d?.['first_token_ms'] === 'number' ? (d['first_token_ms'] as number) : null
  return { modelId, tier, totalMs, firstTokenMs }
}
