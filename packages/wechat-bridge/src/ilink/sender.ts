// iLink outbound: send text back to a WeChat user, chunked to the length limit.
// Ported from heinu1 (bot/src/ilink/sender.ts).

import { randomUUID } from 'node:crypto'
import type { ILinkClient } from './client.js'
import { MessageItemType, MessageState, MessageType } from './types.js'
import { BASE_INFO, CONFIG } from '../config.js'

export class Sender {
  constructor(private readonly client: ILinkClient) {}

  async send(toUserId: string, contextToken: string, text: string): Promise<void> {
    const chunks = splitText(text.trim(), CONFIG.MAX_MSG_LEN)
    for (let i = 0; i < chunks.length; i++) {
      await this.client.post('/ilink/bot/sendmessage', {
        msg: {
          from_user_id: '',
          to_user_id: toUserId,
          client_id: randomUUID(),
          message_type: MessageType.BOT,
          message_state: MessageState.FINISH,
          context_token: contextToken,
          item_list: [{ type: MessageItemType.TEXT, text_item: { text: chunks[i] } }],
        },
        base_info: BASE_INFO,
      })
      if (i < chunks.length - 1) await sleep(400)
    }
  }
}

export function splitText(text: string, maxLen: number): string[] {
  if (text.length <= maxLen) return [text]
  const chunks: string[] = []
  for (let i = 0; i < text.length; i += maxLen) chunks.push(text.slice(i, i + maxLen))
  return chunks
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms))
}
