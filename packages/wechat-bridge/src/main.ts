// F3 WeChat bridge entry point.
//
//   agent24 daemon start            # legacy daemon, A24_DEV_TOOL=1, and
//                                   # A24_INBOUND_DEV_ALLOWLIST=wechat:<uid>,...
//   A24_DEV_TOOL=1 pnpm --filter @agent24/wechat-bridge bridge
//
// First run prints a QR: scan it in WeChat to add the bot. A finished user
// message with text is posted to the daemon. `started` and `duplicate` are
// answered with 「已记录」. An ignored sender is not answered.
// Approvals stay on the desktop.

import { login } from './ilink/auth.js'
import { ILinkClient } from './ilink/client.js'
import { Monitor } from './ilink/monitor.js'
import { Sender } from './ilink/sender.js'
import { Agent24Client } from './agent24.js'
import { Bridge } from './bridge.js'
import { discoverDaemon } from './config.js'

async function main(): Promise<void> {
  if (process.env.A24_DEV_TOOL !== '1') {
    throw new Error('微信桥仅限开发工具，产品环境禁止启动；开发时请显式设置 A24_DEV_TOOL=1。入站执行将经 ADR-041a 准入重做。')
  }
  let daemon: ReturnType<typeof discoverDaemon>
  try {
    daemon = discoverDaemon()
  } catch (err) {
    // daemon.json exists but yields no bearer (capabilities mode / missing token /
    // invalid record): refuse to start rather than send unauthenticated requests.
    console.error(`[wechat] agent24d 状态文件不能用于桥接:${err instanceof Error ? err.message : err}`)
    console.error('[wechat] 桥接需要 legacy 模式的 daemon(状态文件带 token),或显式设置 A24_BASE_URL / A24_TOKEN。')
    process.exit(1)
  }
  if (!daemon) {
    console.error(
      '[wechat] 找不到运行中的 agent24d。先启动它（agent24 daemon start），' +
        '或设置 A24_BASE_URL / A24_TOKEN。',
    )
    process.exit(1)
  }
  console.log(`[wechat] 已连接 agent24d: ${daemon.base}`)

  const { bot_token, baseurl } = await login()
  const client = new ILinkClient(bot_token, baseurl)
  const sender = new Sender(client)
  const bridge = new Bridge(new Agent24Client(daemon), sender)

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
  console.log('[wechat] ✅ 桥已启动。发来的消息只会被记录，审批在桌面端处理。')
}

void main().catch((err) => {
  console.error('[wechat] 启动失败:', err instanceof Error ? err.message : err)
  process.exit(1)
})
