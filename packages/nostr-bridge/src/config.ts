// F4 Nostr bridge configuration. Two connections: OUT to the network via the
// agent-speaker CLI (a subprocess — G3), and IN to the local agent24d over its
// v1 HTTP API (discovered from the daemon's state file, overridable by env).

import path from 'node:path'
import os from 'node:os'
import fs from 'node:fs'

const HOME = os.homedir()

/** Parse a comma/space-separated allowlist of sender npubs into a Set. */
export function parseNpubs(raw: string | undefined): Set<string> {
  return new Set(
    (raw ?? '')
      .split(/[,\s]+/)
      .map((s) => s.trim())
      .filter(Boolean),
  )
}

export const CONFIG = {
  /** The agent-speaker binary (build result). Override for a non-PATH install. */
  SPEAKER_BIN: process.env.A24_SPEAKER_BIN || 'agent-speaker',
  /** agent-speaker identity nickname this bridge acts as. */
  IDENTITY: process.env.A24_NOSTR_IDENTITY || 'agent24',
  /** Relay override; empty = agent-speaker's own default (wss://relay.aastar.io). */
  RELAY: process.env.A24_NOSTR_RELAY || '',
  /** Display name published in the profile. */
  AGENT_NAME: process.env.A24_NOSTR_NAME || 'agent24',
  /** Editable capability source (§5). */
  PROFILE_FILE:
    process.env.A24_NOSTR_PROFILE || path.join(HOME, '.agent24', 'agent-profile.yml'),
  /** Inbound authorization — fail-closed (SECURITY, same as F3). */
  ALLOWED_NPUBS: parseNpubs(process.env.A24_NOSTR_ALLOWED_NPUBS),
  /** How often to poll the inbox for new peer messages. */
  POLL_INTERVAL_MS: Number(process.env.A24_NOSTR_POLL_MS) || 5_000,

  // ── agent24d discovery (Rust writes port+token here; env overrides win) ──
  DAEMON_STATE_FILE: path.join(HOME, '.agent24', 'daemon.json'),
}

export interface DaemonEndpoint {
  base: string
  token: string
}

/** Discover the running agent24d: env first (A24_BASE_URL / A24_TOKEN), else the
 * daemon's state file. Returns null when no daemon can be located. */
export function discoverDaemon(): DaemonEndpoint | null {
  const envBase = process.env.A24_BASE_URL
  if (envBase) return { base: envBase.replace(/\/+$/, ''), token: process.env.A24_TOKEN ?? '' }
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
