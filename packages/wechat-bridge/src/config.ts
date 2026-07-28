// F3 WeChat bridge configuration.
//
// Two connections: OUT to WeChat via the official iLink Bot API, and IN to the
// local agent24d over its v1 HTTP API. The iLink constants are ground-truth
// values (ported from heinu1 / weixin-bot-ilink); the agent24d endpoint is
// discovered from the daemon's state file, overridable by env.

import path from 'node:path'
import os from 'node:os'
import fs from 'node:fs'

const HOME = os.homedir()

export const CONFIG = {
  // ── WeChat iLink (official bot API) ──────────────────────────────────────
  // Domain only — the `/ilink/bot/*` prefix is in each endpoint path.
  ILINK_BASE: process.env.A24_ILINK_BASE || 'https://ilinkai.weixin.qq.com',
  // Where the bot_token from the QR login is persisted, so you scan only once.
  TOKEN_FILE: path.join(HOME, '.agent24', 'wechat-token.json'),
  POLL_TIMEOUT_MS: 40_000,
  RECONNECT_DELAY_MS: 3_000,
  MAX_MSG_LEN: 1800,

  // ── Authorization (SECURITY, fail-closed) ────────────────────────────────
  // Every inbound message becomes an owner-level agent24d run with tool /
  // filesystem access, so only explicitly listed WeChat `from_user_id`s may
  // drive the bridge. With none configured, nobody is authorized (the bridge
  // drops every message). Set A24_WECHAT_ALLOWED_UIDS to a comma/space-separated
  // list of ids (an unauthorized sender's full id is logged so you can add it).
  ALLOWED_UIDS: parseAllowedUids(process.env.A24_WECHAT_ALLOWED_UIDS),

  // ── agent24d (the local daemon this bridge drives) ───────────────────────
  // The daemon writes its dynamic port + token to this discovery file (Rust
  // side). Env overrides win for tests / non-standard setups.
  DAEMON_STATE_FILE: path.join(HOME, '.agent24', 'daemon.json'),
  // How long to wait for a run to finish before telling the user it is still
  // going (the run keeps running on the daemon).
  RUN_WAIT_TIMEOUT_MS: 600_000,
  RUN_POLL_INTERVAL_MS: 1_500,
}

/** Parse a comma/space-separated allowlist of WeChat user ids into a Set. */
export function parseAllowedUids(raw: string | undefined): Set<string> {
  return new Set(
    (raw ?? '')
      .split(/[,\s]+/)
      .map((s) => s.trim())
      .filter(Boolean),
  )
}

// iLink `base_info.channel_version`, from the reference implementation.
export const BASE_INFO = { channel_version: '1.0.0' }

export interface DaemonEndpoint {
  base: string
  token: string
}

/** Discover the running agent24d: env first (A24_BASE_URL / A24_TOKEN), else the
 * daemon's state file. Returns null when no daemon can be located. */
export function discoverDaemon(): DaemonEndpoint | null {
  const envBase = process.env.A24_BASE_URL
  const envToken = process.env.A24_TOKEN
  if (envBase) return { base: envBase.replace(/\/+$/, ''), token: envToken ?? '' }

  try {
    const raw = JSON.parse(fs.readFileSync(CONFIG.DAEMON_STATE_FILE, 'utf8')) as {
      port?: number
      token?: string
    }
    if (typeof raw.port === 'number') {
      return { base: `http://127.0.0.1:${raw.port}`, token: raw.token ?? '' }
    }
  } catch {
    /* no state file yet */
  }
  return null
}
