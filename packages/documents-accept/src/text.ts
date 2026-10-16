// The two text forms gold labels are compared in (samples README §4.3,
// acceptance README §4.1). Both sides of a comparison get the same one.

// CJK as the samples spec lists it: ideographs with their extensions and
// compatibility forms, CJK punctuation, kana. Hangul is left out on purpose:
// Korean separates words with spaces.
const CJK = '[\\u3000-\\u303F\\u3040-\\u30FF\\u3400-\\u4DBF\\u4E00-\\u9FFF\\uF900-\\uFAFF\\u{20000}-\\u{3FFFF}]'
const SPACE_BY_CJK = new RegExp(`(?<=${CJK}) | (?=${CJK})`, 'gu')

/** L1: NFKC, with each run of whitespace (line breaks too) as one space. */
export function l1(text: string): string {
  return text.normalize('NFKC').replace(/\s+/gu, ' ').trim()
}

/** L2: L1 without the spaces that have a CJK character on either side. */
export function l2(text: string): string {
  return l1(text).replace(SPACE_BY_CJK, '')
}
