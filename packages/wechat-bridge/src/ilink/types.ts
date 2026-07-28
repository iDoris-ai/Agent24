// WeChat iLink Bot protocol types. Enum values are ground truth (ported from
// heinu1 / weixin-bot-ilink) — do not reorder.

export enum MessageType {
  USER = 1,
  BOT = 2,
}

export enum MessageState {
  NEW = 0,
  GENERATING = 1,
  FINISH = 2,
}

export enum MessageItemType {
  TEXT = 1,
  IMAGE = 2,
  VOICE = 3,
  FILE = 4,
  VIDEO = 5,
}

export interface TextItem {
  text: string
}

export interface MessageItem {
  type: MessageItemType
  text_item?: TextItem
  // Non-text items (image/voice/file/video) carry CDN media; the MVP bridge
  // only reads text, so their payloads are intentionally left opaque here.
  [k: string]: unknown
}

export interface WeixinMessage {
  message_id: number
  from_user_id: string
  to_user_id: string
  client_id: string
  create_time_ms: number
  message_type: MessageType
  message_state: MessageState
  context_token: string
  item_list: MessageItem[]
}

export interface GetUpdatesResp {
  ret?: number
  errmsg?: string
  msgs: WeixinMessage[]
  get_updates_buf: string
  longpolling_timeout_ms?: number
}

export interface TokenData {
  bot_token: string
  baseurl: string
  saved_at: number
}

/** Concatenate a message's text items into one string (ignoring non-text). */
export function messageText(msg: WeixinMessage): string {
  return (msg.item_list ?? [])
    .filter((i) => i.type === MessageItemType.TEXT && i.text_item?.text)
    .map((i) => i.text_item!.text)
    .join('\n')
    .trim()
}
