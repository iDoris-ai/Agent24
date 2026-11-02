// Strict input checks for the dedicated comm IPC handlers (contact add, send,
// key backup). The renderer is not trusted: these run in main before anything
// is sent to the daemon.

const CHARSET = 'qpzry9x8gf2tvdw0s3jn54khce6mua7l'
const GEN = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3]

function polymod(values: number[]): number {
  let chk = 1
  for (const v of values) {
    const top = chk >>> 25
    chk = ((chk & 0x1ffffff) << 5) ^ v
    for (let i = 0; i < 5; i += 1) if ((top >>> i) & 1) chk ^= GEN[i] as number
  }
  return chk >>> 0
}

/** A well-formed `npub1…`: bech32 checksum valid, 32 bytes of key data. */
export function isValidNpub(value: unknown): value is string {
  if (typeof value !== 'string' || value.length !== 63 || value !== value.toLowerCase()) return false
  if (!value.startsWith('npub1')) return false
  const data: number[] = []
  for (const ch of value.slice(5)) {
    const idx = CHARSET.indexOf(ch)
    if (idx < 0) return false
    data.push(idx)
  }
  const hrpExpanded = [...'npub'].map((c) => c.charCodeAt(0) >> 5).concat([0], [...'npub'].map((c) => c.charCodeAt(0) & 31))
  if (polymod([...hrpExpanded, ...data]) !== 1) return false
  // 52 data groups of 5 bits = 260 bits = 32 bytes + 4 padding bits that must be zero.
  const groups = data.slice(0, -6)
  if (groups.length !== 52) return false
  return ((groups[51] as number) & 0x0f) === 0
}

// Cc plus Cf (format controls, which include the bidi overrides/isolates that
// make a name read differently from what it is). ZWNJ/ZWJ stay allowed: Persian
// and Indic words need them. Mirrors the daemon's `is_format_control`.
// eslint-disable-next-line no-control-regex -- rejecting control characters is the point
const CONTROL = /[\u0000-\u001f\u007f-\u009f\u2028\u2029]|(?![\u200c\u200d])\p{Cf}/u

/** A display name: 1..max chars, no control characters (a newline would let a
 *  name forge fields in Hyphae's line-oriented output). */
export function isSafeName(value: unknown, max = 64): value is string {
  return typeof value === 'string' && value.trim().length > 0 && [...value].length <= max &&
    Buffer.byteLength(value, 'utf8') <= NICKNAME_MAX_BYTES && !CONTROL.test(value)
}

/** The daemon's nickname contract: at most 128 characters (callers may pass a
 *  smaller limit) and 512 UTF-8 bytes. */
export const NICKNAME_MAX_BYTES = 512
export const MAX_MESSAGE_BYTES = 16 * 1024
export const BACKUP_PASSWORD_MIN = 8
export const BACKUP_PASSWORD_MAX = 1024

export function isSafeMessage(value: unknown): value is string {
  return typeof value === 'string' && value.trim().length > 0 && !value.includes('\0') &&
    Buffer.byteLength(value, 'utf8') <= MAX_MESSAGE_BYTES
}

/** The one backup-password rule (mirrors the daemon and OpenAPI): at least 8
 *  characters, at most 1024 UTF-8 bytes, a single line (no CR, LF or NUL: Hyphae
 *  reads one value per stdin line) and not whitespace only. */
export function isSafeBackupPassword(value: unknown): value is string {
  return typeof value === 'string' && [...value].length >= BACKUP_PASSWORD_MIN &&
    Buffer.byteLength(value, 'utf8') <= BACKUP_PASSWORD_MAX && !/[\r\n\0]/.test(value) && value.trim().length > 0
}

/** `<nickname>` as a file name: letters and digits of any script kept, anything
 *  else becomes `_`, no leading dots, never empty. */
export function backupFileStem(nickname: string): string {
  const stem = nickname.replace(/[^\p{L}\p{M}\p{N}]+/gu, '_').replace(/^[._]+|[._]+$/g, '')
  return stem || 'agent24-identity'
}

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
 * host, no userinfo, no query, no fragment. `URL` normalizes (lower-case host,
 * punycode, default port dropped); a bare "/" path is dropped again so a plain
 * `wss://host` stays `wss://host`.
 */
export function normalizeRelayUrl(entry: unknown): string | null {
  if (typeof entry !== 'string' || entry.length > MAX_RELAY_URL_LENGTH || !PRINTABLE_ASCII.test(entry)) return null
  if (entry.includes('?') || entry.includes('#') || entry.includes('@')) return null
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
