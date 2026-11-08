// Deterministic redaction for a local knowledge-entry draft. Pure functions:
// no files, no logs.
//
// How a scan works:
// - Every finder makes its own pass over the whole text and reports every
//   match it sees. No finder can hide text from another: an earlier hit never
//   moves a later rule past a secret.
// - The hits are then joined. Overlapping hits become one span, whose category
//   is that of the earliest rule in the table among them. Taking a little too
//   much is acceptable; leaving a secret is not.
// - A hit made of placeholders alone is dropped, so redacted text scans clean.
//   A hit that only touches a placeholder is kept.
// - A finder whose work at a position is bounded tries every position, inside
//   its own earlier hits too, so a match that starts inside one hit and runs
//   past it is still found. A finder that skips ahead skips only what cannot
//   start such a match.
// - Linear time on any input up to MAX_REDACT_INPUT: a pass only moves
//   forward, regexes are sticky and run over one candidate each, and a
//   position is read a bounded number of times in each pass.
//
// Known limit (Codex round 2, M3 不做): some rules look at the character just
// before a match (a phone is not preceded by a digit, localhost not by a
// letter or digit). Replacing a hit with a placeholder changes that character,
// so text right after a hit can match only in the next pass. preRedact scans
// again up to MAX_PASSES; a longer chain of such hits, each glued to the next
// (`localhost:1localhost:1…`), stays flagged by postScan: held back, never
// let through.
//
// postScan is the rule table plus wider shapes (PR-Daemon B6): 15 or more
// digits anywhere, home directories and `file:` URLs inside URLs too. So
// redacted text may still be held back by postScan; the user edits it.
//
// Known limit (PR-Daemon N5, M3 不做): a placeholder is longer than some hits,
// so redacting text near MAX_REDACT_INPUT can grow it past the limit, and
// postScan then holds the whole text back. A false alarm, not a leak.

export const REDACTION_RULES_VERSION = 1

export type RedactionCategory =
  | 'private_key'
  | 'credential'
  | 'email'
  | 'phone'
  | 'id_card'
  | 'bank_card'
  | 'path'
  | 'internal_address'

/** UTF-16 offsets, half-open `[start, end)`. */
export interface RedactionHit {
  category: RedactionCategory
  start: number
  end: number
}

/** Characters, counted as UTF-16 code units. */
export const MAX_REDACT_INPUT = 512 * 1024

/** preRedact scans its own output again until it is clean, at most this often (see the known limit above). */
const MAX_PASSES = 4

const LABELS: Record<RedactionCategory, string> = {
  private_key: '私钥',
  credential: '凭据',
  email: '邮箱',
  phone: '电话',
  id_card: '证件号',
  bank_card: '银行卡号',
  path: '路径',
  internal_address: '内网地址',
}

const PLACEHOLDER_MARK = '〔已脱敏·'

export function placeholder(category: RedactionCategory): string {
  return `${PLACEHOLDER_MARK}${LABELS[category]}〕`
}

interface Span {
  start: number
  end: number
}

/** One scan's text and its ASCII-lowered copy, which has the same offsets. */
interface Source {
  readonly text: string
  readonly lower: string
}

/** One pass over the whole text, reporting every match in order. */
type Finder = (src: Source, emit: (span: Span) => void) => void

export function preRedact(text: string): { text: string; counts: Partial<Record<RedactionCategory, number>> } {
  if (text.length > MAX_REDACT_INPUT) throw new RangeError('input exceeds MAX_REDACT_INPUT')
  const counts: Partial<Record<RedactionCategory, number>> = {}
  let current = text
  // A placeholder can join what is next to it into a new match; scan again.
  for (let pass = 0; pass < MAX_PASSES && current.length <= MAX_REDACT_INPUT; pass += 1) {
    const hits = scan(current)
    if (hits.length === 0) break
    let out = ''
    let cursor = 0
    for (const hit of hits) {
      out += current.slice(cursor, hit.start)
      out += placeholder(hit.category)
      counts[hit.category] = (counts[hit.category] ?? 0) + 1
      cursor = hit.end
    }
    current = out + current.slice(cursor)
  }
  return { text: current, counts }
}

/**
 * The check before anything leaves the machine: the rule table, plus wider
 * shapes that preRedact does not replace (PR-Daemon B6), so it is not just a
 * second run of the same finders. Any hit holds the text back; the user edits
 * it. Wider means more false alarms, never fewer hits than preRedact's table.
 */
export function postScan(text: string): RedactionHit[] {
  if (text.length > MAX_REDACT_INPUT) return [{ category: 'credential', start: 0, end: text.length }]
  return scan(text, postScanRules())
}

function scan(text: string, rules: readonly Rule[] = redactionRules()): RedactionHit[] {
  const src: Source = { text, lower: asciiLower(text) }
  const marks = placeholderMarks(text)
  const raw: { rank: number; start: number; end: number }[] = []
  for (let rank = 0; rank < rules.length; rank += 1) {
    for (const find of rules[rank].finders) {
      find(src, (span) => {
        if (span.end > span.start && !marks.covers(span)) raw.push({ rank, start: span.start, end: span.end })
      })
    }
  }
  raw.sort((a, b) => a.start - b.start || a.rank - b.rank)
  const hits: RedactionHit[] = []
  let open: { rank: number; start: number; end: number } | null = null
  for (const hit of raw) {
    if (open && hit.start < open.end) {
      open.end = Math.max(open.end, hit.end)
      open.rank = Math.min(open.rank, hit.rank)
      continue
    }
    if (open) hits.push({ category: rules[open.rank].category, start: open.start, end: open.end })
    open = { ...hit }
  }
  if (open) hits.push({ category: rules[open.rank].category, start: open.start, end: open.end })
  return hits
}

