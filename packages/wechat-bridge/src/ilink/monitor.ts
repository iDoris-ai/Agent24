// iLink inbound: cursor-based long-poll of /ilink/bot/getupdates. Delivers each
// USER message to the handler; skips the bot's own echoes. Ported from heinu1.

import type { ILinkClient } from './client.js'
import { MessageState, MessageType, type GetUpdatesResp, type WeixinMessage } from './types.js'
import { BASE_INFO, CONFIG } from '../config.js'
import type { CursorStep } from '../bridge.js'

type MessageHandler = (msg: WeixinMessage) => Promise<CursorStep>

export class Monitor {
  private cursor = ''
  private running = false

  constructor(
    private readonly client: ILinkClient,
    private readonly onMessage: MessageHandler,
  ) {}

  /** Runs until `stop`, or until a handler returns `stop` (`auth`). */
  start(): Promise<'auth' | 'stopped'> {
    this.running = true
    return this.loop()
  }

  stop(): void {
    this.running = false
  }

  private async loop(): Promise<'auth' | 'stopped'> {
    console.log('[wechat] 开始长轮询接收消息...')
    while (this.running) {
      try {
        const res = await this.client.post<GetUpdatesResp>(
          '/ilink/bot/getupdates',
          { get_updates_buf: this.cursor, base_info: BASE_INFO },
          CONFIG.POLL_TIMEOUT_MS + 5_000,
        )
        // The cursor moves only after every finished message in this batch is
        // settled. A failed POST keeps `get_updates_buf` so the next poll
        // redelivers; the daemon dedupes `wechat:<message_id>`.
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
          if (step !== 'advance') hold = true
        }
        if (hold) {
          await sleep(CONFIG.RECONNECT_DELAY_MS)
          continue
        }
        if (next) this.cursor = next
      } catch (err) {
        const m = err instanceof Error ? err.message : String(err)
        if (err instanceof Error && (err.name === 'TimeoutError' || m.includes('timeout'))) {
          continue // long-poll idle timeout — just poll again
        }
        console.error('[wechat] 接收错误:', m, `— ${CONFIG.RECONNECT_DELAY_MS}ms 后重连...`)
        await sleep(CONFIG.RECONNECT_DELAY_MS)
      }
    }
    return 'stopped'
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms))
}
