// F3 WeChat bridge entry point.
//
//   agent24 daemon start            # the daemon this bridge drives
//   pnpm --filter @agent24/wechat-bridge bridge
//
// First run prints a QR: scan it in WeChat to add the bot. After that, messages
// you send the bot become agent24d runs, and replies (and approval requests)
// come back in the chat.

import fs from 'node:fs'
import path from 'node:path'
import { login } from './ilink/auth.js'
import { ILinkClient } from './ilink/client.js'
import { Monitor } from './ilink/monitor.js'
import { Sender } from './ilink/sender.js'
import { Agent24Client } from './agent24.js'
import { Bridge, type SessionStore } from './bridge.js'
import { CONFIG, discoverDaemon } from './config.js'

class FileSessionStore implements SessionStore {
  private readonly file = path.join(path.dirname(CONFIG.TOKEN_FILE), 'wechat-sessions.json')

  load(): Map<string, string> {
    try {
      const raw = JSON.parse(fs.readFileSync(this.file, 'utf8')) as Record<string, string>
      return new Map(Object.entries(raw))
    } catch {
      return new Map()
    }
  }

  save(map: Map<string, string>): void {
    try {
      fs.mkdirSync(path.dirname(this.file), { recursive: true, mode: 0o700 })
      fs.writeFileSync(this.file, JSON.stringify(Object.fromEntries(map), null, 2))
    } catch (err) {
      console.error('[wechat] 保存会话映射失败:', err instanceof Error ? err.message : err)
    }
  }
}

async function main(): Promise<void> {
  const daemon = discoverDaemon()
  if (!daemon) {
    console.error(
      '[wechat] 找不到运行中的 agent24d。先启动它（agent24 daemon start），' +
        '或设置 A24_BASE_URL / A24_TOKEN。',
    )
    process.exit(1)
  }
  console.log(`[wechat] 已连接 agent24d: ${daemon.base}`)

  if (CONFIG.ALLOWED_UIDS.size === 0) {
    console.warn(
      '[wechat] ⚠️  未配置 A24_WECHAT_ALLOWED_UIDS：将拒绝所有消息（fail-closed）。' +
        '给 bot 发一条消息，日志会打印你的 from_user_id，再把它设进环境变量以授权。',
    )
  } else {
    console.log(`[wechat] 已授权 ${CONFIG.ALLOWED_UIDS.size} 个用户`)
  }

  const { bot_token, baseurl } = await login()
  const client = new ILinkClient(bot_token, baseurl)
  const sender = new Sender(client)
  const bridge = new Bridge(new Agent24Client(daemon), sender, new FileSessionStore(), CONFIG.ALLOWED_UIDS)

  const monitor = new Monitor(client, (msg) => {
    void bridge.handle(msg).catch((err) =>
      console.error('[wechat] 处理消息出错:', err instanceof Error ? err.message : err),
    )
  })

  const shutdown = () => {
    console.log('\n[wechat] 停止中...')
    monitor.stop()
    process.exit(0)
  }
  process.on('SIGINT', shutdown)
  process.on('SIGTERM', shutdown)

  monitor.start()
  console.log('[wechat] ✅ 桥已启动，在微信里给 agent24 发消息即可。')
}

void main().catch((err) => {
  console.error('[wechat] 启动失败:', err instanceof Error ? err.message : err)
  process.exit(1)
})
