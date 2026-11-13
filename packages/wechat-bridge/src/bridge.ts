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
 * The cursor stays put and the bridge backs off. Network errors, timeouts,
 * 5xx, and 429 hold with backoff and are never given up on a count: a down
 * daemon must not drop the message. One of those holds clears the consecutive
 * count. 408 and any other unlisted 4xx stop holding after {@link HOLD_GIVE_UP}
 * consecutive holds of that kind: the bridge logs, drops the hold entry, and
 * advances so one rejected event cannot block the rest. A permanent failure
 * drops the entry too.
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

/** Count this hold toward {@link HOLD_GIVE_UP}.
 * Network errors, timeouts, 429, and 5xx do not: those keep holding. */
export function holdCountsTowardGiveUp(err: unknown): boolean {
  if (!(err instanceof InboundHttpError)) return false
  if (err.status === 429 || err.status >= 500) return false
  return err.status >= 400 && err.status < 500
}

/** Oldest ids are dropped past this, so one long run cannot grow without bound. */
export const FAILURE_REPLY_LIMIT = 1024

/** Consecutive holds of one message before the cursor moves past it. */
export const HOLD_GIVE_UP = 20

/** `Date` range. Outside it, `toISOString` throws `RangeError`. */
const MAX_DATE_MS = 8.64e15

export class Bridge {
  /** `记录失败` is sent at most once per WeChat message id, up to {@link FAILURE_REPLY_LIMIT}. */
  private readonly failureReplied = new Set<string>()
  /** Consecutive counting holds for one WeChat message id.
   * Success, a non-counting hold, give-up, or a permanent failure removes it. */
  private readonly holds = new Map<string, number>()

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
    // `String(undefined)` is one shared key. A message with no id cannot be
    // retried on its own, so skip it instead of posting `wechat:undefined`.
    if (typeof msg.message_id !== 'number' || !Number.isFinite(msg.message_id)) {
      console.error('[wechat] 消息缺少 message_id，跳过')
      return 'advance'
    }
    const user = msg.from_user_id
    const ctx = msg.context_token
    const at = inboundStamp(msg.create_time_ms)
    if (at === null) {
      console.error(`[wechat] 消息 ${msg.message_id} 的 create_time_ms 不合法，跳过`)
      return 'advance'
    }
    const key = String(msg.message_id)
    try {
      const recorded = await this.agent.recordInbound({
        id: `wechat:${msg.message_id}`,
        actor: user,
        at,
        text,
      })
      this.holds.delete(key)
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
      if (step === 'hold' && holdCountsTowardGiveUp(err)) {
        const n = (this.holds.get(key) ?? 0) + 1
        if (n > HOLD_GIVE_UP) {
          this.holds.delete(key)
          console.error(`[wechat] 消息 ${key} 连续失败 ${n} 次，放弃并前进`)
          return 'advance'
        }
        this.holds.set(key, n)
      } else if (step === 'hold' || step === 'advance') {
        this.holds.delete(key)
      }
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

/** `YYYY-MM-DDTHH:MM:SSZ`. The daemon rejects fractional seconds.
 * `ms` must already be inside the Date range; {@link inboundStamp} checks. */
export function canonicalStamp(ms: number): string {
  return new Date(ms).toISOString().replace(/\.\d{3}Z$/, 'Z')
}

/** Stamp for an inbound POST, or `null` when `ms` is missing, not finite,
 * or outside the range `Date` can format. Checked before the POST so a
 * `RangeError` cannot hold the cursor. */
export function inboundStamp(ms: unknown): string | null {
  if (typeof ms !== 'number' || !Number.isFinite(ms) || Math.abs(ms) > MAX_DATE_MS) return null
  return canonicalStamp(ms)
}
