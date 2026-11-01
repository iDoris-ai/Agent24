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

// eslint-disable-next-line no-control-regex
// Cc plus Cf (format controls, which include the bidi overrides/isolates that
// make a name read differently from what it is). ZWNJ/ZWJ stay allowed: Persian
// and Indic words need them. Mirrors the daemon's `is_format_control`.
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
