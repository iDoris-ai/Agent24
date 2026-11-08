// Deterministic redaction for a local knowledge-entry draft. Pure functions:
// no files, no logs. Rules run in array order; the earliest rule owns a span.
// Linear time on any input up to MAX_REDACT_INPUT: no regex runs over more
// than a few hundred characters, JWTs are read by hand (no backtracking), the
// next position of each needle is remembered while a rule moves forward, and
// the spans already taken are kept sorted for binary search.

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

interface Rule {
  readonly category: RedactionCategory
  readonly find: (text: string, from: number) => Span | null
}

export function preRedact(text: string): { text: string; counts: Partial<Record<RedactionCategory, number>> } {
  if (text.length > MAX_REDACT_INPUT) throw new RangeError('input exceeds MAX_REDACT_INPUT')
  const hits = scan(text)
  const counts: Partial<Record<RedactionCategory, number>> = {}
  let out = ''
  let cursor = 0
  for (const hit of hits) {
    out += text.slice(cursor, hit.start)
    out += placeholder(hit.category)
    counts[hit.category] = (counts[hit.category] ?? 0) + 1
    cursor = hit.end
  }
  out += text.slice(cursor)
  return { text: out, counts }
}

export function postScan(text: string): RedactionHit[] {
  if (text.length > MAX_REDACT_INPUT) return [{ category: 'credential', start: 0, end: text.length }]
  return scan(text)
}

const REDACTION_RULES: readonly Rule[] = [
  { category: 'private_key', find: findPrivateKey },
  { category: 'credential', find: findCredential },
  { category: 'email', find: findEmail },
  { category: 'phone', find: findPhone },
  { category: 'id_card', find: findIdCard },
  { category: 'bank_card', find: findBankCard },
  { category: 'path', find: findPath },
  { category: 'internal_address', find: findInternalAddress },
]

function scan(text: string): RedactionHit[] {
  beginScan(text)
  try {
    const hits: RedactionHit[] = []
    const blocked = new SpanSet(placeholderSpans(text))
    for (const rule of REDACTION_RULES) {
      let from = 0
      while (from < text.length) {
        from = blocked.advancePast(from)
        if (from >= text.length) break
        const span = rule.find(text, from)
        if (!span || span.end <= span.start || span.start < from || span.end > text.length) break
        if (blocked.overlaps(span) || text.slice(span.start, span.end).includes(PLACEHOLDER_MARK)) {
          const next = Math.max(span.start + 1, blocked.overlapEnd(span))
          if (next <= from) break
          from = next
          continue
        }
        hits.push({ category: rule.category, start: span.start, end: span.end })
        blocked.add(span)
        from = span.end
      }
    }
    hits.sort((a, b) => a.start - b.start || a.end - b.end)
    return hits
  } finally {
    endScan()
  }
}

function placeholderSpans(text: string): Span[] {
  const spans: Span[] = []
  for (const label of Object.values(LABELS)) {
    const token = `${PLACEHOLDER_MARK}${label}〕`
    let from = 0
    while (from < text.length) {
      const at = text.indexOf(token, from)
      if (at < 0) break
      spans.push({ start: at, end: at + token.length })
      from = at + token.length
    }
  }
  spans.sort((a, b) => a.start - b.start)
  return spans
}

/** Spans already taken: never overlapping, kept sorted by start (so by end too). */
class SpanSet {
  private readonly spans: Span[]
  constructor(initial: Span[]) {
    this.spans = initial
  }

  /** Index of the first span whose end is past `at`. */
  private firstEndingAfter(at: number): number {
    let lo = 0
    let hi = this.spans.length
    while (lo < hi) {
      const mid = (lo + hi) >> 1
      if (this.spans[mid].end <= at) lo = mid + 1
      else hi = mid
    }
    return lo
  }

  overlaps(span: Span): boolean {
    const b = this.spans[this.firstEndingAfter(span.start)]
    return b !== undefined && b.start < span.end
  }

