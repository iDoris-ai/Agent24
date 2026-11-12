// F3 WeChat bridge configuration.
//
// Two connections: OUT to WeChat via the official iLink Bot API, and IN to the
// local agent24d over its v1 HTTP API. The iLink constants are ground-truth
// values (ported from heinu1 / weixin-bot-ilink); the agent24d endpoint is
// discovered from the daemon's state file, overridable by env.
//
// The bridge does not keep an allowlist. Who may start a run is the daemon's
// `inbound_allowlist`, seeded in development from `A24_INBOUND_DEV_ALLOWLIST`.

import { DaemonStateError, dataDir, joinLiteral, requireDaemonBearer } from '@agent24/data-dir'

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
}

// iLink `base_info.channel_version`, from the reference implementation.
export const BASE_INFO = { channel_version: '1.0.0' }

export interface DaemonEndpoint {
  base: string
  token: string
}

/** Discover the running agent24d: env first (A24_BASE_URL / A24_TOKEN), else the
 * daemon's state file via the one shared parser (`@agent24/data-dir`).
 * Returns null when no daemon can be located (no state file).
 * Throws DaemonStateError when a state file exists but cannot give this bridge
 * a bearer (capabilities mode, missing/empty token, invalid or unknown-mode
 * record): the bridge must fail loudly, never send an empty token. */
export function discoverDaemon(): DaemonEndpoint | null {
  const envBase = process.env.A24_BASE_URL
  if (envBase) {
    // The override must be a complete credential pair: a valid http(s) URL AND a
    // non-empty A24_TOKEN. Anything less would run unauthenticated business requests.
    let url: URL
    try {
      url = new URL(envBase)
    } catch {
      throw new DaemonStateError('A24_BASE_URL is not a valid URL')
    }
    if (url.protocol !== 'http:' && url.protocol !== 'https:') throw new DaemonStateError('A24_BASE_URL must be http(s)')
    const token = process.env.A24_TOKEN
    if (!token || token.trim() === '') throw new DaemonStateError('A24_BASE_URL is set but A24_TOKEN is missing or empty')
    return { base: envBase.replace(/\/+$/, ''), token }
  }
  const found = requireDaemonBearer(CONFIG.DAEMON_STATE_FILE)
  return found && { base: `http://127.0.0.1:${found.port}`, token: found.token }
}
