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

export type ChatHistorySummary = Pick<ChatHistoryRecord, 'id' | 'title' | 'title_source' | 'created_at' | 'updated_at'>