  overlapEnd(span: Span): number {
    let end = span.start + 1
    for (let k = this.firstEndingAfter(span.start); k < this.spans.length && this.spans[k].start < span.end; k += 1) {
      end = Math.max(end, this.spans[k].end)
    }
    return end
  }

  advancePast(from: number): number {
    let cursor = from
    for (let k = this.firstEndingAfter(cursor); k < this.spans.length && this.spans[k].start <= cursor; k += 1) {
      cursor = this.spans[k].end
    }
    return cursor
  }

  add(span: Span): void {
    this.spans.splice(this.firstEndingAfter(span.start), 0, span)
  }
}

// Each finder asks for the first position at or after `from`. Within one rule
// `from` only grows, so an answer still ahead of it (or "none") stays right.
let memoText: string | null = null
const memo = new Map<string, { from: number; at: number }>()

function beginScan(text: string): void {
  memoText = text
  lowered = null
  memo.clear()
  spanMemo.clear()
}

function endScan(): void {
  memoText = null
  lowered = null
  memo.clear()
  spanMemo.clear()
}

function firstFrom(text: string, key: string, from: number, find: (from: number) => number): number {
  if (memoText !== text) return find(from)
  const known = memo.get(key)
  if (known && from >= known.from && (known.at < 0 || known.at >= from)) return known.at
  const at = find(from)
  memo.set(key, { from, at })
  return at
}

function indexFrom(text: string, needle: string, from: number): number {
  return firstFrom(text, `indexOf:${needle}`, from, (f) => text.indexOf(needle, f))
}

/** The same for a finder whose answer is the earliest span starting at or after `from`. */
const spanMemo = new Map<string, { from: number; span: Span | null }>()

function memoSpan(text: string, key: string, from: number, find: (from: number) => Span | null): Span | null {
  if (memoText !== text) return find(from)
  const known = spanMemo.get(key)
  if (known && from >= known.from && (known.span === null || known.span.start >= from)) return known.span
  const span = find(from)
  spanMemo.set(key, { from, span })
  return span
}

/** A-Z lowered, nothing else, so every offset stays the same. */
let lowered: string | null = null

function asciiLower(text: string): string {
  if (memoText === text && lowered !== null) return lowered
  const out = text.replace(/[A-Z]/g, (c) => c.toLowerCase())
  if (memoText === text) lowered = out
  return out
}

