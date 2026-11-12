// Transport: a finished user message that has text is posted to the daemon.
// The chat is acknowledged only when that post started a run; a `duplicate`
// stays silent because it may replay an off-list message. Admission, runs, and approvals
// stay on the daemon and the desktop. "y" and "n" are ordinary text.

import type { Agent24Client, InboundRecord } from './agent24.js'
import type { Sender } from './ilink/sender.js'
import { messageText, MessageState, MessageType, type WeixinMessage } from './ilink/types.js'

export const RECORDED_REPLY = '已记录'
export const RECORD_FAILED_REPLY = '记录失败，请稍后重试'

/** `ignored` is recorded and not answered (off the list, stale, rate limited).
 * `already_linked` is not this route's replay — a second POST of the same id
 * is `duplicate` — so it does not get a second ack either. */
// Only a fresh run gets the reply. A duplicate may be a replay of an off-list
// message, and the bridge cannot see the original disposition, so it stays
// silent; the first delivery already answered when it started a run.
function shouldAck(outcome: InboundRecord['outcome']): boolean {
  return outcome === 'started'
}

export class Bridge {
  constructor(
    private readonly agent: Agent24Client,
    private readonly sender: Sender,
  ) {}

  /** Drop unfinished, non-user, and textless messages, then record the rest. */
  async handle(msg: WeixinMessage): Promise<void> {
    if (msg.message_type !== MessageType.USER || msg.message_state !== MessageState.FINISH) {
      return
    }
    const text = messageText(msg)
    if (!text) return
    const user = msg.from_user_id
    const ctx = msg.context_token
    try {
      const recorded = await this.agent.recordInbound({
        id: `wechat:${msg.message_id}`,
        actor: user,
        at: canonicalStamp(msg.create_time_ms),
        text,
      })
      if (shouldAck(recorded.outcome)) await this.reply(user, ctx, RECORDED_REPLY)
    } catch (err) {
      console.error('[wechat] 记录消息出错:', err instanceof Error ? err.message : err)
      await this.reply(user, ctx, RECORD_FAILED_REPLY)
    }
  }

  private async reply(user: string, ctx: string, text: string): Promise<void> {
    try {
      await this.sender.send(user, ctx, text)
    } catch (err) {
      console.error('[wechat] 发送失败:', err instanceof Error ? err.message : err)
    }
  }
}

/** `YYYY-MM-DDTHH:MM:SSZ`. The daemon rejects fractional seconds. */
export function canonicalStamp(ms: number): string {
  return new Date(ms).toISOString().replace(/\.\d{3}Z$/, 'Z')
}
