// F3 WeChat bridge configuration.
//
// Two connections: OUT to WeChat via the official iLink Bot API, and IN to the
// local agent24d over its v1 HTTP API. The iLink constants are ground-truth
// values (ported from heinu1 / weixin-bot-ilink); the agent24d endpoint is
// discovered from the daemon's state file, overridable by env.
//
// The bridge does not keep an allowlist. Who may start a run is the daemon's
// `inbound_allowlist`. The bearer is the dev-bridge file under `dataDir()`,
// not a field of `daemon.json`.

import fs from 'node:fs'

import { DaemonStateError, dataDir, joinLiteral, readDaemonState } from '@agent24/data-dir'

export const CONFIG = {
  // ── WeChat iLink (official bot API) ──────────────────────────────────────
  // Domain only — the `/ilink/bot/*` prefix is in each endpoint path.
  ILINK_BASE: process.env.A24_ILINK_BASE || 'https://ilinkai.weixin.qq.com',
  // Where the bot_token from the QR login is persisted, so you scan only once.
  // Read on each access so AGENT24_DATA_DIR / HOME are the process's current values.
  get TOKEN_FILE(): string {
    return joinLiteral(dataDir(), 'wechat-token.json')
  },
  POLL_TIMEOUT_MS: 40_000,
  RECONNECT_DELAY_MS: 3_000,
  MAX_MSG_LEN: 1800,

  // ── agent24d (the local daemon this bridge posts events to) ──────────────
  // The daemon writes its dynamic port + token to this discovery file (Rust
  // side). Env overrides win for tests / non-standard setups.
  get DAEMON_STATE_FILE(): string {
    return joinLiteral(dataDir(), 'daemon.json')
  },
  // Development ingest. The route fixes source to wechat.
  INBOUND_EVENTS_PATH: '/api/v1/inbound/events',
  // Dev-bridge credential. Same data root as the daemon (`dataDir()`), under `dev/`.
  get BRIDGE_TOKEN_FILE(): string {
    return joinLiteral(dataDir(), 'dev', 'wechat-bridge.token')
  },
}

export const BRIDGE_TOKEN_HINT = '请运行 `agent24 dev bridge-token`'

// iLink `base_info.channel_version`, from the reference implementation.
export const BASE_INFO = { channel_version: '1.0.0' }

export interface DaemonEndpoint {
  base: string
  token: string
}

export interface BridgeCredential {
  token: string
  expiresAt: number
}

/** Read `dev/wechat-bridge.token`. Missing, not mode 0600, or expired: refuse.
 * This does not open `daemon.json`. */
export function loadBridgeCredential(nowMs = Date.now()): BridgeCredential {
  const path = CONFIG.BRIDGE_TOKEN_FILE
  let st: fs.Stats
  try {
    st = fs.lstatSync(path)
  } catch (err) {
    const code = (err as NodeJS.ErrnoException).code
    if (code === 'ENOENT') throw new Error(`没有微信桥令牌文件 ${path}。${BRIDGE_TOKEN_HINT}`)
    throw new Error(`无法读取微信桥令牌文件 ${path}。${BRIDGE_TOKEN_HINT}`)
  }
  if (!st.isFile() || (st.mode & 0o777) !== 0o600) {
    throw new Error(`微信桥令牌文件权限必须是 0600（${path}）。${BRIDGE_TOKEN_HINT}`)
  }
  let parsed: { token?: unknown; expires_at?: unknown }
  try {
    parsed = JSON.parse(fs.readFileSync(path, 'utf8')) as { token?: unknown; expires_at?: unknown }
  } catch {
    throw new Error(`微信桥令牌文件无法解析（${path}）。${BRIDGE_TOKEN_HINT}`)
  }
  if (typeof parsed.token !== 'string' || parsed.token.length === 0) {
    throw new Error(`微信桥令牌文件没有令牌（${path}）。${BRIDGE_TOKEN_HINT}`)
  }
  if (typeof parsed.expires_at !== 'number' || !Number.isInteger(parsed.expires_at)) {
    throw new Error(`微信桥令牌文件没有过期时间（${path}）。${BRIDGE_TOKEN_HINT}`)
  }
  if (parsed.expires_at * 1000 <= nowMs) {
    throw new Error(`微信桥令牌已过期（${path}）。${BRIDGE_TOKEN_HINT}`)
  }
  return { token: parsed.token, expiresAt: parsed.expires_at }
}

function daemonBase(): string {
  const envBase = process.env.A24_BASE_URL
  if (envBase) {
    let url: URL
    try {
      url = new URL(envBase)
    } catch {
      throw new DaemonStateError('A24_BASE_URL is not a valid URL')
    }
    if (url.protocol !== 'http:' && url.protocol !== 'https:') throw new DaemonStateError('A24_BASE_URL must be http(s)')
    return envBase.replace(/\/+$/, '')
  }
  const found = readDaemonState(CONFIG.DAEMON_STATE_FILE)
  if (!found.ok) {
    if (found.reason === 'unreadable') {
      throw new Error('找不到运行中的 agent24d。先启动它（agent24 daemon start）。')
    }
    throw new DaemonStateError(found.message)
  }
  return `http://127.0.0.1:${found.state.port}`
}

/** The bridge bearer comes only from the token file. The state file, when
 * read, supplies the port. A bad token file returns before that read. */
export function discoverDaemon(nowMs = Date.now()): DaemonEndpoint {
  const cred = loadBridgeCredential(nowMs)
  return { base: daemonBase(), token: cred.token }
}
