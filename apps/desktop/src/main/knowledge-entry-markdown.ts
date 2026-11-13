// Restricted Markdown for a knowledge-entry body, ported from WeKnora
// agent24RestrictedMarkdownViolation (candidate_content.go). Callers hand in a
// body that is already NFC and uses LF line breaks.
//
// Unit: UTF-16 code units (JavaScript string indexes). The Go scanner walks
// bytes, and the two agree on this check: every sentinel is ASCII, a non-ASCII
// scalar never contains an ASCII byte (UTF-8) or an ASCII code unit (a surrogate
// is not ASCII either), and the only length test is "longer than the ASCII
// prefix https://". An ASCII case fold below is used instead of toLowerCase,
// which can change a string's length.

/** What restrictedMarkdownProblem reports. One Go rejection site each, except unsafe_link. */
export type MarkdownProblem =
  // agent24MarkdownLineViolation case '<': '<' then an ASCII letter, '/', '!' or '?'.
  | 'raw_html'
  // agent24MarkdownLineViolation case '!': the two characters "![".
  | 'image'
  // agent24MarkdownLineViolation case '[': the two characters "[^".
  | 'footnote'
  // agent24MarkdownLineViolation case '\\': '\\' at the end of the line, or before ASCII punctuation.
  | 'escape'
  // agent24MarkdownLineViolation case '`': a backtick run with no same-length closer on this line.
  | 'unpaired_backtick'
  // agent24MarkdownRawViolation case ':': "]:" and the rest of the line is not a safe https destination.
  // agent24MarkdownLineViolation case ']': "](" is not optional space/tab, a safe https destination, and ')'.
  // Both sites are the one "destination is not a plain https URL" rule.
  | 'unsafe_link'
  // agent24MarkdownRawViolation case '[': "][" whose label has no ']' on this line, or contains '`'.
  | 'reference_label'

const HTTPS = 'https://'
const SPACE_TAB = ' \t'
/** Inline destination end: IndexAny of space, tab, or ')'. Brackets are not terminators. */
const SPACE_TAB_PAREN = ' \t)'
/** agent24MarkdownSafeDestination's ContainsAny set. ':' and '/' are allowed. */
const UNSAFE_IN_DEST = ' \t`<>"\'\\()[]'

export function restrictedMarkdownProblem(body: string): { problem: MarkdownProblem; line: number } | null {
  const lines = body.split('\n')
  let fence = ''
  for (let index = 0; index < lines.length; index++) {
    const line = lines[index]
    if (fence !== '') {
      if (closesFence(line, fence)) fence = ''
      continue
    }
    const opened = opensFence(line)
    if (opened !== null) {
      fence = opened
      continue
    }
    // Raw "]:" / "][" over the whole line, including what a code span would hide.
    // Only if that passes does the left-to-right prose scan run. First hit wins.
    const raw = rawViolation(line)
    if (raw !== null) return { problem: raw, line: index + 1 }
    const prose = lineViolation(line)
    if (prose !== null) return { problem: prose, line: index + 1 }
  }
  return null
}

/**
 * Rewrites brackets inside https link destinations so a URL that only failed
 * the safe-destination rule for those characters will pass
 * restrictedMarkdownProblem. "brackets" here means both square brackets and
 * parentheses: '[' → %5B, ']' → %5D, '(' → %28, ')' → %29. Link text is left
 * unchanged. Idempotent: the replacements contain none of those four characters.
 *
 * Inline "](" : after "](" and any leading space or tab, the destination runs
 * up to the ')' that pairs with that opening '(', by CommonMark's rule that
 * parentheses inside a destination come in balanced pairs. A ')' seen while
 * an inner '(' is still open closes that pair; the ')' seen when none is open
 * is the link's end. That closing ')' stays as itself. When no such ')' exists
 * before the end of the line, or a space or tab comes before it, the text is
 * not a plain destination and is left unchanged for restrictedMarkdownProblem
 * to reject (so a title's parentheses are never rewritten).
 * Reference "]:" : the destination runs until whitespace (space or tab) or the
 * end of the line, and all four brackets in it are encoded.
 * The https:// prefix is ASCII case-insensitive, like strings.EqualFold.
 * A code span (a backtick run with a same-length closer on the line) and a
 * top-level fence are copied unchanged, so a "]:" the checker still sees inside
 * inline code is not rewritten.
 */
export function encodeLinkBrackets(body: string): string {
  const lines = body.split('\n')
  let fence = ''
  for (let index = 0; index < lines.length; index++) {
    const line = lines[index]
    if (fence !== '') {
      if (closesFence(line, fence)) fence = ''
      continue
    }
    const opened = opensFence(line)
    if (opened !== null) {
      fence = opened
      continue
    }
    lines[index] = encodeProseLine(line)
  }
  return lines.join('\n')
}

