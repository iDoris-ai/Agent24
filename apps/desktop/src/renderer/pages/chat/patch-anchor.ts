// M1 (c), General/PM 11-21: the same anchor rule the documents module keeps
// (`docx_patch::anchor_matches`), so a patch aimed at the wrong paragraph is
// sent back to the model before the user is asked, not after.

const NUMERAL = /[0-9一二三四五六七八九十百零〇]/

/** A paragraph's words without its numbering: 「第三条 押金…」 → 「押金…」. */
function withoutNumbering(text: string): string {
  let s = [...text].filter((c) => !/\s/.test(c))
  const numerals = (from: number) => { let n = 0; while (from + n < s.length && NUMERAL.test(s[from + n])) n++; return n }
  if (s[0] === '第') {
    const n = numerals(1)
    if (n > 0 && '条章节款编'.includes(s[1 + n] ?? '')) s = s.slice(2 + n)
  }
  if (s[0] === '（' || s[0] === '(') {
    const n = numerals(1)
    if (n > 0 && (s[1 + n] === '）' || s[1 + n] === ')')) s = s.slice(2 + n)
  }
  const n = numerals(0)
  if (n > 0 && '、.．'.includes(s[n] ?? '')) s = s.slice(n + 1)
  return s.join('')
}

const words = (t: string) => [...t].filter((c) => /[\p{L}\p{N}]/u.test(c)).join('')

/** Whether `anchor` opens `paragraph`: spaces and punctuation aside; numbering
 * may be left out but, written, must be the paragraph's; fewer than six words
 * besides numbering (or the whole of a shorter paragraph) is no anchor. */
export function anchorMatches(paragraph: string, anchor: string): boolean {
  const whole = words(paragraph)
  const body = words(withoutNumbering(paragraph))
  const asked = words(anchor)
  const askedBody = words(withoutNumbering(anchor))
  const enough = Math.max(1, Math.min(6, [...body].length))
  return [...askedBody].length >= enough && (whole.startsWith(asked) || body.startsWith(asked))
}

/** The first edit whose anchor does not open the paragraph at its index, with
 * how that paragraph begins; null when every edit's does (or names none). */
export function mismatchedEdit(
  originals: ReadonlyMap<number, string>,
  edits: ReadonlyArray<Record<string, unknown>>,
): { index: number; opening: string } | null {
  for (const e of edits) {
    if (typeof e['index'] !== 'number' || typeof e['anchor'] !== 'string') continue
    const real = originals.get(e['index'])
    if (real === undefined || !anchorMatches(real, e['anchor'])) {
      return { index: e['index'], opening: [...(real ?? '')].slice(0, 24).join('') }
    }
  }
  return null
}
