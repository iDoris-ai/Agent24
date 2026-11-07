/** One turn of the conversation: a user message and the reply after it. */
export interface Turn {
  /** Index of the user message in the page's message list. */
  messageIndex: number
  prompt: string
  /** The first assistant message after the prompt; null while none came yet. */
  reply: string | null
}

/** Builds the turn list: one turn per user message. A turn's reply is the
 *  first assistant message before the next user message. */
export function buildTurns(messages: ReadonlyArray<{ role: 'user' | 'assistant'; content: string }>): Turn[] {
  const turns: Turn[] = []
  messages.forEach((m, i) => {
    if (m.role === 'user') turns.push({ messageIndex: i, prompt: m.content, reply: null })
    else if (turns.length > 0 && turns[turns.length - 1].reply === null) turns[turns.length - 1].reply = m.content
  })
  return turns
}

/** The turn being read: the last one whose prompt starts at or above a line
 *  a third of the way down the viewport. `tops` are the prompts' offsets
 *  from the top of the scroll container's content, in turn order. */
export function activeTurn(tops: readonly number[], scrollTop: number, viewportHeight: number): number {
  const line = scrollTop + viewportHeight / 3
  let active = 0
  tops.forEach((top, i) => { if (top <= line) active = i })
  return active
}
