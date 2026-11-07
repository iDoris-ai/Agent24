// UI-04: conversation titles. Lengths count what a reader sees as one
// character (a grapheme: an emoji with its modifiers, a letter with its
// accents), so a cut never splits one.

/** Characters of the first prompt used as a conversation's title. */
export const PROMPT_TITLE_CHARS = 60
/** Most characters a renamed title keeps. */
export const RENAME_TITLE_CHARS = 80

const segmenter = typeof Intl !== 'undefined' && 'Segmenter' in Intl
  ? new Intl.Segmenter(undefined, { granularity: 'grapheme' })
  : null

export function graphemes(text: string): string[] {
  return segmenter ? Array.from(segmenter.segment(text), s => s.segment) : Array.from(text)
}

/** The text on one line: runs of white space as one space, none at the ends. */
export const oneLine = (text: string): string => text.replace(/\s+/g, ' ').trim()

/** The first prompt as a title: one line, cut to `PROMPT_TITLE_CHARS`
 *  characters with an ellipsis when longer. */
export function promptTitle(prompt: string): string {
  const chars = graphemes(oneLine(prompt))
  return chars.length > PROMPT_TITLE_CHARS ? `${chars.slice(0, PROMPT_TITLE_CHARS).join('')}…` : chars.join('')
}

/** What the user typed as a new title: one line, at most
 *  `RENAME_TITLE_CHARS` characters; empty when there is nothing to keep. */
export function renameTitle(input: string): string {
  return graphemes(oneLine(input)).slice(0, RENAME_TITLE_CHARS).join('').trim()
}
