// Chat page's per-reply latency suffix ("Qwen3.6-35B-A3B · 本地 · 首字 412 ms
// · 总计 1,834 ms") — split out of Chat.tsx so it's trivial to unit test
// without React, same reasoning as voice/display.ts.
//
// Today's `/api/v1/chat` (agent24-protocol's `ChatResponse`) is a single
// blocking call with no streaming and no `model_id`/`tier`/latency fields of
// its own (rust/crates/agent24-protocol/src/types.rs) — so in practice
// `extractServerReported` always comes back empty and every reply falls back
// to "frontend-timed total only, topbar's daemon default model name". Both
// functions still accept the richer shape so a future streaming/instrumented
// `/api/v1/chat` is picked up with no call-site change.

import { formatMs } from '../../shared/format-latency'

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

const TIER_LABELS: Record<'local' | 'remote', string> = { local: '本地', remote: '远端' }

/** The one line shown under an assistant reply. Never throws on a `null`
 *  modelName/tier — it just omits that segment. */
export function formatReplySuffix(latency: ReplyLatency): string {
  const parts: string[] = []
  if (latency.modelName) parts.push(latency.modelName)
  if (latency.tier) parts.push(TIER_LABELS[latency.tier])
  if (latency.firstTokenMs !== null && latency.firstTokenMs !== latency.totalMs) {
    parts.push(`首字 ${formatMs(latency.firstTokenMs)}`)
  }
  parts.push(`总计 ${formatMs(latency.totalMs)}`)
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
