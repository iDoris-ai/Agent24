// F3 WeChat bridge entry point.
//
//   A24_DEV_TOOL=1 agent24 dev bridge-token
//   A24_DEV_TOOL=1 pnpm --filter @agent24/wechat-bridge bridge
//
// First run prints a QR: scan it in WeChat to add the bot. A finished user
// message with text is posted to the daemon. Only `started` is answered with
// 「已记录」; ignored senders and duplicates are not answered.
// Approvals stay on the desktop.

import { login } from './ilink/auth.js'
import { ILinkClient } from './ilink/client.js'
import { Monitor } from './ilink/monitor.js'
import { Sender } from './ilink/sender.js'
import { Agent24Client } from './agent24.js'
import { Bridge } from './bridge.js'
import { discoverDaemon, loadBridgeCursor, type DaemonEndpoint } from './config.js'

async function main(): Promise<void> {
  if (process.env.A24_DEV_TOOL !== '1') {
    throw new Error('微信桥仅限开发工具，产品环境禁止启动；开发时请显式设置 A24_DEV_TOOL=1。入站执行将经 ADR-041a 准入重做。')
  }
  let daemon: DaemonEndpoint
  try {
    daemon = discoverDaemon()
  } catch (err) {
    console.error(`[wechat] ${err instanceof Error ? err.message : err}`)
    process.exit(1)
  }
  console.log(`[wechat] 已连接 agent24d: ${daemon.base}`)

  const { bot_token, baseurl } = await login()
  const client = new ILinkClient(bot_token, baseurl)
  const sender = new Sender(client)
  const bridge = new Bridge(new Agent24Client(daemon), sender)

  const monitor = new Monitor(client, (msg) => bridge.handle(msg), loadBridgeCursor())

  const shutdown = () => {
    console.log('\n[wechat] 停止中...')
    monitor.stop()
    process.exit(0)
  }
  process.on('SIGINT', shutdown)
  process.on('SIGTERM', shutdown)

  console.log('[wechat] ✅ 桥已启动。发来的消息只会被记录，审批在桌面端处理。')
  const reason = await monitor.start()
  if (reason === 'auth') {
    console.error('[wechat] 令牌失效，请重新运行 `agent24 dev bridge-token`')
    process.exit(1)
  }
  if (reason === 'capabilities') {
    console.error('[wechat] 需要 capabilities 模式')
    process.exit(1)
  }
}

void main().catch((err) => {
  console.error('[wechat] 启动失败:', err instanceof Error ? err.message : err)
  process.exit(1)
})