function opensFence(rest: string): string | null {
  if (rest.length === 0) return null
  const marker = rest.charAt(0)
  if (marker !== '`' && marker !== '~') return null
  const run = runLength(rest, 0, marker)
  if (run < 3) return null
  // CommonMark: a backtick fence's info string holds no backtick.
  if (marker === '`' && rest.slice(run).includes('`')) return null
  return rest.slice(0, run)
}

function fenceIndent(line: string): string | null {
  let n = 0
  while (n < line.length && line.charAt(n) === ' ') n++
  if (n > 3) return null
  return line.slice(n)
}

function closesFence(line: string, fence: string): boolean {
  const rest = fenceIndent(line)
  if (rest === null) return false
  const marker = fence.charAt(0)
  const run = runLength(rest, 0, marker)
  if (run < fence.length) return false
  return trimRightChars(rest.slice(run), SPACE_TAB).length === 0
}

function rawViolation(line: string): MarkdownProblem | null {
  for (let i = 0; i + 1 < line.length; i++) {
    if (line.charAt(i) !== ']') continue
    const next = line.charAt(i + 1)
    if (next === ':') {
      if (!safeDestination(trimLeftChars(line.slice(i + 2), SPACE_TAB))) return 'unsafe_link'
    } else if (next === '[') {
      const end = line.indexOf(']', i + 2)
      if (end < 0 || line.slice(i + 2, end).includes('`')) return 'reference_label'
    }
  }
  return null
}

function lineViolation(line: string): MarkdownProblem | null {
  for (let i = 0; i < line.length; i++) {
    switch (line.charAt(i)) {
      case '`': {
        const run = runLength(line, i, '`')
        const end = codeSpanEnd(line, i + run, run)
        if (end < 0) return 'unpaired_backtick'
        i = end - 1
        break
      }
      case '\\': {
        if (i + 1 === line.length || asciiPunct(line.charCodeAt(i + 1))) return 'escape'
        break
      }
      case '<': {
        if (i + 1 < line.length && rawHtmlFollower(line.charCodeAt(i + 1))) return 'raw_html'
        break
      }
      case '!': {
        if (i + 1 < line.length && line.charAt(i + 1) === '[') return 'image'
        break
      }
      case '[': {
        if (i + 1 < line.length && line.charAt(i + 1) === '^') return 'footnote'
        break
      }
      case ']': {
        // A safe "](" is not skipped: the scan keeps walking its destination.
        // A safe destination holds none of the prose sentinels.
        if (i + 1 < line.length && line.charAt(i + 1) === '(' && inlineDestinationUnsafe(line, i)) {
          return 'unsafe_link'
        }
        break
      }
      default:
        break
    }
  }
  return null
}

function inlineDestinationUnsafe(line: string, i: number): boolean {
  const rest = trimLeftChars(line.slice(i + 2), SPACE_TAB)
  const end = indexAny(rest, SPACE_TAB_PAREN)
  return end < 0 || rest.charAt(end) !== ')' || !safeDestination(rest.slice(0, end))
}

function runLength(s: string, i: number, c: string): number {
  let n = 0
  while (i + n < s.length && s.charAt(i + n) === c) n++
  return n
}

function codeSpanEnd(s: string, from: number, n: number): number {
  for (let i = from; i < s.length;) {
    if (s.charAt(i) !== '`') {
      i++
      continue
    }
    const run = runLength(s, i, '`')
    if (run === n) return i + run
    i += run
  }
  return -1
}

function safeDestination(dest: string): boolean {
  if (dest.length <= HTTPS.length) return false
  if (!asciiEqualFold(dest.slice(0, HTTPS.length), HTTPS)) return false
  return !containsAny(dest, UNSAFE_IN_DEST)
}

function asciiEqualFold(s: string, lowerAscii: string): boolean {
  if (s.length !== lowerAscii.length) return false
  for (let i = 0; i < s.length; i++) {
    let code = s.charCodeAt(i)
    if (code >= 0x41 && code <= 0x5a) code |= 0x20
    if (code !== lowerAscii.charCodeAt(i)) return false
  }
  return true
}

function rawHtmlFollower(n: number): boolean {
  return n === 0x2f || n === 0x21 || n === 0x3f || ((n | 0x20) >= 0x61 && (n | 0x20) <= 0x7a)
}