/** Quotes and closing punctuation that a redacted value may keep around its placeholder. */
const NEUTRAL = new Set([',', ';', '.', ':', ')', ']', '}', '"', "'", '`', '，', '。', '；', '）'])

/**
 * The complete placeholders in `text`, to tell a hit that holds nothing but
 * placeholders and the quotes or punctuation around them: redacted already.
 */
function placeholderMarks(text: string): { covers(span: Span): boolean } {
  const spans: Span[] = []
  for (const label of Object.values(LABELS)) {
    const token = `${PLACEHOLDER_MARK}${label}〕`
    for (let at = text.indexOf(token); at >= 0; at = text.indexOf(token, at + token.length)) {
      spans.push({ start: at, end: at + token.length })
    }
  }
  // Placeholders never overlap: each one opens with 〔 and holds no other.
  spans.sort((a, b) => a.start - b.start)
  return {
    covers: (span) => {
      let lo = 0
      let hi = spans.length
      while (lo < hi) {
        const mid = (lo + hi) >> 1
        if (spans[mid].end <= span.start) lo = mid + 1
        else hi = mid
      }
      let any = false
      for (let p = span.start; p < span.end;) {
        const mark = spans[lo]
        if (mark !== undefined && mark.start <= p) {
          any = true
          p = mark.end
          lo += 1
        } else if (NEUTRAL.has(text[p])) {
          p += 1
        } else {
          return false
        }
      }
      return any
    },
  }
}

/** A-Z lowered, nothing else, so every offset stays the same. */
function asciiLower(text: string): string {
  return text.replace(/[A-Z]+/g, (run) => run.toLowerCase())
}

function isDigit(c: string | undefined): boolean {
  return c !== undefined && c >= '0' && c <= '9'
}

