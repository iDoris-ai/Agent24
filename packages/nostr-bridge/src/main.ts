// F4 Nostr bridge entry point — the runnable that 联调 / production uses.
//
//   agent24 daemon start                       # the daemon this bridge drives
//   agent-speaker daemon --identity agent24 &   # pulls inbound into messages.db
//   pnpm --filter @agent24/nostr-bridge bridge  # this
//
// On start it default-registers the agent's business capabilities, then polls
// the inbox and turns each authorized peer message into a GATED agent24d run,
// replying with the result over Nostr. Outbound drives the agent-speaker BINARY
// (build result) per command; inbound reads what agent-speaker's daemon pulled.

import fs from 'node:fs'
import { cliRunner, SpeakerClient } from './speaker.js'
import { Agent24Client } from './agent24.js'
import { NostrBridge } from './bridge.js'
import { InboundBridge, pollOnce } from './inbound.js'
import { loadAgent24Profile } from './profile.js'
import { CONFIG, discoverDaemon } from './config.js'

async function main(): Promise<void> {
  const daemon = discoverDaemon()
  if (!daemon) {
    console.error(
      '[nostr] 找不到运行中的 agent24d。先启动它(agent24 daemon start),或设置 A24_BASE_URL / A24_TOKEN。',
    )
    process.exit(1)
  }
  console.log(`[nostr] 已连接 agent24d: ${daemon.base}`)

  const speaker = new SpeakerClient(cliRunner(CONFIG.SPEAKER_BIN), CONFIG.RELAY || undefined)
  const agent = new Agent24Client(daemon)

  // ── register (default): publish business capabilities so peers can find us ──
  if (fs.existsSync(CONFIG.PROFILE_FILE)) {
    try {
      const profile = loadAgent24Profile(fs.readFileSync(CONFIG.PROFILE_FILE, 'utf8'))
      const { result } = await new NostrBridge(speaker, CONFIG.IDENTITY).register(
        CONFIG.AGENT_NAME,
        profile,
      )
      console.log(`[nostr] ✅ 已注册能力,发布到 ${result.published_to ?? '?'} 个 relay`)
    } catch (err) {
      console.error('[nostr] 注册失败(继续起入站):', err instanceof Error ? err.message : err)
    }
  } else {
    console.warn(`[nostr] 未找到 ${CONFIG.PROFILE_FILE},跳过默认注册(可从 agent-profile.example.yml 复制一份)。`)
  }

  // ── inbound: authorized peer messages → gated runs ──
  if (CONFIG.ALLOWED_NPUBS.size === 0) {
    console.warn(
      '[nostr] ⚠️  未配置 A24_NOSTR_ALLOWED_NPUBS:将拒绝所有入站消息(fail-closed)。把授权对端的 npub 设进环境变量。',
    )
  } else {
    console.log(`[nostr] 已授权 ${CONFIG.ALLOWED_NPUBS.size} 个对端 agent`)
  }
  const inbound = new InboundBridge(agent, speaker, CONFIG.IDENTITY, CONFIG.ALLOWED_NPUBS)

  let stopped = false
  const tick = async (): Promise<void> => {
    if (stopped) return
    try {
      await pollOnce(speaker, inbound)
    } catch (err) {
      console.error('[nostr] 轮询入站出错:', err instanceof Error ? err.message : err)
    }
    if (!stopped) setTimeout(() => void tick(), CONFIG.POLL_INTERVAL_MS)
  }

  const shutdown = (): void => {
    console.log('\n[nostr] 停止中...')
    stopped = true
    process.exit(0)
  }
  process.on('SIGINT', shutdown)
  process.on('SIGTERM', shutdown)

  console.log(`[nostr] ✅ 桥已启动,每 ${CONFIG.POLL_INTERVAL_MS}ms 轮询入站。`)
  void tick()
}

void main().catch((err) => {
  console.error('[nostr] 启动失败:', err instanceof Error ? err.message : err)
  process.exit(1)
})
