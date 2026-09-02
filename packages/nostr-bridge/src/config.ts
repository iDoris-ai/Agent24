// F4 Nostr bridge configuration. Two connections: OUT to the network via the
// agent-speaker CLI (a subprocess — G3), and IN to the local agent24d over its
// v1 HTTP API (discovered from the daemon's state file, overridable by env).

import path from 'node:path'
import os from 'node:os'
import fs from 'node:fs'

const HOME = os.homedir()

/** Parse a duration env var into a finite positive number of milliseconds.
 *
 * `Number(env) || fallback` is not enough for anything that feeds a timer:
 * `A24_..._MS=-1` survives it, and `setTimeout(-1)` fires immediately — a
 * negative tick would spin the probe into a tight loop spawning `history inbox`
 * subprocesses as fast as the machine allows. `Infinity` and `1e30` are just as
 * bad from the other side (Node clamps an out-of-range delay to 1ms). So: reject
 * anything not finite, and clamp to a sane floor. */
export function durationMs(raw: string | undefined, fallback: number, min: number): number {
  const n = Number(raw)
  if (!raw || !Number.isFinite(n) || n <= 0) return fallback
  // 24 days is Node's signed-32-bit timer ceiling; past it a delay silently
  // becomes 1ms, which is the tight loop again.
  return Math.min(Math.max(n, min), 2 ** 31 - 1)
}

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
  POLL_INTERVAL_MS: durationMs(process.env.A24_NOSTR_POLL_MS, 5_000, 250),
  /** Hard wall-clock cap on one `agent-speaker` invocation.
   *
   * WITHOUT this the whole bridge can stop for good: `tick()` in `main.ts` is a
   * SEQUENTIAL self-rescheduling loop — it only schedules the next poll after the
   * current one settles. An `execFile` with no timeout that never calls back
   * (hung relay socket after the machine wakes, DNS stall, a wedged child) leaves
   * that promise pending forever, so no further tick is ever scheduled. The
   * process stays alive and healthy-looking, so launchd's KeepAlive never fires
   * and nothing is logged. Inbound simply stops.
   *
   * 60s is well above a normal relay round-trip and below any human's patience
   * for "why hasn't it answered". */
  SPEAKER_TIMEOUT_MS: durationMs(process.env.A24_NOSTR_SPEAKER_TIMEOUT_MS, 60_000, 1_000),
  /** Rows per inbox read. Wide enough that nothing falls out of the window
   * between two polls (agent-speaker's own default is 20). */
  INBOX_LIMIT: Math.round(durationMs(process.env.A24_NOSTR_INBOX_LIMIT, 100, 1)),

  // ── FU-32 inbound liveness (see liveness.ts for why a canary, not a timeout) ──
  /** Set to `0` to disable the probe entirely. Disabling means the bridge can no
   * longer tell an empty inbox from a dead relay path — the F5 soak must not. */
  LIVENESS_ENABLED: process.env.A24_NOSTR_LIVENESS !== '0',
  /** How often the bridge sends itself a canary. 5 min ≈ 288 events/day. */
  CANARY_INTERVAL_MS: durationMs(process.env.A24_NOSTR_CANARY_MS, 5 * 60_000, 10_000),
  /** No canary back for this long ⇒ inbound is presumed dead. Three missed
   * canaries: long enough to ride out one relay hiccup, short enough that a
   * machine that woke up broken is caught within the quarter hour. */
  LIVENESS_STALE_MS: durationMs(process.env.A24_NOSTR_STALE_MS, 15 * 60_000, 30_000),
  /** How often the probe wakes up. It runs on its OWN timer, not on the poll
   * loop — that loop awaits each inbound message's agent run, so a probe riding
   * it would be starved by a slow run for as long as the run takes. */
  LIVENESS_TICK_MS: durationMs(process.env.A24_NOSTR_LIVENESS_TICK_MS, 30_000, 1_000),
  /** NIP-44-encrypt canaries (set `0` for plaintext). */
  CANARY_ENCRYPT: process.env.A24_NOSTR_CANARY_ENCRYPT !== '0',
  /** Health snapshot an operator (or the F5 soak) can `cat`. Empty disables.
   *
   * Scoped by identity: two bridges acting as different identities would
   * otherwise take turns overwriting one file, interleaving their silence,
   * counters and generation into a single meaningless ledger — and a monitor
   * sampling it would see whichever instance wrote last, so one bridge's outage
   * could be masked by the other's health. */
  HEALTH_FILE:
    process.env.A24_NOSTR_HEALTH_FILE ??
    path.join(
      HOME,
      '.agent24',
      `nostr-bridge-health-${(process.env.A24_NOSTR_IDENTITY || 'agent24').replace(/[^A-Za-z0-9_.-]/g, '_')}.json`,
    ),

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