function isAsciiLetter(c: string | undefined): boolean {
  return c !== undefined && ((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z'))
}

function isAsciiAlnum(c: string | undefined): boolean {
  return isDigit(c) || isAsciiLetter(c)
}

/** What `\s` matches in a JavaScript regex, one UTF-16 unit at a time. */
function isSpace(c: string | undefined): boolean {
  if (c === undefined || c.length === 0) return false
  const k = c.charCodeAt(0)
  return (
    k === 0x20 || (k >= 0x09 && k <= 0x0d) || k === 0xa0 || k === 0x1680 || (k >= 0x2000 && k <= 0x200a) ||
    k === 0x2028 || k === 0x2029 || k === 0x202f || k === 0x205f || k === 0x3000 || k === 0xfeff
  )
}

/**
 * The first index at or after `from` of `needle` where `ok` holds, or -1, for
 * one pass whose `from` never goes back: an answer still ahead is reused.
 */
function forwardIndex(hay: string, needle: string, ok: (at: number) => boolean = () => true): (from: number) => number {
  let askedFrom = Number.POSITIVE_INFINITY
  let answer = -1
  return (from) => {
    if (askedFrom <= from && (answer < 0 || answer >= from)) return answer
    let at = hay.indexOf(needle, from)
    while (at >= 0 && !ok(at)) at = hay.indexOf(needle, at + 1)
    askedFrom = from
    answer = at
    return at
  }
}

// ---------------------------------------------------------------- private_key

const BEGIN_RE = /-----BEGIN ([A-Z ]{0,40})PRIVATE KEY-----/y
const END_RE = /-----END ([A-Z ]{0,40})PRIVATE KEY-----/y

function isBase64Char(c: string): boolean {
  return isAsciiAlnum(c) || c === '+' || c === '/' || c === '='
}


/**
 * Every BEGIN … PRIVATE KEY block. It runs through the next END line of the
 * same key type, whatever lies between. With none after it, it runs through
 * the last line that reads as key material (see pemEnd).
 */
const privateKeys: Finder = ({ text }, emit) => {
  // Every END line, by key type, in order: read once, so no BEGIN searches.
  // BEGINs come in order too, so each type's cursor only moves forward.
  const ends = new Map<string, { at: number[]; next: number }>()
  for (let at = text.indexOf('-----END'); at >= 0; at = text.indexOf('-----END', at + 1)) {
    END_RE.lastIndex = at
    const m = END_RE.exec(text)
    if (!m) continue
    const list = ends.get(m[1]) ?? { at: [], next: 0 }
    list.at.push(at + m[0].length)
    ends.set(m[1], list)
  }
  /** The end of the first END of `type` that ends after `from`, or -1. */
  const endAfter = (type: string, from: number): number => {
    const list = ends.get(type)
    if (!list) return -1
    while (list.next < list.at.length && list.at[list.next] <= from) list.next += 1
    return list.next < list.at.length ? list.at[list.next] : -1
  }
  let i = text.indexOf('-----BEGIN')
  while (i >= 0) {
    BEGIN_RE.lastIndex = i
    const head = BEGIN_RE.exec(text)
    if (!head) {
      i = text.indexOf('-----BEGIN', i + 1)
      continue
    }
    const bodyStart = i + head[0].length
    // The first END of this type that starts at or after the body.
    const closed = endAfter(head[1], bodyStart + `-----END ${head[1]}PRIVATE KEY-----`.length - 1)
    const end = closed >= 0 ? closed : pemEnd(text, bodyStart)
    emit({ start: i, end })
    // A block of another type may begin inside this one and end past it. What
    // pemEnd takes never holds a BEGIN line, so only END-closed blocks repeat.
    i = text.indexOf('-----BEGIN', i + 1)
  }
}

const OTHER_END_AT = /-----END [A-Z ]{0,40}PRIVATE KEY-----/y
const PEM_HEADER_AT = /[A-Za-z][A-Za-z0-9-]{0,40}:/y
/** A body line may hold several base64 words, each at least this long (prose words are shorter). */
const PEM_WORD_MIN = 16

/**
 * A block with no END of its type after it: through the last line that reads
 * as key material. That is the rest of the BEGIN line in base64 words, then
 * lines of one base64 word or of several long ones, `Name: value` headers
 * before them, END lines of another type and single blank lines.
 *
 * Read character by character, so a line is never collected before it is
 * judged: what is taken becomes the block, and what ends it is read only as
 * far as its first word that fails.
 */
function pemEnd(text: string, bodyStart: number): number {
  const n = text.length
  const lineSpace = (c: string): boolean => c !== '\n' && isSpace(c)
  const base64End = (j: number): number => {
    let k = j
    while (k < n && isBase64Char(text[k])) k += 1
    return k
  }
  const restOfLine = (j: number): number => {
    let k = j
    while (k < n && lineSpace(text[k])) k += 1
    return k
  }
  let last = bodyStart
  let k = bodyStart
  // The rest of the BEGIN line: base64 words up to the first other word.
  for (;;) {
    k = restOfLine(k)
    if (k >= n) return last
    if (text[k] === '\n') break
    const e = base64End(k)
    if (e === k || (e < n && !isSpace(text[e]))) return last
    last = e
    k = e
  }
  // The lines after it. Here `k` is at a line break.
  let blanks = 0
  let sawBody = false
  while (k < n && text[k] === '\n') {
    k = restOfLine(k + 1)
    if (k >= n) return last
    if (text[k] === '\n') {
      blanks += 1
      if (blanks > 1) return last
      continue
    }
    blanks = 0
    const e = base64End(k)
    if (e > k && (e >= n || isSpace(text[e]))) {
      // Base64 words to the end of the line: one, or several long ones.
      let end = e
      let t = restOfLine(e)
      const long = e - k >= PEM_WORD_MIN
      while (t < n && text[t] !== '\n') {
        const w = base64End(t)
        if (!long || w - t < PEM_WORD_MIN || (w < n && !isSpace(text[w]))) return last
        end = w
        t = restOfLine(w)
      }
      last = end
      k = t
      sawBody = true
      continue
    }
    OTHER_END_AT.lastIndex = k
    const other = OTHER_END_AT.exec(text)
    if (other) {
      const t = restOfLine(k + other[0].length)
      if (t < n && text[t] !== '\n') return last
      last = k + other[0].length
      k = t
      continue
    }
    PEM_HEADER_AT.lastIndex = k
    const header = sawBody ? null : PEM_HEADER_AT.exec(text)
    if (header && isSpace(text[k + header[0].length])) {
      let t = k + header[0].length
      let end = t
      while (t < n && text[t] !== '\n') {
        if (!isSpace(text[t])) end = t + 1
        t += 1
      }
      last = end
      k = t
      continue
    }
    return last
  }
  return last
}

// ----------------------------------------------------------------- credential

interface Literal {
  readonly needle: string
  /** Sticky; tried only where the needle starts. */
  readonly re: RegExp
  /** The needle is searched in the lowered text. */
  readonly anyCase?: boolean
  /**
   * A match may hold the needle again with a longer match after it (only
   * Bearer: its token is any non-space run), so the next search starts just
   * after the needle. The others end where their own characters end, so a
   * needle inside one ends at the same place.
   */
  readonly nested?: boolean
}

const CREDENTIAL_LITERALS: readonly Literal[] = [
  { needle: 'sk-', re: /sk-[A-Za-z0-9_-]{16,}/y },
  { needle: 'ghp_', re: /ghp_[A-Za-z0-9]{20,}/y },
  { needle: 'gho_', re: /gho_[A-Za-z0-9]{20,}/y },
  { needle: 'ghs_', re: /ghs_[A-Za-z0-9]{20,}/y },
  { needle: 'ghu_', re: /ghu_[A-Za-z0-9]{20,}/y },
  { needle: 'github_pat_', re: /github_pat_\w{20,}/y },
  { needle: 'AKIA', re: /AKIA[0-9A-Z]{16}/y },
  { needle: 'xox', re: /xox[abprs]-[A-Za-z0-9-]{10,}/y },
  // `Bearer` in any ASCII case, as HTTP headers write it.
  { needle: 'bearer', re: /bearer\s{1,4}\S{16,}/iy, anyCase: true, nested: true },
]

function literalFinder(spec: Literal): Finder {
  return ({ text, lower }, emit) => {
    const hay = spec.anyCase ? lower : text
    let i = hay.indexOf(spec.needle)
    while (i >= 0) {
      spec.re.lastIndex = i
      const m = spec.re.exec(text)
      if (m) {
        emit({ start: i, end: i + m[0].length })
        i = hay.indexOf(spec.needle, spec.nested ? i + 1 : i + m[0].length)
      } else {
        i = hay.indexOf(spec.needle, i + 1)
      }
    }
  }
}

function isTokenChar(c: string | undefined): boolean {
  return c !== undefined && (isAsciiAlnum(c) || c === '_' || c === '-')
}

/** `eyJ…{4,}.eyJ…{4,}.…{4,}`, read by hand. */
const jwts: Finder = ({ text }, emit) => {
  const tokenEnd = (j: number): number => {
    let k = j
    while (k < text.length && isTokenChar(text[k])) k += 1
    return k
  }
  let i = text.indexOf('eyJ')
  while (i >= 0) {
    const first = tokenEnd(i + 3)
    let end = -1
    if (first - (i + 3) >= 4 && text[first] === '.' && text.startsWith('eyJ', first + 1)) {
      const second = tokenEnd(first + 4)
      if (second - (first + 4) >= 4 && text[second] === '.') {
        const third = tokenEnd(second + 1)
        if (third - (second + 1) >= 4) end = third
      }
    }
    if (end >= 0) emit({ start: i, end })
    // An eyJ later in the same first segment reaches the same `first`, so it
    // ends where this one does, or fails the same way. One in a later segment
    // may run past this one.
    i = text.indexOf('eyJ', first)
  }
}

const ASSIGN_KEYS = [
  'access_key', 'access-key', 'accesskey',
  'private_key', 'private-key', 'privatekey',
  'password', 'passphrase', 'api_key', 'api-key', 'apikey',
  'passwd', 'secret', 'token', 'pwd', 'credential',
  'authorization',
] as const

/** Keys whose value is the rest of the line: `Authorization: Basic …`. */
const LINE_VALUE_KEYS = new Set<string>(['authorization'])

/** Most characters of an identifier that a key may sit in (`SECRET_KEY`, `db_password`). */
const MAX_KEY_RUN = 64
/** Most white space around the `=` or `:` (PR-Daemon B1). */
const MAX_GAP = 64

function isKeyChar(c: string | undefined): boolean {
  return isTokenChar(c) || c === '.'
}

/**
 * `key = value` and `key: value`, the key in any ASCII case, anywhere in an
 * identifier (`SECRET_KEY=`, `db_password:`) and optionally closed by a quote
 * (`"password": "…"`). The hit is the value only: a quoted value through its
 * closing quote, on any later line too; an unclosed quote through the next
 * word after it; `Authorization` through the end of its line; else the run up
 * to the next whitespace.
 */
function assignmentFinder(key: string): Finder {
  const lineValue = LINE_VALUE_KEYS.has(key)
  return ({ text, lower }, emit) => {
    // Each memo holds the last answer and the stretch it holds for: a later
    // search from inside that stretch gets the same answer.
    let runFrom = -1
    let runTo = -1
    /** The end of the run without whitespace that holds `j`. */
    const wordEnd = (j: number): number => {
      if (j >= runFrom && j <= runTo) return runTo
      let k = j
      while (k < text.length && !isSpace(text[k])) k += 1
      runFrom = j
      runTo = k
      return k
    }
    let lineFrom = -1
    let lineTo = -1
    const lineEnd = (j: number): number => {
      if (j >= lineFrom && j <= lineTo) return lineTo
      let k = j
      while (k < text.length && text[k] !== '\n' && text[k] !== '\r') k += 1
      lineFrom = j
      lineTo = k
      return k
    }
    /**
     * The closing `quote` at or after `j`, past backslash escapes; -1 when
     * there is none. `j` is just after an opening quote. Linear over all
     * keys: an opening quote follows `=`, `:` or white space, so it is never
     * escaped, and a search from an earlier opening quote of the same kind
     * ends at the latest on it. Searches do not overlap, and only the last
     * one can run to the end.
     */
    const closingQuote = (j: number, quote: string): number => {
      for (let k = j; k < text.length;) {
        if (text[k] === '\\') k += 2
        else if (text[k] === quote) return k
        else k += 1
      }
      return -1
    }
    const valueAt = (afterKey: number): Span | null => {
      let j = afterKey
      while (j - afterKey < MAX_KEY_RUN && isKeyChar(text[j])) j += 1
      if (text[j] === '"' || text[j] === "'") j += 1
      j = skipSpaces(text, j, MAX_GAP)
      if (text[j] !== ':' && text[j] !== '=') return null
      j = skipSpaces(text, j + 1, MAX_GAP)
      if (j >= text.length || isSpace(text[j])) return null
      const quote = text[j]
      if (quote === '"' || quote === "'") {
        const close = closingQuote(j + 1, quote)
        if (close > j + 1) return { start: j, end: close + 1 }
        if (close < 0) {
          // No closing quote anywhere: the value is the next word, wherever it is (B2).
          const word = skipSpaces(text, j + 1, MAX_GAP)
          return { start: j, end: word < text.length && !isSpace(text[word]) ? wordEnd(word) : j + 1 }
        }
      }
      return { start: j, end: lineValue ? lineEnd(j) : wordEnd(j) }
    }
    let i = lower.indexOf(key)
    while (i >= 0) {
      const value = valueAt(i + key.length)
      if (value) emit(value)
      // The next key may sit inside this value and quote a longer one.
      i = lower.indexOf(key, i + 1)
    }
  }
}

function skipSpaces(text: string, j: number, most: number): number {
  let k = j
  while (k - j < most && k < text.length && isSpace(text[k])) k += 1
  return k
}

/** The password in `scheme://user:password@host` (PR-Daemon N4). */
const urlPasswords: Finder = ({ text }, emit) => {
  for (let at = text.indexOf('://'); at >= 0; at = text.indexOf('://', at + 1)) {
    let k = at + 3
    while (k - at - 3 < 256 && k < text.length && isUserinfoChar(text[k]) && text[k] !== ':') k += 1
    // The user may be empty (`redis://:secret@host`).
    if (text[k] !== ':') continue
    const start = k + 1
    let e = start
    while (e - start < 256 && e < text.length && isUserinfoChar(text[e])) e += 1
    if (e > start && text[e] === '@') emit({ start, end: e })
  }
}

function isUserinfoChar(c: string | undefined): boolean {
  return c !== undefined && !isSpace(c) && c !== '/' && c !== '@' && c !== '?' && c !== '#'
}

// ---------------------------------------------------------------------- email

function isEmailLocal(c: string | undefined): boolean {
  return isAsciiAlnum(c) || c === '.' || c === '_' || c === '%' || c === '+' || c === '-'
}

function isEmailDomain(c: string | undefined): boolean {
  return isAsciiAlnum(c) || c === '.' || c === '-'
}

const emails: Finder = ({ text }, emit) => {
  for (let at = text.indexOf('@'); at >= 0; at = text.indexOf('@', at + 1)) {
    let start = at
    while (start > 0 && at - start < 64 && isEmailLocal(text[start - 1])) start -= 1
    if (start === at) continue
    const end = matchEmailDomain(text, at + 1)
    if (end !== null) emit({ start, end })
  }
}

/** Longest body that still leaves `.<letters>{2,24}`, matching the greedy pattern. */
function matchEmailDomain(text: string, i: number): number | null {
  const limit = Math.min(text.length, i + 253 + 1 + 24)
  let j = i
  while (j < limit && isEmailDomain(text[j])) j += 1
  for (let dot = j - 1; dot > i; dot -= 1) {
    if (text[dot] !== '.') continue
    if (dot - i > 253) continue
    let t = dot + 1
    while (t - dot - 1 < 24 && t < text.length && isAsciiLetter(text[t])) t += 1
    if (t - dot - 1 >= 2) return t
  }
  return null
}

// ---------------------------------------------------------------------- phone

/** Between the digits of a mainland mobile. Not a dot: `172.31.255.255` is an address. */
function isPhoneJoin(c: string | undefined): boolean {
  return c === ' ' || c === '-' || c === '\u00a0'
}

/** Mainland mobiles: `1[3-9]` and nine more digits, optionally after +86 or 86. */
const cnPhones: Finder = ({ text }, emit) => {
  for (let i = 0; i < text.length; i += 1) {
    const c = text[i]
    if (c !== '+' && c !== '8' && c !== '1') continue
    if (i > 0 && isDigit(text[i - 1])) continue
    const end = cnPhoneAt(text, i)
    if (end !== null) emit({ start: i, end })
  }
}

function cnPhoneAt(text: string, i: number): number | null {
  const afterJoin = (j: number): number => (isPhoneJoin(text[j]) ? j + 1 : j)
  if (text.startsWith('+86', i)) return mobileAt(text, afterJoin(i + 3))
  if (text.startsWith('86', i)) {
    const end = mobileAt(text, afterJoin(i + 2))
    if (end !== null) return end
  }
  return mobileAt(text, i)
}

/** Eleven digits from `j`, one separator allowed between any two, and no digit right after. */
function mobileAt(text: string, j: number): number | null {
  let digits = 0
  let k = j
  while (k < text.length && digits < 11) {
    const c = text[k]
    if (isDigit(c)) {
      if (digits === 0 && c !== '1') return null
      if (digits === 1 && (c < '3' || c > '9')) return null
      digits += 1
      k += 1
      continue
    }
    if (digits > 0 && isPhoneJoin(c) && isDigit(text[k + 1])) {
      k += 1
      continue
    }
    break
  }
  if (digits !== 11 || isDigit(text[k])) return null
  return k
}

/** `+` and 8–15 digits, at most two of space, dash, dot or brackets between any two. */
const intlPhones: Finder = ({ text }, emit) => {
  let i = text.indexOf('+')
  while (i >= 0) {
    const end = i === 0 || !isDigit(text[i - 1]) ? intlPhoneAt(text, i) : null
    if (end !== null) {
      emit({ start: i, end })
      i = text.indexOf('+', end)
    } else {
      i = text.indexOf('+', i + 1)
    }
  }
}

function intlPhoneAt(text: string, i: number): number | null {
  const joins = (c: string | undefined): boolean => isPhoneJoin(c) || c === '.' || c === '(' || c === ')'
  let digits = 0
  let end = -1
  let k = i + 1
  while (k < text.length) {
    if (isDigit(text[k])) {
      digits += 1
      if (digits > 15) return null
      k += 1
      end = k
      continue
    }
    let s = k
    while (s - k < 2 && joins(text[s])) s += 1
    if (s > k && isDigit(text[s])) {
      k = s
      continue
    }
    break
  }
  return digits >= 8 ? end : null
}

// -------------------------------------------------------------------- id_card

const ID_WEIGHTS = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2]
const ID_CODES = '10X98765432'

/** GB 11643 eighteen-character numbers with a valid check character, with no digit right before or after. */
const idCards: Finder = ({ text }, emit) => {
  for (let i = 0; i + 18 <= text.length; i += 1) {
    // Only a digit next to it makes it part of a longer number; letters do
    // not (`ID110101…`, PR-Daemon B5).
    if (!isDigit(text[i]) || (i > 0 && isDigit(text[i - 1]))) continue
    if (isIdAt(text, i)) emit({ start: i, end: i + 18 })
  }
}

function isIdAt(text: string, i: number): boolean {
  let sum = 0
  for (let k = 0; k < 17; k += 1) {
    if (!isDigit(text[i + k])) return false
    sum += (text.charCodeAt(i + k) - 48) * ID_WEIGHTS[k]
  }
  const last = text[i + 17]
  if (!isDigit(last) && last !== 'X' && last !== 'x') return false
  if (isDigit(text[i + 18])) return false
  return last.toUpperCase() === ID_CODES[sum % 11]
}

// ------------------------------------------------------------------ bank_card

function isCardJoin(c: string | undefined): boolean {
  return c === ' ' || c === '-' || c === '\u00a0'
}

/**
 * 13–19 digits passing Luhn: one run, or groups of at least three digits split
 * by single spaces or dashes in any mix. The longest prefix ending at a group's
 * end wins.
 */
const bankCards: Finder = ({ text }, emit) => {
  for (let i = 0; i < text.length; i += 1) {
    if (!isDigit(text[i]) || (i > 0 && isDigit(text[i - 1]))) continue
    const end = bankCardAt(text, i)
    if (end !== null) emit({ start: i, end })
  }
}

function bankCardAt(text: string, i: number): number | null {
  const digits: number[] = []
  const cuts: Array<{ count: number; end: number }> = []
  let group = 0
  let k = i
  while (k < text.length && digits.length <= 19) {
    const c = text[k]
    if (isDigit(c)) {
      digits.push(c.charCodeAt(0) - 48)
      group += 1
      k += 1
      continue
    }
    if (group >= 3 && isCardJoin(c) && isDigit(text[k + 1])) {
      cuts.push({ count: digits.length, end: k })
      group = 0
      k += 1
      continue
    }
    break
  }
  if (digits.length <= 19 && group >= 3 && !isDigit(text[k])) cuts.push({ count: digits.length, end: k })
  for (let c = cuts.length - 1; c >= 0; c -= 1) {
    const { count, end } = cuts[c]
    if (count >= 13 && count <= 19 && luhnOk(digits, count)) return end
  }
  return null
}

function luhnOk(digits: readonly number[], count: number): boolean {
  let sum = 0
  let alt = false
  for (let i = count - 1; i >= 0; i -= 1) {
    let n = digits[i]
    if (alt) {
      n *= 2
      if (n > 9) n -= 9
    }
    sum += n
    alt = !alt
  }
  return sum % 10 === 0
}

// ----------------------------------------------------------------------- path

const PATH_PREFIXES = ['/Volumes/', '/private/', '/Users/', '/home/', '/root/', '/var/', '/etc/', '/opt/']
const PATH_STOP = new Set(['`', '"', "'", ')', ']', '>', '，', '。'])

function isPathStop(c: string | undefined): boolean {
  return c === undefined || isSpace(c) || PATH_STOP.has(c)
}

/**
 * Local paths up to the next stop character. A Unix path inside a URL
 * (`https://host/home/…`) is the URL's own path and is left alone; a `file:`
 * URL is not a URL here, and a Windows or UNC path is a path wherever it is
 * (PR-Daemon B4).
 */
const paths: Finder = ({ text }, emit) => {
  const unix = [
    ...PATH_PREFIXES.map((prefix) => forwardIndex(text, prefix)),
    forwardIndex(text, '~/', (at) => !isAsciiAlnum(text[at - 1])),
  ]
  const windows = [
    windowsStart(text, ':\\'),
    windowsStart(text, ':/'),
    forwardIndex(text, '\\\\', (at) => uncAt(text, at)),
  ]
  const inUrl = urlContext(text)
  let cursor = 0
  for (;;) {
    let best = -1
    // Whether a Windows or UNC path starts at `best`: then no URL hides it.
    let anywhere = false
    for (const [starts, free] of [[unix, false], [windows, true]] as const) {
      for (const next of starts) {
        const at = next(cursor)
        if (at < 0) continue
        if (best < 0 || at < best) {
          best = at
          anywhere = free
        } else if (at === best) {
          anywhere ||= free
        }
      }
    }
    if (best < 0) return
    if (!anywhere && inUrl(best)) {
      cursor = best + 1
      continue
    }
    let end = best
    while (end < text.length && !isPathStop(text[end])) end += 1
    emit({ start: best, end })
    cursor = end
  }
}

/** `X:\` or `X:/` (not `X://`) with no letter or digit before the drive letter. */
function windowsStart(text: string, sep: ':\\' | ':/'): (from: number) => number {
  const colon = forwardIndex(text, sep, (at) => at > 0 && isAsciiLetter(text[at - 1]) && !isAsciiAlnum(text[at - 2]) && !(sep === ':/' && text[at + 2] === '/'))
  return (from) => {
    const at = colon(from + 1)
    return at < 0 ? -1 : at - 1
  }
}

function uncAt(text: string, i: number): boolean {
  let j = i + 2
  const part = (): number => {
    const begin = j
    while (j < text.length && j - begin < 200 && text[j] !== '\\' && !isPathStop(text[j])) j += 1
    return j - begin
  }
  if (part() < 1 || text[j] !== '\\') return false
  j += 1
  return part() >= 1
}

/**
 * Whether a path starting at `start` is inside a URL: a `://` at most 2048
 * characters back with no stop character since. `start` only grows.
 */
function urlContext(text: string): (start: number) => boolean {
  let scanned = 0
  let scheme = -1
  return (start) => {
    for (; scanned < start; scanned += 1) {
      const c = text[scanned]
      if (isPathStop(c)) scheme = -1
      else if (c === ':' && text[scanned + 1] === '/' && text[scanned + 2] === '/') scheme = isFileScheme(text, scanned) ? -1 : scanned
    }
    return scheme >= 0 && start - scheme <= 2048
  }
}

/** `file` (any case) ends at `colon` as a whole word. */
function isFileScheme(text: string, colon: number): boolean {
  return colon >= 4 && text.slice(colon - 4, colon).toLowerCase() === 'file' && !isAsciiAlnum(text[colon - 5])
}

// ----------------------------------------------------------- internal_address

const HOST_SUFFIXES = ['.intranet', '.internal', '.local', '.corp', '.lan']

/** Private IPv4: 10/8, 127/8, 192.168/16, 172.16/12, 169.254/16, 100.64/10, optionally with a port. */
const privateIps: Finder = ({ text }, emit) => {
  for (let i = 0; i < text.length; i += 1) {
    if (!isDigit(text[i])) continue
    if (i > 0 && (isDigit(text[i - 1]) || text[i - 1] === '.')) continue
    const end = privateIpAt(text, i)
    if (end !== null) emit({ start: i, end })
  }
}

function readOctet(text: string, j: number): { n: number; j: number } | null {
  if (!isDigit(text[j])) return null
  let n = 0
  let k = j
  while (k - j < 3 && k < text.length && isDigit(text[k])) {
    n = n * 10 + (text.charCodeAt(k) - 48)
    k += 1
  }
  if (n > 255) return null
  return { n, j: k }
}

function privateIpAt(text: string, i: number): number | null {
  let j = i
  const octets: number[] = []
  for (let part = 0; part < 4; part += 1) {
    if (part > 0) {
      if (text[j] !== '.') return null
      j += 1
    }
    const oct = readOctet(text, j)
    if (!oct) return null
    octets.push(oct.n)
    j = oct.j
  }
  // A dot ends a sentence unless a digit follows (`See 10.0.0.1.`, PR-Daemon N4).
  if (isDigit(text[j]) || (text[j] === '.' && isDigit(text[j + 1]))) return null
  const [a, b] = octets
  const priv = a === 10 || a === 127 || (a === 192 && b === 168) || (a === 172 && b >= 16 && b <= 31)
    || (a === 169 && b === 254) || (a === 100 && b >= 64 && b <= 127)
  if (!priv) return null
  return portEnd(text, j) ?? j
}

/** The end of `:<1–5 digits>` at `j`, or null. */
function portEnd(text: string, j: number): number | null {
  if (text[j] !== ':') return null
  let k = j + 1
  while (k - j - 1 < 5 && k < text.length && isDigit(text[k])) k += 1
  if (k === j + 1 || isDigit(text[k])) return null
  return k
}

/** `localhost:<port>` in any ASCII case. */
const localhosts: Finder = ({ text, lower }, emit) => {
  for (let i = lower.indexOf('localhost:'); i >= 0; i = lower.indexOf('localhost:', i + 1)) {
    if (i > 0 && (isAsciiAlnum(text[i - 1]) || text[i - 1] === '-')) continue
    const end = portEnd(text, i + 'localhost'.length)
    if (end !== null) emit({ start: i, end })
  }
}

function isHostChar(c: string | undefined): boolean {
  return isAsciiAlnum(c) || c === '.' || c === '-'
}

/** `<name>.<suffix>` in any ASCII case, the suffix ending the host name. */
function hostFinder(suffix: string): Finder {
  return ({ text, lower }, emit) => {
    // A host never starts before the end of the one found last.
    let floor = 0
    for (let i = lower.indexOf(suffix); i >= 0; i = lower.indexOf(suffix, i + 1)) {
      const end = i + suffix.length
      // A root dot (`db.corp./`) still ends the name (PR-Daemon N4).
      const rootDot = text[end] === '.' && !isHostChar(text[end + 1])
      if ((isHostChar(text[end]) && !rootDot) || i === 0 || !isAsciiAlnum(text[i - 1])) continue
      let s = i
      while (s > floor && i - s < 300 && isHostChar(text[s - 1])) s -= 1
      emit({ start: s, end })
      floor = end
    }
  }
}

/** IPv6 unique local (fc00::/7) and link-local (fe80::/10) addresses. */
const IPV6_LOCAL = /(?:f[cd][0-9a-f]{2}|fe[89ab][0-9a-f]):(?:[0-9a-f]{0,4}:){1,6}[0-9a-f]{0,4}/y

function isHexOrColon(c: string | undefined): boolean {
  return isDigit(c) || (c !== undefined && c >= 'a' && c <= 'f') || c === ':'
}

const localIpv6s: Finder = ({ lower }, emit) => {
  for (let i = lower.indexOf('f'); i >= 0; i = lower.indexOf('f', i + 1)) {
    if (i > 0 && (isHexOrColon(lower[i - 1]) || isAsciiLetter(lower[i - 1]))) continue
    IPV6_LOCAL.lastIndex = i
    const m = IPV6_LOCAL.exec(lower)
    if (m) emit({ start: i, end: i + m[0].length })
  }
}

// ---------------------------------------------------------------------- rules

interface Rule {
  readonly category: RedactionCategory
  readonly finders: readonly Finder[]
}

let rulesTable: readonly Rule[] | null = null

/** The rule table, in priority order: a joined hit takes the earliest category. */
function redactionRules(): readonly Rule[] {
  rulesTable ??= [
    { category: 'private_key', finders: [privateKeys] },
    { category: 'credential', finders: [jwts, urlPasswords, ...CREDENTIAL_LITERALS.map(literalFinder), ...ASSIGN_KEYS.map(assignmentFinder)] },
    { category: 'email', finders: [emails] },
    { category: 'phone', finders: [cnPhones, intlPhones] },
    { category: 'id_card', finders: [idCards] },
    { category: 'bank_card', finders: [bankCards] },
    { category: 'path', finders: [paths] },
    { category: 'internal_address', finders: [privateIps, localIpv6s, localhosts, ...HOST_SUFFIXES.map(hostFinder)] },
  ]
  return rulesTable
}

// ------------------------------------------------- wider shapes for postScan

/** 15 or more digits in a row, whatever is around them: `id_card` for 17
 *  digits and a check character or 18 digits, `bank_card` otherwise. */
const longDigitRuns: (category: 'id_card' | 'bank_card') => Finder = (category) => ({ text }, emit) => {
  for (let i = 0; i < text.length;) {
    if (!isDigit(text[i])) {
      i += 1
      continue
    }
    let k = i
    while (k < text.length && isDigit(text[k])) k += 1
    let end = k
    if (k - i === 17 && (text[k] === 'X' || text[k] === 'x')) end = k + 1
    const isId = end - i === 18
    if (end - i >= 15 && isId === (category === 'id_card')) emit({ start: i, end })
    i = k
  }
}

/** Home directories and `file:` URLs wherever they are, URLs included. */
const widePaths: Finder = ({ text, lower }, emit) => {
  const starts = [
    forwardIndex(lower, '/users/'),
    forwardIndex(lower, '/home/'),
    forwardIndex(lower, '\\users\\'),
    // `file:/…` as a word, not `profile:`.
    forwardIndex(lower, 'file:/', (at) => !isAsciiAlnum(text[at - 1])),
  ]
  let cursor = 0
  for (;;) {
    let best = -1
    for (const next of starts) {
      const at = next(cursor)
      if (at >= 0 && (best < 0 || at < best)) best = at
    }
    if (best < 0) return
    let end = best
    while (end < text.length && !isPathStop(text[end])) end += 1
    if (end > best) emit({ start: best, end })
    cursor = Math.max(end, best + 1)
  }
}

let postScanTable: readonly Rule[] | null = null

/** The rule table with the wider shapes added, each under its own category. */
function postScanRules(): readonly Rule[] {
  postScanTable ??= redactionRules().map((rule) => {
    const wider: Finder[] = rule.category === 'id_card' ? [longDigitRuns('id_card')]
      : rule.category === 'bank_card' ? [longDigitRuns('bank_card')]
        : rule.category === 'path' ? [widePaths]
          : []
    return { category: rule.category, finders: [...rule.finders, ...wider] }
  })
  return postScanTable
}

/** For tests: the rule table alone, as preRedact sees it (postScan without the wider shapes). */
function tableScan(text: string): RedactionHit[] {
  if (text.length > MAX_REDACT_INPUT) return [{ category: 'credential', start: 0, end: text.length }]
  return scan(text)
}

export const testing = { isSpace, tableScan }