function asciiPunct(b: number): boolean {
  return (b >= 0x21 && b <= 0x2f) || (b >= 0x3a && b <= 0x40) || (b >= 0x5b && b <= 0x60) || (b >= 0x7b && b <= 0x7e)
}

function trimLeftChars(s: string, chars: string): string {
  let i = 0
  while (i < s.length && chars.includes(s.charAt(i))) i++
  return s.slice(i)
}

function trimRightChars(s: string, chars: string): string {
  let i = s.length
  while (i > 0 && chars.includes(s.charAt(i - 1))) i--
  return s.slice(0, i)
}

function containsAny(s: string, chars: string): boolean {
  for (let i = 0; i < s.length; i++) {
    if (chars.includes(s.charAt(i))) return true
  }
  return false
}

function indexAny(s: string, chars: string): number {
  for (let i = 0; i < s.length; i++) {
    if (chars.includes(s.charAt(i))) return i
  }
  return -1
}

function startsWithHttps(s: string): boolean {
  return s.length >= HTTPS.length && asciiEqualFold(s.slice(0, HTTPS.length), HTTPS)
}

function leadingWhitespace(s: string): number {
  let n = 0
  while (n < s.length && (s.charAt(n) === ' ' || s.charAt(n) === '\t')) n++
  return n
}

type Span = { start: number; end: number }

/** Backtick runs paired the way the line scanner pairs them. An unpaired run is not a span. */
function codeSpanRanges(line: string): Span[] {
  const ranges: Span[] = []
  let i = 0
  while (i < line.length) {
    if (line.charAt(i) !== '`') {
      i++
      continue
    }
    const run = runLength(line, i, '`')
    const end = codeSpanEnd(line, i + run, run)
    if (end < 0) {
      i += run
      continue
    }
    ranges.push({ start: i, end })
    i = end
  }
  return ranges
}

function spanCovering(spans: readonly Span[], i: number): Span | null {
  for (const span of spans) {
    if (i >= span.start && i < span.end) return span
  }
  return null
}

function encodeProseLine(line: string): string {
  const spans = codeSpanRanges(line)
  const parts: string[] = []
  let i = 0
  while (i < line.length) {
    const span = spanCovering(spans, i)
    if (span !== null) {
      parts.push(line.slice(i, span.end))
      i = span.end
      continue
    }
    const next = i + 1 < line.length ? line.charAt(i + 1) : ''
    if (line.charAt(i) === ']' && (next === '(' || next === ':')) {
      const taken = takeHttpsDestination(line, i, next, spans)
      if (taken !== null) {
        parts.push(taken.text)
        i = taken.next
        continue
      }
    }
    parts.push(line.charAt(i))
    i++
  }
  return parts.join('')
}

function takeHttpsDestination(
  line: string,
  i: number,
  next: '(' | ':',
  spans: readonly Span[],
): { text: string; next: number } | null {
  const ws = leadingWhitespace(line.slice(i + 2))
  const destStart = i + 2 + ws
  const dest = line.slice(destStart)
  if (!startsWithHttps(dest)) return null
  const destEnd = next === ':' ? referenceDestEnd(line, destStart) : inlineLinkCloser(line, destStart)
  if (destEnd < 0) return null
  return { text: line.slice(i, destStart) + encodeDestSlice(line, destStart, destEnd, spans), next: destEnd }
}

// The ')' that pairs with the '(' of "](". -1 when the line ends with a pair
// still open, or a space or tab comes first: a destination has no whitespace,
// so what follows (a title, say) is not rewritten; the checker refuses it.
function inlineLinkCloser(line: string, destStart: number): number {
  let depth = 0
  for (let j = destStart; j < line.length; j++) {
    const ch = line.charAt(j)
    if (ch === ' ' || ch === '\t') return -1
    if (ch === '(') depth++
    else if (ch === ')') {
      if (depth === 0) return j
      depth--
    }
  }
  return -1
}

function referenceDestEnd(line: string, destStart: number): number {
  const end = indexAny(line.slice(destStart), SPACE_TAB)
  return end < 0 ? line.length : destStart + end
}

function encodeDestSlice(line: string, start: number, end: number, spans: readonly Span[]): string {
  const parts: string[] = []
  let i = start
  while (i < end) {
    const span = spanCovering(spans, i)
    if (span !== null) {
      const stop = Math.min(span.end, end)
      parts.push(line.slice(i, stop))
      i = stop
      continue
    }
    const ch = line.charAt(i)
    if (ch === '[') parts.push('%5B')
    else if (ch === ']') parts.push('%5D')
    else if (ch === '(') parts.push('%28')
    else if (ch === ')') parts.push('%29')
    else parts.push(ch)
    i++
  }
  return parts.join('')
}
