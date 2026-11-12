// iLink inbound: cursor-based long-poll of /ilink/bot/getupdates. Delivers each
// USER message to the handler; skips the bot's own echoes. Ported from heinu1.

import type { ILinkClient } from './client.js'
import { MessageState, MessageType, type GetUpdatesResp, type WeixinMessage } from './types.js'
import { BASE_INFO, CONFIG, saveBridgeCursor } from '../config.js'
import type { CursorStep } from '../bridge.js'

/** Upper bound for the hold backoff. The first wait is `RECONNECT_DELAY_MS`. */
export const HOLD_BACKOFF_CAP_MS = 60_000

export function nextHoldDelay(current: number, cap = HOLD_BACKOFF_CAP_MS): number {
  if (!Number.isFinite(current) || current <= 0) return cap
  return Math.min(current * 2, cap)
}

type MessageHandler = (msg: WeixinMessage) => Promise<CursorStep>

export class Monitor {
  private cursor: string
  private holdDelay = CONFIG.RECONNECT_DELAY_MS
  private running = false

  constructor(
    private readonly client: ILinkClient,
    private readonly onMessage: MessageHandler,
    cursor = '',
  ) {
    this.cursor = cursor
  }

  /** Runs until `stop`, or until a handler returns `stop` / `capabilities`. */
  start(): Promise<'auth' | 'capabilities' | 'stopped'> {
    this.running = true
    return this.loop()
  }

  stop(): void {
    this.running = false
  }

  private async loop(): Promise<'auth' | 'capabilities' | 'stopped'> {
    console.log('[wechat] 开始长轮询接收消息...')
    while (this.running) {
      try {
        const res = await this.client.post<GetUpdatesResp>(
          '/ilink/bot/getupdates',
          { get_updates_buf: this.cursor, base_info: BASE_INFO },
          CONFIG.POLL_TIMEOUT_MS + 5_000,
        )
        // The cursor moves only after every finished message in this batch is
        // settled. A hold or a stop keeps the last committed `get_updates_buf`.
        const next = res.get_updates_buf
        let hold = false
        for (const msg of res.msgs ?? []) {
          if (msg.message_type === MessageType.BOT) continue // skip our own echoes
          // Only act on finished messages — ignore NEW/GENERATING updates so a
          // still-composing/streaming message can't trigger a run prematurely.
          if (msg.message_state !== MessageState.FINISH) continue
          const step = await this.onMessage(msg)
          if (step === 'stop') {
            this.running = false
            return 'auth'
          }
          if (step === 'capabilities') {
            this.running = false
            return 'capabilities'
          }
          if (step === 'hold') {
            // Do not post the rest of this batch. The next poll redelivers it.
            hold = true
            break
          }
        }
        if (hold) {
          await this.backoff('记录失败')
          continue
        }
        this.holdDelay = CONFIG.RECONNECT_DELAY_MS
        if (next) {
          this.cursor = next
          try {
            saveBridgeCursor(next)
          } catch (err) {
            console.error('[wechat] 游标没有写入:', err instanceof Error ? err.message : err)
          }
        }
      } catch (err) {
        const m = err instanceof Error ? err.message : String(err)
        if (err instanceof Error && (err.name === 'TimeoutError' || m.includes('timeout'))) {
          continue // long-poll idle timeout — just poll again
        }
        console.error('[wechat] 接收错误:', m)
        await this.backoff('接收错误')
      }
    }
    return 'stopped'
  }

  private async backoff(why: string): Promise<void> {
    const wait = this.holdDelay
    this.holdDelay = nextHoldDelay(this.holdDelay)
    console.error(`[wechat] ${why} — ${wait}ms 后重试`)
    await sleep(wait)
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms))
}