/** `indexFrom` ignoring ASCII case; `needle` is lower case. */
function indexFromCi(text: string, needle: string, from: number): number {
  return firstFrom(text, `indexOfCi:${needle}`, from, (f) => asciiLower(text).indexOf(needle, f))
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

function isSpace(c: string | undefined): boolean {
  return c !== undefined && /\s/.test(c)
}

const BEGIN_RE = /^-----BEGIN [A-Z ]{0,40}PRIVATE KEY-----/
const END_RE = /^-----END [A-Z ]{0,40}PRIVATE KEY-----/

function findPrivateKey(text: string, from: number): Span | null {
  let i = indexFrom(text, '-----BEGIN', from)
  while (i >= 0) {
    const head = BEGIN_RE.exec(text.slice(i, i + 80))
    if (head && head.index === 0) {
      const start = i
      const after = i + head[0].length
      const endAt = findPemEnd(text, after)
      if (endAt !== null) return { start, end: endAt }
      let line = after
      while (line < text.length && text[line] !== '\n' && text[line] !== '\r' && line - start < 400) line += 1
      return { start, end: line }
    }
    i = indexFrom(text, '-----BEGIN', i + 1)
  }
  return null
}

function findPemEnd(text: string, from: number): number | null {
  let i = indexFrom(text, '-----END', from)
  while (i >= 0) {
    const head = END_RE.exec(text.slice(i, i + 80))
    if (head && head.index === 0) return i + head[0].length
    i = indexFrom(text, '-----END', i + 1)
  }
  return null
}

/** `Bearer` in any ASCII case, as HTTP headers write it. */
const BEARER = { needle: 'bearer', re: /^bearer\s{1,4}\S{16,400}/i, max: 420 }

const CREDENTIAL_LITERALS: readonly { needle: string; re: RegExp; max: number }[] = [
  { needle: 'sk-', re: /^sk-[A-Za-z0-9_-]{16,200}/, max: 220 },
  { needle: 'ghp_', re: /^ghp_[A-Za-z0-9]{20,255}/, max: 280 },
  { needle: 'gho_', re: /^gho_[A-Za-z0-9]{20,255}/, max: 280 },
  { needle: 'ghs_', re: /^ghs_[A-Za-z0-9]{20,255}/, max: 280 },
  { needle: 'ghu_', re: /^ghu_[A-Za-z0-9]{20,255}/, max: 280 },
  { needle: 'github_pat_', re: /^github_pat_\w{20,255}/, max: 280 },
  { needle: 'AKIA', re: /^AKIA[0-9A-Z]{16}/, max: 24 },
  { needle: 'xoxa-', re: /^xox[abprs]-[A-Za-z0-9-]{10,200}/, max: 220 },
  { needle: 'xoxb-', re: /^xox[abprs]-[A-Za-z0-9-]{10,200}/, max: 220 },
  { needle: 'xoxp-', re: /^xox[abprs]-[A-Za-z0-9-]{10,200}/, max: 220 },
  { needle: 'xoxr-', re: /^xox[abprs]-[A-Za-z0-9-]{10,200}/, max: 220 },
  { needle: 'xoxs-', re: /^xox[abprs]-[A-Za-z0-9-]{10,200}/, max: 220 },
]

const ASSIGN_KEYS = [
  'access_key', 'access-key', 'accesskey',
  'password', 'api_key', 'api-key', 'apikey',
  'passwd', 'secret', 'token', 'pwd',
] as const

function isTokenChar(c: string | undefined): boolean {
  return c !== undefined && (isAsciiAlnum(c) || c === '_' || c === '-')
}

/** End of a run of token characters from `j`, or null when shorter than `min`. */
function tokenRun(text: string, j: number, min: number): number | null {
  let k = j
  while (k < text.length && isTokenChar(text[k])) k += 1
  return k - j >= min ? k : null
}

/** `eyJ…{4,}.eyJ…{4,}.…{4,}` at `i`, read by hand: the end, or null. */
function jwtAt(text: string, i: number): number | null {
  let j = tokenRun(text, i + 3, 4)
  if (j === null || text[j] !== '.' || !text.startsWith('eyJ', j + 1)) return null
  j = tokenRun(text, j + 4, 4)
  if (j === null || text[j] !== '.') return null
  return tokenRun(text, j + 1, 4)
}

function findJwt(text: string, from: number): Span | null {
  let i = indexFrom(text, 'eyJ', from)
  while (i >= 0) {
    const end = jwtAt(text, i)
    if (end !== null) return { start: i, end }
    // An eyJ later in the same first segment runs into the same end, so it fails the same way.
    let k = i + 3
    while (k < text.length && isTokenChar(text[k])) k += 1
    i = indexFrom(text, 'eyJ', Math.max(i + 1, k))
  }
  return null
}

function findCredential(text: string, from: number): Span | null {
  let best: Span | null = memoSpan(text, 'jwt', from, (f) => findJwt(text, f))
  const consider = (span: Span | null): void => {
    if (span && (!best || span.start < best.start)) best = span
  }
  for (const spec of CREDENTIAL_LITERALS) {
    consider(memoSpan(text, `literal:${spec.needle}`, from, (f) => findLiteral(text, f, spec.needle, spec.re, spec.max, false)))
  }
  consider(memoSpan(text, 'literal:bearer', from, (f) => findLiteral(text, f, BEARER.needle, BEARER.re, BEARER.max, true)))
  consider(findAssignment(text, from))
  return best
}

function findLiteral(text: string, from: number, needle: string, re: RegExp, max: number, ci: boolean): Span | null {
  const next = (f: number): number => (ci ? indexFromCi(text, needle, f) : indexFrom(text, needle, f))
  let i = next(from)
  while (i >= 0) {
    const m = re.exec(text.slice(i, i + max))
    if (m && m.index === 0) return { start: i, end: i + m[0].length }
    i = next(i + 1)
  }
  return null
}

/** The hit is the value only; the key stays in the text. Keys match in any ASCII case. */
function findAssignment(text: string, from: number): Span | null {
  let best: Span | null = null
  for (const key of ASSIGN_KEYS) {
    // The first key at or after `from` that is followed by a value.
    const at = firstFrom(text, `assign:${key}`, from, (f) => {
      let i = indexFromCi(text, key, f)
      while (i >= 0 && !matchAssignValue(text, i + key.length)) i = indexFromCi(text, key, i + 1)
      return i
    })
    if (at < 0) continue
    const value = matchAssignValue(text, at + key.length)
    if (value && (!best || value.start < best.start)) best = value
  }
  return best
}

function matchAssignValue(text: string, afterKey: number): Span | null {
  let j = afterKey
  let ws = 0
  while (ws < 3 && j < text.length && isSpace(text[j])) {
    j += 1
    ws += 1
  }
  if (text[j] !== ':' && text[j] !== '=') return null
  j += 1
  ws = 0
  while (ws < 3 && j < text.length && isSpace(text[j])) {
    j += 1
    ws += 1
  }
  const start = j
  const limit = Math.min(text.length, start + 400)
  while (j < limit && !isSpace(text[j])) j += 1
  if (j === start) return null
  return { start, end: j }
}

function isEmailLocal(c: string | undefined): boolean {
  return isAsciiAlnum(c) || c === '.' || c === '_' || c === '%' || c === '+' || c === '-'
}

function isEmailDomain(c: string | undefined): boolean {
  return isAsciiAlnum(c) || c === '.' || c === '-'
}

function findEmail(text: string, from: number): Span | null {
  let at = indexFrom(text, '@', from)
  while (at >= 0) {
    let local = 0
    let start = at
    while (start > 0 && local < 64 && isEmailLocal(text[start - 1])) {
      start -= 1
      local += 1
    }
    if (start < from) {
      start = from
      local = at - start
    }
    if (local >= 1 && local <= 64 && isEmailLocal(text[start])) {
      const end = matchEmailDomain(text, at + 1)
      if (end !== null) return { start, end }
    }
    at = indexFrom(text, '@', at + 1)
  }
  return null
}

/** Longest body that still leaves `.<letters>{2,24}`, matching the greedy pattern. */
function matchEmailDomain(text: string, i: number): number | null {
  const limit = Math.min(text.length, i + 253 + 1 + 24)
  let j = i
  while (j < limit && isEmailDomain(text[j])) j += 1
  for (let dot = j - 1; dot > i; dot -= 1) {
    if (text[dot] !== '.') continue
    const body = dot - i
    if (body < 1 || body > 253) continue
    let t = dot + 1
    let letters = 0
    while (letters < 24 && t < text.length && isAsciiLetter(text[t])) {
      t += 1
      letters += 1
    }
    if (letters >= 2) return t
  }
  return null
}

function findPhone(text: string, from: number): Span | null {
  const cn = findCnPhone(text, from)
  const intl = findIntlPhone(text, from)
  if (!cn) return intl
  if (!intl) return cn
  return intl.start < cn.start ? intl : cn
}

function findCnPhone(text: string, from: number): Span | null {
  return memoSpan(text, 'cnPhone', from, (f) => findCnPhoneRaw(text, f))
}

function findCnPhoneRaw(text: string, from: number): Span | null {
  for (let i = from; i < text.length; i += 1) {
    if (i > 0 && isDigit(text[i - 1])) continue
    const c = text[i]
    if (c !== '+' && c !== '8' && c !== '1') continue
    const span = parseCnPhone(text, i)
    if (span) return span
  }
  return null
}

function isMobileSecond(c: string | undefined): boolean {
  return c !== undefined && c >= '3' && c <= '9'
}

function parseCnPhone(text: string, i: number): Span | null {
  let j = i
  if (text.startsWith('+86', j)) {
    j += 3
    if (text[j] === ' ' || text[j] === '-') j += 1
  } else if (text.startsWith('86', j)) {
    const rest = j + 2
    const sep = text[rest] === ' ' || text[rest] === '-'
    const head = sep ? rest + 1 : rest
    if (text[head] === '1' && isMobileSecond(text[head + 1])) j = head
  }
  if (text[j] !== '1' || !isMobileSecond(text[j + 1])) return null
  let digits = 0
  let k = j
  while (digits < 11 && k < text.length) {
    if (isDigit(text[k])) {
      if (digits === 1 && !isMobileSecond(text[k])) return null
      k += 1
      digits += 1
      continue
    }
    if ((text[k] === ' ' || text[k] === '-') && digits > 0 && digits < 11 && isDigit(text[k + 1])) {
      k += 1
      continue
    }
    break
  }
  if (digits !== 11) return null
  if (k < text.length && isDigit(text[k])) return null
  return { start: i, end: k }
}

function findIntlPhone(text: string, from: number): Span | null {
  return memoSpan(text, 'intlPhone', from, (f) => findIntlPhoneRaw(text, f))
}

function findIntlPhoneRaw(text: string, from: number): Span | null {
  let i = indexFrom(text, '+', from)
  while (i >= 0) {
    if (i === 0 || !isDigit(text[i - 1])) {
      const span = parseIntlPhone(text, i)
      if (span) return span
    }
    i = indexFrom(text, '+', i + 1)
  }
  return null
}

function parseIntlPhone(text: string, i: number): Span | null {
  let j = i + 1
  let digits = ''
  let sepAfter = -1
  while (j < text.length && digits.length < 17) {
    const c = text[j]
    if (isDigit(c)) {
      digits += c
      j += 1
      continue
    }
    if (sepAfter < 0 && (c === ' ' || c === '-') && digits.length >= 1 && digits.length <= 3 && isDigit(text[j + 1])) {
      sepAfter = digits.length
      j += 1
      continue
    }
    break
  }
  if (j < text.length && isDigit(text[j])) return null
  if (digits.length < 8 || digits.length > 17) return null
  if (sepAfter >= 0) {
    const rest = digits.length - sepAfter
    if (rest < 6 || rest > 14) return null
  }
  return { start: i, end: j }
}

const ID_WEIGHTS = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2]
const ID_CODES = '10X98765432'

