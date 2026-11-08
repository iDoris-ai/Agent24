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
  /** UI-06: the group the user put the conversation in, or null. A group
   *  that no longer exists counts as none. Added within version 1. */
  group_id: string | null
  /** UI-06: when the user archived it (UTC ISO), or null. Added within
   *  version 1. Only `organize` changes these two; a save keeps them. */
  archived_at: string | null
  created_at: string
  updated_at: string
  messages: ChatHistoryMessage[]
}

/** A group's id: only these characters, so it can never be a file name or
 *  collide with a conversation's id. */
export const CHAT_GROUP_ID = /^grp_[a-z0-9_]{1,80}$/

/** UI-06: a group the user made in the history list. */
export interface ChatGroup {
  id: string
  name: string
  created_at: string
  collapsed: boolean
}

export const CHAT_GROUPS_FORMAT = 'agent24.desktop.chat-groups'
export const CHAT_GROUPS_VERSION = 1

/** UI-06: the history list's groups, in the order they were made, and which
 *  parts of the list are folded. Kept beside the conversations in
 *  `chat-history/_groups.json`. */
export interface ChatGroupsState {
  groups: ChatGroup[]
  /** The whole list under the assistant is folded. */
  assistant_collapsed: boolean
  /** The archived conversations are folded (they start folded). */
  archive_collapsed: boolean
}

/** What the page may change about a conversation's place in the list. */
export interface ChatHistoryOrganize {
  group_id?: string | null
  archived?: boolean
}

/** What the history list shows and searches: the record without its
 *  messages, plus the start of its first prompt (UI-04 T3). */
export interface ChatHistorySummary extends Pick<ChatHistoryRecord,
  'id' | 'title' | 'title_source' | 'group_id' | 'archived_at' | 'created_at' | 'updated_at'> {
  /** The first prompt on one line, at most `FIRST_PROMPT_CHARS` characters. */
  first_prompt: string
}

/** Characters of the first prompt a summary carries. */
export const FIRST_PROMPT_CHARS = 200

const graphemeSegmenter = typeof Intl !== 'undefined' && 'Segmenter' in Intl
  ? new Intl.Segmenter(undefined, { granularity: 'grapheme' })
  : null

/** The text's characters, one at a time, read only as far as asked. */
function* segmentsOf(text: string): Generator<string> {
  if (graphemeSegmenter) for (const part of graphemeSegmenter.segment(text)) yield part.segment
  else yield* text
}

/** A conversation's summary, the same in main and in the page. */
export function chatHistorySummary(r: ChatHistoryRecord): ChatHistorySummary {
  // The first prompt on one line, read only as far as its first 200
  // characters: a pasted one can run to megabytes, and main builds a summary
  // of every conversation when it lists them. Runs of white space count as
  // one space; none at the ends.
  const content = r.messages.find(m => m.role === 'user')?.content ?? ''
  const chars: string[] = []
  let space = false
  for (const ch of segmentsOf(content)) {
    if (/^\s+$/.test(ch)) { space = chars.length > 0; continue }
    if (space) {
      chars.push(' ')
      space = false
      if (chars.length === FIRST_PROMPT_CHARS) break
    }
    chars.push(ch)
    if (chars.length === FIRST_PROMPT_CHARS) break
  }
  return {
    id: r.id, title: r.title, title_source: r.title_source, group_id: r.group_id, archived_at: r.archived_at,
    created_at: r.created_at, updated_at: r.updated_at, first_prompt: chars.join(''),
  }
}
