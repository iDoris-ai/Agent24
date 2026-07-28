// iLink inbound: cursor-based long-poll of /ilink/bot/getupdates. Delivers each
// USER message to the handler; skips the bot's own echoes. Ported from heinu1.

import type { ILinkClient } from './client.js'
import { MessageType, type GetUpdatesResp, type WeixinMessage } from './types.js'
import { BASE_INFO, CONFIG } from '../config.js'

type MessageHandler = (msg: WeixinMessage) => void

export class Monitor {
  private cursor = ''
  private running = false

  constructor(
    private readonly client: ILinkClient,
    private readonly onMessage: MessageHandler,
  ) {}

  start(): void {
    this.running = true
    void this.loop()
  }

  stop(): void {
    this.running = false
  }

  private async loop(): Promise<void> {
    console.log('[wechat] 开始长轮询接收消息...')
    while (this.running) {
      try {
        const res = await this.client.post<GetUpdatesResp>(
          '/ilink/bot/getupdates',
          { get_updates_buf: this.cursor, base_info: BASE_INFO },
          CONFIG.POLL_TIMEOUT_MS + 5_000,
        )
        if (res.get_updates_buf) this.cursor = res.get_updates_buf
        for (const msg of res.msgs ?? []) {
          if (msg.message_type === MessageType.BOT) continue // skip our own echoes
          this.onMessage(msg)
        }
      } catch (err) {
        const m = err instanceof Error ? err.message : String(err)
        if (err instanceof Error && (err.name === 'TimeoutError' || m.includes('timeout'))) {
          continue // long-poll idle timeout — just poll again
        }
        console.error('[wechat] 接收错误:', m, `— ${CONFIG.RECONNECT_DELAY_MS}ms 后重连...`)
        await sleep(CONFIG.RECONNECT_DELAY_MS)
      }
    }
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms))
}