function findIdCard(text: string, from: number): Span | null {
  for (let i = from; i < text.length; i += 1) {
    if (!isDigit(text[i])) continue
    if (i > 0 && isAsciiAlnum(text[i - 1])) continue
    if (!isEighteenId(text, i)) continue
    const id = text.slice(i, i + 18)
    if (idChecksumOk(id)) return { start: i, end: i + 18 }
  }
  return null
}

function isEighteenId(text: string, i: number): boolean {
  if (i + 18 > text.length) return false
  for (let k = 0; k < 17; k += 1) if (!isDigit(text[i + k])) return false
  const last = text[i + 17]
  if (!isDigit(last) && last !== 'X' && last !== 'x') return false
  const after = text[i + 18]
  return !isAsciiAlnum(after)
}

function idChecksumOk(id: string): boolean {
  let sum = 0
  for (let i = 0; i < 17; i += 1) sum += (id.charCodeAt(i) - 48) * ID_WEIGHTS[i]
  return id[17].toUpperCase() === ID_CODES[sum % 11]
}

function findBankCard(text: string, from: number): Span | null {
  for (let i = from; i < text.length; i += 1) {
    if (!isDigit(text[i])) continue
    if (i > 0 && isDigit(text[i - 1])) continue
    const span = parseBankCard(text, i)
    if (span && luhnOk(span.digits)) return { start: i, end: span.end }
  }
  return null
}

