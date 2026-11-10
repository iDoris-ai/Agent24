/** Relay list rules shared by the main-process IPC gate and the page that edits the list. */

export const MAX_RELAYS = 8
/** Longest relay URL, raw and normalized. The confirmation dialog shows every
 *  URL in full, so the whole list always fits in it: MAX_RELAYS lines of at most
 *  this many characters. Mirrors the daemon's `MAX_RELAY_URL_BYTES`. */
export const MAX_RELAY_URL_LENGTH = 256
/** What the relay confirmation can show without truncating anything. */
export const RELAY_DIALOG_MAX_CHARS = MAX_RELAYS * (MAX_RELAY_URL_LENGTH + 1)

const PRINTABLE_ASCII = /^[\x21-\x7e]+$/

function isLoopbackHost(hostname: string): boolean {
  if (hostname === 'localhost' || hostname === '[::1]') return true
  const v4 = /^(\d{1,3})\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.exec(hostname)
  return v4 !== null && Number(v4[1]) === 127
}

/**
 * One relay URL in the form that is both shown and sent, or null if unusable:
 * printable ASCII only in the raw text (so no homoglyph, bidi or zero-width
 * character can be hidden in it), `wss://` (or `ws://` for loopback only), a
 * host, no userinfo (an `@` in the PATH is fine), no query, no fragment. `URL` normalizes (lower-case host,
 * punycode, default port dropped); a bare "/" path is dropped again so a plain
 * `wss://host` stays `wss://host`.
 */
export function normalizeRelayUrl(entry: unknown): string | null {
  if (typeof entry !== 'string' || entry.length > MAX_RELAY_URL_LENGTH || !PRINTABLE_ASCII.test(entry)) return null
  if (entry.includes('?') || entry.includes('#')) return null
  let url: URL
  try { url = new URL(entry) } catch { return null }
  if (!/^wss?:\/\//i.test(entry)) return null
  if (!url.hostname || url.username || url.password || url.search || url.hash) return null
  if (url.protocol === 'ws:' && !isLoopbackHost(url.hostname)) return null
  if (url.protocol !== 'ws:' && url.protocol !== 'wss:') return null
  const href = url.pathname === '/' ? url.href.slice(0, -1) : url.href
  return href.length <= MAX_RELAY_URL_LENGTH ? href : null
}

/**
 * The relay list the page asked for in normalized form, or null if any part is
 * unusable: 1..8 distinct valid relays (see {@link normalizeRelayUrl}).
 */
export function parseRelayList(value: unknown): string[] | null {
  if (!Array.isArray(value) || value.length < 1 || value.length > MAX_RELAYS) return null
  const seen = new Set<string>()
  const out: string[] = []
  for (const entry of value) {
    const normalized = normalizeRelayUrl(entry)
    if (normalized === null || seen.has(normalized)) return null
    seen.add(normalized)
    out.push(normalized)
  }
  // Defensive: the dialog lists one line per relay and must never truncate.
  if (out.join('\n').length > RELAY_DIALOG_MAX_CHARS) return null
  return out
}
