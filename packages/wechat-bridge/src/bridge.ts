// Transport: a finished user message that has text is posted to the daemon.
// The chat is acknowledged only when that post started a run; a `duplicate`
// stays silent because it may replay an off-list message. Admission, runs, and approvals
// stay on the daemon and the desktop. "y" and "n" are ordinary text.

import { InboundHttpError, type Agent24Client, type InboundRecord } from './agent24.js'
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

/** `advance` — the server accepted the event, the message was not posted,
 * or the failure is permanent (only 400/403/413/422).
 * `hold` — network error, timeout, 5xx, 429, 408, or any other unlisted 4xx.
 * The cursor stays put and the bridge backs off.
 * `stop` — the bridge token was rejected (401).
 * `dev_tool` — 404: the daemon is not serving the dev inbound route. Stop.
 * `capabilities` — legacy mode answered 409 `capability_mode_required`. */
export type CursorStep = 'advance' | 'hold' | 'stop' | 'dev_tool' | 'capabilities'

const PERMANENT_STATUS = new Set([400, 403, 413, 422])

export const DEV_ROUTE_MISSING = 'daemon 未挂载开发入站路由（需 A24_DEV_TOOL=1）'

/** Classify a failed `recordInbound`. Network errors and timeouts hold. */
export function classifyInboundFailure(err: unknown): CursorStep {
  if (err instanceof InboundHttpError) {
    if (err.status === 401) return 'stop'
    if (err.status === 404) return 'dev_tool'
    if (err.status === 409 && err.code === 'capability_mode_required') return 'capabilities'
    if (err.status === 429 || err.status >= 500) return 'hold'
    if (PERMANENT_STATUS.has(err.status)) return 'advance'
    if (err.status >= 400 && err.status < 500) return 'hold'
  }
  return 'hold'
}

/** Oldest ids are dropped past this, so one long run cannot grow without bound. */
export const FAILURE_REPLY_LIMIT = 1024

export class Bridge {
  /** `记录失败` is sent at most once per WeChat message id, up to {@link FAILURE_REPLY_LIMIT}. */
  private readonly failureReplied = new Set<string>()

  constructor(
    private readonly agent: Agent24Client,
    private readonly sender: Sender,
  ) {}

  /** Drop unfinished, non-user, and textless messages, then record the rest.
   * A textless finish still returns `advance`: nothing was posted, and leaving
   * the cursor there would deliver the same message forever. */
  async handle(msg: WeixinMessage): Promise<CursorStep> {
    if (msg.message_type !== MessageType.USER || msg.message_state !== MessageState.FINISH) {
      return 'advance'
    }
    const text = messageText(msg)
    if (!text) return 'advance'
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
      return 'advance'
    } catch (err) {
      const step = classifyInboundFailure(err)
      if (step === 'dev_tool') {
        console.error(`[wechat] ${DEV_ROUTE_MISSING}`)
        return step
      }
      if (step === 'stop' || step === 'capabilities') return step
      console.error('[wechat] 记录消息出错:', err instanceof Error ? err.message : err)
      await this.failReplyOnce(msg, user, ctx)
      return step
    }
  }

  private async failReplyOnce(msg: WeixinMessage, user: string, ctx: string): Promise<void> {
    const key = String(msg.message_id)
    if (this.failureReplied.has(key)) return
    this.failureReplied.add(key)
    while (this.failureReplied.size > FAILURE_REPLY_LIMIT) {
      const oldest = this.failureReplied.values().next().value
      if (oldest === undefined) break
      this.failureReplied.delete(oldest)
    }
    await this.reply(user, ctx, RECORD_FAILED_REPLY)
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