function parseBankCard(text: string, i: number): { digits: string; end: number } | null {
  let digits = ''
  let sep: string | null = null
  let j = i
  while (j < text.length && digits.length < 19) {
    const c = text[j]
    if (isDigit(c)) {
      digits += c
      j += 1
      continue
    }
    if ((c === ' ' || c === '-') && digits.length > 0 && digits.length % 4 === 0 && isDigit(text[j + 1])) {
      if (sep === null) sep = c
      else if (sep !== c) break
      j += 1
      continue
    }
    break
  }
  if (digits.length < 13 || digits.length > 19) return null
  if (j < text.length && isDigit(text[j])) return null
  return { digits, end: j }
}

function luhnOk(digits: string): boolean {
  let sum = 0
  let alt = false
  for (let i = digits.length - 1; i >= 0; i -= 1) {
    let n = digits.charCodeAt(i) - 48
    if (alt) {
      n *= 2
      if (n > 9) n -= 9
    }
    sum += n
    alt = !alt
  }
  return sum % 10 === 0
}

const PATH_PREFIXES = ['/Volumes/', '/private/', '/Users/', '/home/', '/root/', '/var/', '/etc/', '/opt/']
const PATH_STOP = new Set([' ', '\t', '\n', '\r', '\f', '\v', '`', '"', "'", ')', ']', '>', '，', '。'])

