// Thin wrapper over the agent-speaker CLI (G3: drive the binary, don't
// reimplement Nostr). Every call shells out to `agent-speaker <cmd> ... --json`
// and parses stdout. The runner is injectable so tests drive a hermetic fake
// instead of a real subprocess (see testing/fake-speaker.ts).

import { execFile } from 'node:child_process'
import type { AgentSpeakerProfile } from './profile.js'

/** Runs one agent-speaker invocation and resolves its stdout. */
export type SpeakerRunner = (args: string[]) => Promise<string>

/** The real runner: exec the `agent-speaker` binary. */
export function cliRunner(bin = 'agent-speaker'): SpeakerRunner {
  return (args) =>
    new Promise<string>((resolve, reject) => {
      execFile(bin, args, { maxBuffer: 8 * 1024 * 1024 }, (err, stdout, stderr) => {
        if (err) reject(new Error(`agent-speaker ${args[0] ?? ''} failed: ${stderr.trim() || err.message}`))
        else resolve(stdout)
      })
    })
}

/** `agent msg --json` result (agent-speaker internal/messaging/agent.go). */
export interface SendResult {
  event_id?: string
  published_to?: number
  relay_count?: number
  relays?: { url: string; ok: boolean; error?: string }[]
  queued_for_retry?: boolean
}

/** `profile publish --json` result (agent-speaker PR #29). */
export interface PublishResult {
  name?: string
  published_to?: number
  relay_count?: number
  relays?: { url: string; ok: boolean; error?: string }[]
}

/** One `profile discover --json` entry. */
export interface DiscoverEntry {
  npub: string
  profile: AgentSpeakerProfile
}

/** A decrypted inbound message from `agent inbox --json`. `content` is the raw
 * decompressed/decrypted string the sender put on the wire (an F4 envelope JSON
 * when it came from another agent24). */
export interface InboundMessage {
  /** Sender npub — the allowlist / session key. */
  from: string
  content: string
  event_id?: string
  created_at?: number
}

export class SpeakerClient {
  constructor(
    private readonly run: SpeakerRunner,
    /** Relay override; falls back to agent-speaker's default when unset. */
    private readonly relay?: string,
  ) {}

  private relayArgs(): string[] {
    return this.relay ? ['--relay', this.relay] : []
  }

  /** register → `profile publish --json-file` reads the profile, `--json`
   * (agent-speaker PR #29) returns structured per-relay results so we know how
   * many relays it actually reached. */
  async publishProfile(identity: string, jsonFile: string): Promise<PublishResult> {
    const out = await this.run([
      'profile',
      'publish',
      '--from',
      identity,
      '--json-file',
      jsonFile,
      '--json',
      ...this.relayArgs(),
    ])
    return parseJson<PublishResult>(out, 'profile publish')
  }

  /** say / answer → `agent msg` (directed, NIP-44 encrypted by default). */
  async sendMessage(from: string, to: string, content: string, encrypt = true): Promise<SendResult> {
    const out = await this.run([
      'agent',
      'msg',
      '--from',
      from,
      '--to',
      to,
      '--content',
      content,
      `--encrypt=${encrypt}`,
      '--json',
      ...this.relayArgs(),
    ])
    return parseJson<SendResult>(out, 'agent msg')
  }

  /** search → `profile discover --capability` (returns [{npub, profile}]). */
  async discover(capability: string): Promise<DiscoverEntry[]> {
    const out = await this.run([
      'profile',
      'discover',
      '--capability',
      capability,
      '--json',
      ...this.relayArgs(),
    ])
    const parsed = parseJson<DiscoverEntry[]>(out, 'profile discover')
    return Array.isArray(parsed) ? parsed : []
  }

  /** listen/subscribe (inbound) → `agent inbox --json`. The daemon pulls inbound
   * to the local store; this reads it. Maps agent-speaker's `StoredMessage`
   * (PR #29: `sender_npub` / `plaintext` / `id` / `is_incoming`), tolerating a
   * couple of older field names so a version skew during 联调 doesn't break it.
   * Only INCOMING messages surface (our own outbound is in the same store). */
  async inbox(): Promise<InboundMessage[]> {
    const out = await this.run(['agent', 'inbox', '--json', ...this.relayArgs()])
    const parsed = parseJson<unknown>(out, 'agent inbox')
    const rows = Array.isArray(parsed)
      ? parsed
      : ((parsed as { messages?: unknown[] })?.messages ?? [])
    return (rows as Record<string, unknown>[])
      .filter((r) => r.is_incoming !== false) // absent (older build) → treat as inbound
      .map((r) => ({
        from: String(r.sender_npub ?? r.from ?? r.from_npub ?? r.sender ?? ''),
        // prefer decrypted plaintext; fall back to raw content
        content: String(r.plaintext ?? r.content ?? r.text ?? ''),
        event_id:
          r.id != null ? String(r.id) : r.event_id != null ? String(r.event_id) : undefined,
        created_at: typeof r.created_at === 'number' ? r.created_at : undefined,
      }))
  }
}

function parseJson<T>(raw: string, label: string): T {
  try {
    return JSON.parse(raw) as T
  } catch {
    throw new Error(`agent-speaker ${label}: expected JSON, got: ${raw.slice(0, 200)}`)
  }
}
