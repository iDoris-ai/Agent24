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

// --- Titles written by the model (UI-04 T2) ---------------------------------

/** Most characters a title written by the model keeps (about 12 Chinese
 *  characters or 6 English words, as the request asks, with some slack). */
export const MODEL_TITLE_CHARS = 24
/** A prompt shorter than this ("你好", "hi") says too little to title the
 *  conversation by: the title waits for the next turn. */
export const SHORT_PROMPT_CHARS = 4
/** After this many answered turns the title is asked for however short the
 *  prompts were. */
export const LAST_TITLE_TURN = 3
/** Characters of the conversation sent to the model, at most. */
const EXCERPT_CHARS = 1000

export interface AnsweredTurn { prompt: string; reply: string }
type PageMessage = { role: 'user'; content: string; delivery: { kind: string } } | { role: 'assistant'; content: string }

/** The turns that reached the model and got its reply, in order. A failed
 *  turn (not sent, or answered with an error) is left out. */
export function answeredTurns(messages: readonly PageMessage[]): AnsweredTurn[] {
  const turns: AnsweredTurn[] = []
  messages.forEach((m, i) => {
    const next = messages[i + 1]
    if (m.role === 'user' && m.delivery.kind === 'sent' && next?.role === 'assistant') {
      turns.push({ prompt: m.content, reply: next.content })
    }
  })
  return turns
}

/** Whether to ask for the title now: after the first answered turn, unless
 *  its prompt is too short, and by the third at the latest. */
export function titleDue(turns: readonly AnsweredTurn[]): boolean {
  const last = turns[turns.length - 1]
  if (!last) return false
  return turns.length >= LAST_TITLE_TURN || graphemes(oneLine(last.prompt)).length >= SHORT_PROMPT_CHARS
}

const clip = (text: string, chars: number): string => {
  const g = graphemes(oneLine(text))
  return g.length > chars ? `${g.slice(0, chars).join('')}…` : g.join('')
}

const TITLE_INSTRUCTION = [
  'Write a short title for the conversation below.',
  'Write it in the language the user wrote in (a Chinese conversation gets a Chinese title).',
  'At most 12 Chinese characters, or at most 6 words in other languages.',
  'Reply with the title only: no quotes, no final punctuation, no explanation.',
].join(' ')

/** The `/api/v1/chat` body that asks for a title. No `session_id`: the
 *  daemon neither recalls nor writes memory for it, and routes it to a
 *  local model only (ADR-K1-02 §2.4 (a)). Only the turns' text goes, cut
 *  short; no Knowledge sources or receipts. */
export function titleRequest(turns: readonly AnsweredTurn[]): { messages: Array<{ role: 'system' | 'user'; content: string }> } {
  const share = Math.floor(EXCERPT_CHARS / (2 * Math.max(1, turns.length)))
  const conversation = turns
    .map(turn => `User: ${clip(turn.prompt, share)}\nAssistant: ${clip(turn.reply, share)}`)
    .join('\n\n')
  return { messages: [{ role: 'system', content: TITLE_INSTRUCTION }, { role: 'user', content: conversation }] }
}

const EDGE = '\\s"\'“”‘’「」『』《》【】*_`#'
const LEADING = new RegExp(`^[${EDGE}]+`)
const TRAILING = new RegExp(`[${EDGE}。．.！!？?，,；;：:、…]+$`)

/** The model's answer as a title: its thinking dropped, the first line,
 *  without a "Title:" label, quotes, markdown or final punctuation, cut to
 *  `MODEL_TITLE_CHARS`. Empty when nothing usable is left. */
export function cleanModelTitle(answer: string): string {
  const text = answer.replace(/<think>[\s\S]*?<\/think>/gi, '')
  // Thinking that never ended: the answer was cut off before the title.
  if (/<think>/i.test(text)) return ''
  // Code fence lines (```text) are wrapping, not the title.
  const line = text.split(/\r?\n/).map(l => l.trim()).find(l => l.length > 0 && !l.startsWith('```')) ?? ''
  // Markdown and quotes first, so a label inside them (**Title:**) goes too.
  const bare = line
    .replace(LEADING, '')
    .replace(/^(title|标题|標題)\s*[*_]*\s*[:：]\s*[*_]*/i, '')
    .replace(LEADING, '')
    .replace(TRAILING, '')
  return graphemes(oneLine(bare)).slice(0, MODEL_TITLE_CHARS).join('').trim()
}

type Proxy = (req: { method: 'POST'; path: string; body: unknown }) => Promise<{ ok: boolean; data?: unknown }>

/** Asks the daemon's model for a title once. Null on any failure (no model,
 *  busy, a headroom confirmation, which is never given for a title) or an
 *  answer with nothing usable: the conversation keeps its prompt's title. */
export async function generateTitle(proxy: Proxy, turns: readonly AnsweredTurn[]): Promise<string | null> {
  const res = await proxy({ method: 'POST', path: '/api/v1/chat', body: titleRequest(turns) }).catch(() => null)
  if (!res?.ok) return null
  const content = (res.data as { message?: { content?: unknown } } | null)?.message?.content
  return typeof content === 'string' ? cleanModelTitle(content) || null : null
}