function findPath(text: string, from: number): Span | null {
  let cursor = from
  while (cursor < text.length) {
    const span = nextPath(text, cursor)
    if (!span) return null
    if (span.start < cursor) return null
    if (!inUrl(text, span.start)) return span
    cursor = span.start + 1
  }
  return null
}

function nextPath(text: string, from: number): Span | null {
  let best = -1
  const consider = (at: number) => {
    if (at >= 0 && (best < 0 || at < best)) best = at
  }
  for (const prefix of PATH_PREFIXES) consider(indexFrom(text, prefix, from))
  consider(indexTilde(text, from))
  consider(indexWin(text, from))
  consider(indexUnc(text, from))
  if (best < 0) return null
  return extendPath(text, best)
}

function indexTilde(text: string, from: number): number {
  return firstFrom(text, 'tilde', from, (f) => indexTildeRaw(text, f))
}

function indexTildeRaw(text: string, from: number): number {
  let i = indexFrom(text, '~/', from)
  while (i >= 0) {
    if (i === 0 || !isAsciiAlnum(text[i - 1])) return i
    i = indexFrom(text, '~/', i + 1)
  }
  return -1
}

function indexWin(text: string, from: number): number {
  return firstFrom(text, 'win', from, (f) => indexWinRaw(text, f))
}

function indexWinRaw(text: string, from: number): number {
  for (let i = from; i < text.length - 2; i += 1) {
    if (i > 0 && isAsciiAlnum(text[i - 1])) continue
    const c = text[i]
    if (isAsciiLetter(c) && text[i + 1] === ':' && text[i + 2] === '\\') return i
  }
  return -1
}

function indexUnc(text: string, from: number): number {
  return firstFrom(text, 'unc', from, (f) => indexUncRaw(text, f))
}

function indexUncRaw(text: string, from: number): number {
  let i = indexFrom(text, '\\\\', from)
  while (i >= 0) {
    if (uncAt(text, i)) return i
    i = indexFrom(text, '\\\\', i + 1)
  }
  return -1
}

function uncAt(text: string, i: number): boolean {
  if (!text.startsWith('\\\\', i)) return false
  let j = i + 2
  let n = 0
  while (j < text.length && n < 200 && text[j] !== '\\' && !PATH_STOP.has(text[j])) {
    j += 1
    n += 1
  }
  if (n < 1 || text[j] !== '\\') return false
  j += 1
  n = 0
  while (j < text.length && n < 200 && text[j] !== '\\' && !PATH_STOP.has(text[j])) {
    j += 1
    n += 1
  }
  return n >= 1
}

function extendPath(text: string, start: number): Span {
  const limit = Math.min(text.length, start + 400)
  let end = start
  while (end < limit && !PATH_STOP.has(text[end])) end += 1
  return { start, end }
}

