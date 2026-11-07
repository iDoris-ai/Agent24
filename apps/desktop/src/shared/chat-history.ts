// UI-02: the main chat's history, kept by the desktop app on this machine.
//
// A stopgap until the main chat runs through daemon sessions: the file says
// what it is (`format`, `version`) and stamps every message with a time, and a
// reply with the run that answered it when the page knew one, so a later
// version can import these conversations into sessions or drop them on
// purpose rather than guess.

export const CHAT_HISTORY_FORMAT = 'agent24.desktop.chat-history'
export const CHAT_HISTORY_VERSION = 1

/** The chat's `session_id` (the daemon's memory key for the conversation)
 *  doubles as the record id. Only these characters, so an id is always a
 *  plain file name. */
export const CHAT_HISTORY_ID = /^chat_[a-z0-9_]{1,80}$/

export interface ChatHistoryLatency {
  modelName: string | null
  tier: string | null
  firstTokenMs: number | null
  totalMs: number
}

export type ChatHistoryMessage =
  | {
    role: 'user'
    content: string
    /** ISO 8601, when the message was sent. */
    at: string
    /** False when the turn never reached the model (failed or cancelled);
     *  such a prompt is not sent again as context after a restore. */
    delivered: boolean
  }
  | {
    role: 'assistant'
    content: string
    at: string
    /** The run that produced the reply, when the page knew it; null otherwise. */
    run_id: string | null
    latency: ChatHistoryLatency | null
  }

/** Where a conversation's title came from, strongest first: the user's
 *  rename, a title the model wrote, or the first prompt cut short. A weaker
 *  title never replaces a stronger one (main/chat-history.ts). Added within
 *  version 1: a file without it has the first prompt's title. */
export type ChatHistoryTitleSource = 'user' | 'model' | 'prompt'
export const CHAT_HISTORY_TITLE_SOURCES: readonly ChatHistoryTitleSource[] = ['user', 'model', 'prompt']

export interface ChatHistoryRecord {
  format: typeof CHAT_HISTORY_FORMAT
  version: typeof CHAT_HISTORY_VERSION
  id: string
  title: string
  title_source: ChatHistoryTitleSource
  created_at: string
  updated_at: string
  messages: ChatHistoryMessage[]
}

/** What the history list shows and searches: the record without its
 *  messages, plus the start of its first prompt (UI-04 T3). */
export interface ChatHistorySummary extends Pick<ChatHistoryRecord, 'id' | 'title' | 'title_source' | 'created_at' | 'updated_at'> {
  /** The first prompt on one line, at most `FIRST_PROMPT_CHARS` characters. */
  first_prompt: string
}

/** Characters of the first prompt a summary carries. */
export const FIRST_PROMPT_CHARS = 200

const graphemeSegmenter = typeof Intl !== 'undefined' && 'Segmenter' in Intl
  ? new Intl.Segmenter(undefined, { granularity: 'grapheme' })
  : null

/** A conversation's summary, the same in main and in the page. */
export function chatHistorySummary(r: ChatHistoryRecord): ChatHistorySummary {
  // Only the start of a prompt is read: a pasted one can run to megabytes,
  // and main builds a summary of every conversation when it lists them.
  const content = r.messages.find(m => m.role === 'user')?.content ?? ''
  const first = content.slice(0, FIRST_PROMPT_CHARS * 32).replace(/\s+/g, ' ').trim()
  const chars: string[] = []
  const parts = graphemeSegmenter?.segment(first) ?? null
  if (parts) {
    for (const part of parts) {
      if (chars.length === FIRST_PROMPT_CHARS) break
      chars.push(part.segment)
    }
  } else {
    for (const ch of first) {
      if (chars.length === FIRST_PROMPT_CHARS) break
      chars.push(ch)
    }
  }
  return {
    id: r.id, title: r.title, title_source: r.title_source, created_at: r.created_at, updated_at: r.updated_at,
    first_prompt: chars.join(''),
  }
}
