// Pure display-formatting helpers for the voice panel, split out from
// VoicePanel.tsx so they're trivial to unit test without React.

/** AgentEar's own 8 KiB wire limit (design §7.1) is far too long for a log
 *  row; this is Agent24's OWN display-side truncation for the panel list —
 *  a separate concern from AgentEar's protocol-level truncation, which
 *  already happened (with its own in-band annotation) before the text ever
 *  reaches here. We never re-truncate in a way that could hide or further
 *  mutate whatever AgentEar already put in `text` — this only decides how
 *  much of the (already-final) string this one list row shows, and always
 *  labels it visibly when it does. */
export const DISPLAY_TRUNCATE_CHARS = 200

export interface DisplayTruncation {
  shown: string
  truncated: boolean
  /** Full character count of the untouched source text. */
  fullLength: number
}

export function truncateForDisplay(
  text: string,
  maxChars: number = DISPLAY_TRUNCATE_CHARS,
): DisplayTruncation {
  const chars = [...text] // count by code point, not UTF-16 unit (design §7.2 note on kb.rs: 按字符截断)
  if (chars.length <= maxChars) {
    return { shown: text, truncated: false, fullLength: chars.length }
  }
  return { shown: chars.slice(0, maxChars).join(''), truncated: true, fullLength: chars.length }
}