function inUrl(text: string, start: number): boolean {
  const begin = Math.max(0, start - 2048)
  for (let k = start - 1; k >= begin; k -= 1) {
    if (PATH_STOP.has(text[k])) return false
    if (text[k] === ':' && text[k + 1] === '/' && text[k + 2] === '/') return true
  }
  return false
}

const HOST_SUFFIXES = ['.intranet', '.internal', '.local', '.corp', '.lan']

function findInternalAddress(text: string, from: number): Span | null {
  const candidates = [findIp(text, from), findLocalhost(text, from), findHost(text, from)]
  let best: Span | null = null
  for (const span of candidates) {
    if (span && (!best || span.start < best.start)) best = span
  }
  return best
}

function findIp(text: string, from: number): Span | null {
  return memoSpan(text, 'ip', from, (f) => findIpRaw(text, f))
}

function findIpRaw(text: string, from: number): Span | null {
  for (let i = from; i < text.length; i += 1) {
    if (!isDigit(text[i])) continue
    if (i > 0 && (isDigit(text[i - 1]) || text[i - 1] === '.')) continue
    const span = parseIp(text, i)
    if (span) return span
  }
  return null
}

function readOctet(text: string, j: number): { n: number; j: number } | null {
  if (!isDigit(text[j])) return null
  let n = 0
  let d = 0
  let k = j
  while (d < 3 && k < text.length && isDigit(text[k])) {
    n = n * 10 + (text.charCodeAt(k) - 48)
    k += 1
    d += 1
  }
  if (n > 255) return null
  return { n, j: k }
}

function parseIp(text: string, i: number): Span | null {
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
  if (j < text.length && (isDigit(text[j]) || text[j] === '.')) return null
  const a = octets[0]
  const b = octets[1]
  const priv = a === 10 || a === 127 || (a === 192 && b === 168) || (a === 172 && b >= 16 && b <= 31)
  if (!priv) return null
  if (text[j] === ':') {
    let k = j + 1
    let pd = 0
    while (pd < 5 && k < text.length && isDigit(text[k])) {
      k += 1
      pd += 1
    }
    if (pd >= 1 && (k >= text.length || !isDigit(text[k]))) return { start: i, end: k }
  }
  return { start: i, end: j }
}

function findLocalhost(text: string, from: number): Span | null {
  return memoSpan(text, 'localhost', from, (f) => findLocalhostRaw(text, f))
}

function findLocalhostRaw(text: string, from: number): Span | null {
  let i = indexFrom(text, 'localhost:', from)
  while (i >= 0) {
    if (i === 0 || (!isAsciiAlnum(text[i - 1]) && text[i - 1] !== '-')) {
      let k = i + 'localhost:'.length
      let pd = 0
      while (pd < 5 && k < text.length && isDigit(text[k])) {
        k += 1
        pd += 1
      }
      if (pd >= 1 && (k >= text.length || !isDigit(text[k]))) return { start: i, end: k }
    }
    i = indexFrom(text, 'localhost:', i + 1)
  }
  return null
}

function isHostChar(c: string | undefined): boolean {
  return isAsciiAlnum(c) || c === '.' || c === '-'
}

function findHost(text: string, from: number): Span | null {
  return memoSpan(text, 'host', from, (f) => findHostRaw(text, f))
}

function findHostRaw(text: string, from: number): Span | null {
  let best: Span | null = null
  for (const suf of HOST_SUFFIXES) {
    let i = indexFrom(text, suf, from)
    while (i >= 0) {
      const end = i + suf.length
      const next = text[end]
      if (next !== undefined && isHostChar(next)) {
        i = indexFrom(text, suf, i + 1)
        continue
      }
      if (!(i > 0 && isAsciiAlnum(text[i - 1]))) {
        i = indexFrom(text, suf, i + 1)
        continue
      }
      let s = i
      let walked = 0
      while (s > 0 && walked < 300 && isHostChar(text[s - 1])) {
        s -= 1
        walked += 1
      }
      if (s >= from && s < i) {
        const span = { start: s, end }
        if (!best || span.start < best.start) best = span
        break
      }
      i = indexFrom(text, suf, i + 1)
    }
  }
  return best
}
