// Shared, pure latency-display formatting — used by both the chat page's
// per-reply suffix and the voice panel's per-model-call line, so the two
// surfaces agree on the same rule (product ask: "本地模型的两大卖点是隐私和
// 快，要让用户看到有多快"):
//   < 1000 ms  → "xxx ms"
//   >= 1000 ms → "1,234 ms" (thousands-separated, never rounded to seconds)

/** Locale-independent thousands separator — deterministic across CI
 *  environments and test runners, unlike `toLocaleString` (whose default
 *  locale depends on the runtime). */
function withThousands(n: number): string {
  return n.toString().replace(/\B(?=(\d{3})+(?!\d))/g, ',')
}

/** Rounds to the nearest millisecond (never further, e.g. never to seconds)
 *  and renders with a thousands separator once >= 1000. Negative/NaN input
 *  (a clock anomaly, never expected in practice) clamps to 0 rather than
 *  showing a nonsensical negative duration. */
export function formatMs(ms: number): string {
  const n = Number.isFinite(ms) ? Math.max(0, Math.round(ms)) : 0
  return `${withThousands(n)} ms`
}
