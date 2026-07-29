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

/** One `profile discover --json` entry. */
export interface DiscoverEntry {
  npub: string
  profile: AgentSpeakerProfile
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

  /** register → `profile publish --json-file`. This command has no `--json`
   * yet (CC-82 gap), so success is the non-zero-exit contract of the runner. */
  async publishProfile(identity: string, jsonFile: string): Promise<void> {
    await this.run([
      'profile',
      'publish',
      '--from',
      identity,
      '--json-file',
      jsonFile,
      ...this.relayArgs(),
    ])
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
}

function parseJson<T>(raw: string, label: string): T {
  try {
    return JSON.parse(raw) as T
  } catch {
    throw new Error(`agent-speaker ${label}: expected JSON, got: ${raw.slice(0, 200)}`)
  }
}
